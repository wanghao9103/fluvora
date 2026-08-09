use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{CONTENT_TYPE, ETAG, IF_MATCH, LOCATION};
use axum::http::{HeaderValue, StatusCode};
use axum::routing::{get, patch, post, put};
use axum::{Router, middleware};
use fluvora_auth::TokenKeyRing;
use fluvora_control_store::{PostgresStore, StoredSignal};
use fluvora_event_dispatcher::EventEnvelope;
use fluvora_observability::MediaNodeMetrics;
use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};
use fluvora_transcode_bridge::{Coordinator as TranscodeCoordinator, Quotas as TranscodeQuotas};
use futures_util::StreamExt as _;
use tokio::sync::Mutex as AsyncMutex;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::config::ApiConfig;
use crate::control_client::build_internal_http_client;
use crate::error::{ApiError, internal_error, lock_error};
use crate::gateway_routes::{
    complete_asset, create_asset, create_live_stream, delete_asset, delete_live_stream,
    finish_live_stream, get_asset, get_live_stream, upload_asset_chunk, upload_live_init,
    upload_live_segment,
};
use crate::models::{AppState, TranscodeRegistry};
use crate::persistence::{LoadedRooms, RoomPersistence, load_postgres_rooms, load_rooms};
use crate::routes::{
    answer_offer, create_room, create_whep_session, create_whip_session, delete_whep_session,
    delete_whip_session, end_room, get_ice_servers, get_room, get_signals, issue_event_ticket,
    join_room, leave_room, patch_whep_session, patch_whip_session, post_signal, record_gift,
    register_track, revoke_token, room_events, send_chat, send_custom_data, set_role,
    set_subscription_layer, start_publishing, stop_publishing, subscribe_track, unpublish_track,
    unsubscribe_track,
};
use crate::runtime::{MAX_PROTOCOL_SESSIONS, shutdown_signal};
use crate::services::{refresh_postgres_room, reject_revoked_token};
use crate::signals::{MAX_JSON_REQUEST_BYTES, cache_signal, signal_record};
use crate::transcode_reconciler::run_transcode_reconciler;
use crate::validation::parse_room_id;

pub(crate) async fn run() {
    let bind = env::var("FLUVORA_API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let address: SocketAddr = bind.parse().expect("FLUVORA_API_BIND must be host:port");
    let state = initialize_state().await;
    let event_subscriber = start_event_subscriber(state.clone()).await;
    let app = build_router(state.clone());
    let (heartbeat, heartbeat_task) = start_api_heartbeat(state.clone());
    let transcode_reconciler = tokio::spawn(run_transcode_reconciler(state.clone()));
    let revocation_gc = start_revocation_gc(&state);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("API listener bind");
    println!(
        "{} API server listening on {address} (signaling v{})",
        fluvora_domain::PLATFORM_NAME,
        fluvora_protocol::SIGNALING_VERSION
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("API server");
    stop_api_heartbeat(heartbeat.as_ref(), heartbeat_task, &state).await;
    transcode_reconciler.abort();
    if let Some(task) = revocation_gc {
        task.abort();
    }
    if let Some(task) = event_subscriber {
        task.abort();
    }
}

fn start_revocation_gc(state: &AppState) -> Option<tokio::task::JoinHandle<()>> {
    let RoomPersistence::Postgres(store) = state.persistence.as_ref() else {
        return None;
    };
    let store = store.clone();
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_hours(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match store.purge_expired_token_revocations(10_000).await {
                Ok(deleted) if deleted > 0 => {
                    println!("purged {deleted} expired access-token revocations");
                }
                Ok(_) => {}
                Err(error) => eprintln!("access-token revocation cleanup failed: {error}"),
            }
        }
    }))
}

async fn start_event_subscriber(state: AppState) -> Option<tokio::task::JoinHandle<()>> {
    let Ok(nats_url) = env::var("FLUVORA_NATS_URL") else {
        return None;
    };
    let nats_token = env::var("FLUVORA_NATS_TOKEN")
        .expect("FLUVORA_NATS_TOKEN is required when FLUVORA_NATS_URL is configured");
    assert!(
        (24..=4_096).contains(&nats_token.len())
            && !nats_token.bytes().any(|byte| byte.is_ascii_control()),
        "FLUVORA_NATS_TOKEN must contain 24..=4096 non-control bytes"
    );
    let subject_root =
        env::var("FLUVORA_EVENT_SUBJECT").unwrap_or_else(|_| "fluvora.events".to_owned());
    state.event_bus_ready.store(false, Ordering::Release);
    let client = async_nats::ConnectOptions::with_token(nats_token)
        .name(format!("api-events-{}", std::process::id()))
        .connect(nats_url)
        .await
        .expect("connect FLUVORA_NATS_URL");
    let mut subscriber = client
        .subscribe(format!("{}.>", subject_root.trim_end_matches('.')))
        .await
        .expect("subscribe to Fluvora events");
    state.event_bus_ready.store(true, Ordering::Release);
    Some(tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            let envelope = match serde_json::from_slice::<EventEnvelope>(&message.payload) {
                Ok(envelope) => envelope,
                Err(error) => {
                    eprintln!("ignoring invalid shared event envelope: {error}");
                    continue;
                }
            };
            if let Err(error) = apply_shared_event(&state, envelope).await {
                eprintln!("shared event application failed: {}", error.message);
            }
        }
        state.event_bus_ready.store(false, Ordering::Release);
    }))
}

async fn apply_shared_event(state: &AppState, envelope: EventEnvelope) -> Result<(), ApiError> {
    if envelope.schema_version != fluvora_event_dispatcher::EVENT_SCHEMA_VERSION {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "unsupported_event_schema",
            message: format!(
                "unsupported shared event schema {}",
                envelope.schema_version
            ),
        });
    }
    match envelope.aggregate_type.as_str() {
        "room" => {
            let room_id = parse_room_id(&envelope.aggregate_id)?;
            let was_active = state
                .rooms
                .read()
                .map_err(lock_error)?
                .get(&room_id)
                .is_some_and(|managed| !managed.room.is_ended());
            refresh_postgres_room(state, room_id).await?;
            let is_active = state
                .rooms
                .read()
                .map_err(lock_error)?
                .get(&room_id)
                .is_some_and(|managed| !managed.room.is_ended());
            match (was_active, is_active) {
                (false, true) => state.metrics.active_rooms.add(1),
                (true, false) => state.metrics.active_rooms.add(-1),
                _ => {}
            }
            Ok(())
        }
        "room_signal" if envelope.event_type == "signal.created" => {
            let signal: StoredSignal =
                serde_json::from_value(envelope.payload).map_err(internal_error)?;
            let room_id = parse_room_id(&signal.room_id)?;
            if !state
                .rooms
                .read()
                .map_err(lock_error)?
                .contains_key(&room_id)
            {
                refresh_postgres_room(state, room_id).await?;
            }
            cache_signal(state, room_id, signal_record(room_id, signal)?)
        }
        _ => Ok(()),
    }
}

fn start_api_heartbeat(
    state: AppState,
) -> (Option<HeartbeatClient>, Option<tokio::task::JoinHandle<()>>) {
    let client =
        HeartbeatClient::from_env(ServiceKind::Api).expect("valid status heartbeat configuration");
    let task = client.as_ref().map(|client| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .run(|| std::future::ready(api_capacity(&state)))
                .await;
        })
    });
    (client, task)
}

async fn stop_api_heartbeat(
    client: Option<&HeartbeatClient>,
    task: Option<tokio::task::JoinHandle<()>>,
    state: &AppState,
) {
    if let Some(client) = client {
        client.mark_draining();
        if let Err(error) = client.report(api_capacity(state), true).await {
            eprintln!("failed to report draining API heartbeat: {error}");
        }
    }
    if let Some(task) = task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            eprintln!("API heartbeat task failed during shutdown: {error}");
        }
    }
}

fn api_capacity(state: &AppState) -> NodeCapacity {
    let rooms = state
        .rooms
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .filter(|room| !room.room.is_ended())
        .count();
    let sessions = state
        .protocol_sessions
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    let tracks = state
        .tracks
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    NodeCapacity {
        rooms_limit: u64::try_from(MAX_PROTOCOL_SESSIONS).unwrap_or(u64::MAX),
        rooms_used: u64::try_from(rooms).unwrap_or(u64::MAX),
        sessions_limit: u64::try_from(MAX_PROTOCOL_SESSIONS).unwrap_or(u64::MAX),
        sessions_used: u64::try_from(sessions).unwrap_or(u64::MAX),
        publisher_tracks: u64::try_from(tracks).unwrap_or(u64::MAX),
        memory_bytes: process_memory_bytes(),
        ..NodeCapacity::default()
    }
}

async fn initialize_state() -> AppState {
    let config = ApiConfig::from_env();
    let (persistence, loaded) = initialize_persistence().await;
    let active_rooms = loaded
        .rooms
        .values()
        .filter(|managed| !managed.room.is_ended())
        .count();
    let metrics_registry = Arc::new(MediaNodeMetrics::default());
    metrics_registry
        .active_rooms
        .add(i64::try_from(active_rooms).unwrap_or(i64::MAX));
    AppState {
        rooms: Arc::new(RwLock::new(loaded.rooms)),
        room_creations: Arc::new(RwLock::new(loaded.room_creations)),
        event_channels: Arc::new(RwLock::new(loaded.event_channels)),
        event_tickets: Arc::new(RwLock::new(HashMap::new())),
        protocol_sessions: Arc::new(RwLock::new(HashMap::new())),
        protocol_updates: Arc::new(AsyncMutex::new(())),
        tracks: Arc::new(RwLock::new(HashMap::new())),
        subscriptions: Arc::new(RwLock::new(HashMap::new())),
        transcodes: Arc::new(AsyncMutex::new(TranscodeRegistry {
            coordinator: TranscodeCoordinator::new(TranscodeQuotas {
                global_jobs: config.transcode_global_jobs,
                jobs_per_tenant: config.transcode_tenant_jobs,
            }),
            active: HashMap::new(),
            subscriptions: HashMap::new(),
            health_failures: HashMap::new(),
        })),
        persistence: Arc::new(persistence),
        room_mutations: Arc::new(AsyncMutex::new(())),
        region: Arc::from(config.region),
        placement_stale_after: config.placement_stale_after,
        tokens: Arc::new(TokenKeyRing::new(config.token_keys).expect("strong token key ring")),
        metrics: metrics_registry,
        dtls_fingerprint: Arc::from(config.dtls_fingerprint),
        candidate: config.candidate.map(Arc::from),
        media_control_url: Arc::from(config.media_control_url),
        media_control_token: Arc::from(config.media_control_token),
        gateway_control_url: Arc::from(config.gateway_control_url),
        gateway_control_token: Arc::from(config.gateway_control_token),
        worker_control_url: Arc::from(config.worker_control_url),
        worker_control_token: Arc::from(config.worker_control_token),
        ice_urls: Arc::new(config.ice_urls),
        turn_rest_secret: Arc::from(config.turn_rest_secret),
        http_client: build_internal_http_client(),
        event_bus_ready: Arc::new(AtomicBool::new(true)),
        revoked_tokens: Arc::new(RwLock::new(HashMap::new())),
        gift_webhook_secret: Arc::from(config.gift_webhook_secret),
    }
}

async fn initialize_persistence() -> (RoomPersistence, LoadedRooms) {
    if let Ok(database_url) = env::var("FLUVORA_DATABASE_URL") {
        let maximum_connections = env::var("FLUVORA_DATABASE_MAX_CONNECTIONS")
            .map_or(Ok(16), |value| value.parse::<u32>())
            .expect("FLUVORA_DATABASE_MAX_CONNECTIONS must be an integer");
        let store = PostgresStore::connect(&database_url, maximum_connections)
            .await
            .expect("connect FLUVORA_DATABASE_URL");
        store.migrate().await.expect("apply PostgreSQL migrations");
        let rooms = store
            .load_rooms()
            .await
            .expect("load PostgreSQL room state");
        let loaded = load_postgres_rooms(rooms).expect("restore PostgreSQL room state");
        (RoomPersistence::Postgres(store), loaded)
    } else {
        let state_directory = PathBuf::from(
            env::var("FLUVORA_STATE_DIR").unwrap_or_else(|_| "./data/control".to_owned()),
        );
        std::fs::create_dir_all(&state_directory).expect("create FLUVORA_STATE_DIR");
        let loaded = load_rooms(&state_directory).expect("load persisted room state");
        (RoomPersistence::Files(Arc::new(state_directory)), loaded)
    }
}

fn build_router(state: AppState) -> Router {
    let router = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/v1/rooms", post(create_room))
        .route("/v1/rooms/{room_id}", get(get_room))
        .route("/v1/rooms/{room_id}/join", post(join_room))
        .route("/v1/rooms/{room_id}/leave", post(leave_room))
        .route("/v1/rooms/{room_id}/end", post(end_room))
        .route("/v1/rooms/{room_id}/roles", post(set_role))
        .route("/v1/rooms/{room_id}/gifts", post(record_gift))
        .route("/v1/auth/revocations", post(revoke_token))
        .route("/v1/rooms/{room_id}/chat", post(send_chat))
        .route("/v1/rooms/{room_id}/custom", post(send_custom_data))
        .route("/v1/rooms/{room_id}/publish/start", post(start_publishing))
        .route("/v1/rooms/{room_id}/publish/stop", post(stop_publishing))
        .route("/v1/rooms/{room_id}/tracks", post(register_track))
        .route(
            "/v1/rooms/{room_id}/tracks/{track_id}",
            axum::routing::delete(unpublish_track),
        )
        .route("/v1/rooms/{room_id}/subscriptions", post(subscribe_track))
        .route(
            "/v1/rooms/{room_id}/subscriptions/{subscription_id}",
            axum::routing::delete(unsubscribe_track),
        )
        .route(
            "/v1/rooms/{room_id}/subscriptions/{subscription_id}/layer",
            post(set_subscription_layer),
        )
        .route("/v1/rooms/{room_id}/webrtc/offer", post(answer_offer))
        .route("/v1/rooms/{room_id}/whip", post(create_whip_session))
        .route("/v1/rooms/{room_id}/whep", post(create_whep_session))
        .route(
            "/v1/rooms/{room_id}/whip/{session_id}",
            patch(patch_whip_session).delete(delete_whip_session),
        )
        .route(
            "/v1/rooms/{room_id}/whep/{session_id}",
            patch(patch_whep_session).delete(delete_whep_session),
        )
        .route(
            "/v1/rooms/{room_id}/signals",
            post(post_signal).get(get_signals),
        )
        .route(
            "/v1/rooms/{room_id}/events/tickets",
            post(issue_event_ticket),
        )
        .route("/v1/rooms/{room_id}/events", get(room_events))
        .route("/v1/rooms/{room_id}/ice-servers", get(get_ice_servers))
        .route("/v1/assets", post(create_asset))
        .route("/v1/assets/{asset_id}", get(get_asset).delete(delete_asset))
        .route("/v1/assets/{asset_id}/source", patch(upload_asset_chunk))
        .route("/v1/assets/{asset_id}/complete", post(complete_asset))
        .route(
            "/v1/live/{stream_id}",
            post(create_live_stream)
                .get(get_live_stream)
                .delete(delete_live_stream),
        )
        .route("/v1/live/{stream_id}/init", put(upload_live_init))
        .route(
            "/v1/live/{stream_id}/segments/{sequence}",
            put(upload_live_segment),
        )
        .route("/v1/live/{stream_id}/finish", post(finish_live_stream))
        .layer(DefaultBodyLimit::max(MAX_JSON_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            reject_revoked_token,
        ))
        .with_state(state);
    apply_cors(router)
}

fn apply_cors(router: Router) -> Router {
    let Ok(value) = env::var("FLUVORA_CORS_ORIGINS") else {
        return router;
    };
    let mut layer = CorsLayer::new()
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            CONTENT_TYPE,
            IF_MATCH,
            axum::http::header::IF_NONE_MATCH,
            axum::http::HeaderName::from_static("idempotency-key"),
        ])
        .expose_headers([LOCATION, ETAG, axum::http::header::LINK])
        .max_age(std::time::Duration::from_mins(10));
    if value.trim() == "*" {
        layer = layer.allow_origin(Any);
    } else {
        let origins = value
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(|origin| {
                origin
                    .parse::<HeaderValue>()
                    .expect("FLUVORA_CORS_ORIGINS contains an invalid origin")
            })
            .collect::<Vec<_>>();
        assert!(
            !origins.is_empty(),
            "FLUVORA_CORS_ORIGINS must contain at least one origin"
        );
        layer = layer.allow_origin(AllowOrigin::list(origins));
    }
    router.layer(layer)
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    let media = dependency_ready(&state.http_client, &state.media_control_url);
    let gateway = dependency_ready(&state.http_client, &state.gateway_control_url);
    let worker = dependency_ready(&state.http_client, &state.worker_control_url);
    let database = persistence_ready(&state.persistence);
    let (media, gateway, worker, database) = tokio::join!(media, gateway, worker, database);
    if media && gateway && worker && database && state.event_bus_ready.load(Ordering::Acquire) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn persistence_ready(persistence: &RoomPersistence) -> bool {
    match persistence {
        RoomPersistence::Files(_) => true,
        RoomPersistence::Postgres(store) => store.healthcheck().await.is_ok(),
    }
}

async fn dependency_ready(client: &reqwest::Client, base_url: &str) -> bool {
    let request = client
        .get(format!("{}/health/ready", base_url.trim_end_matches('/')))
        .send();
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), request).await,
        Ok(Ok(response)) if response.status().is_success()
    )
}

async fn metrics(State(state): State<AppState>) -> String {
    state.metrics.render_prometheus()
}
