use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluvora_control_store::{MediaNodeHeartbeat, PostgresStore, ServiceNodeHeartbeat};
use fluvora_status_service::{
    NodeHeartbeatInput, PlatformStatus, RegistryError, ServiceKind, StatusRegistry,
};
use serde::Serialize;

#[derive(Clone)]
struct AppState {
    registry: Arc<StatusRegistry>,
    heartbeat_token: Arc<str>,
    control_store: Option<PostgresStore>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

impl From<fluvora_control_store::StoreError> for ApiError {
    fn from(error: fluvora_control_store::StoreError) -> Self {
        eprintln!("status-service control-store operation failed: {error}");
        drop(error);
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "control_store_unavailable",
            message: "control store is unavailable".to_owned(),
        }
    }
}

#[tokio::main]
async fn main() {
    let bind = env::var("FLUVORA_STATUS_BIND").unwrap_or_else(|_| "127.0.0.1:8090".to_owned());
    let address: SocketAddr = bind.parse().expect("FLUVORA_STATUS_BIND must be host:port");
    let token = env::var("FLUVORA_STATUS_TOKEN").expect("FLUVORA_STATUS_TOKEN is required");
    assert!(
        (16..=4_096).contains(&token.len()) && !token.bytes().any(|byte| byte.is_ascii_control()),
        "FLUVORA_STATUS_TOKEN must contain 16..=4096 non-control bytes"
    );
    let state = AppState {
        registry: Arc::new(StatusRegistry::new(15_000)),
        heartbeat_token: Arc::from(token),
        control_store: initialize_control_store().await,
    };
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/status", get(status))
        .route("/v1/nodes/{node_id}/heartbeat", post(heartbeat))
        .route("/metrics", get(metrics))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("status listener bind");
    println!(
        "{} status service listening on {address}",
        fluvora_domain::PLATFORM_NAME
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("status server");
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    let _ = state.registry.snapshot(now_millis());
    if let Some(store) = &state.control_store
        && store.healthcheck().await.is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

async fn status(State(state): State<AppState>) -> Json<PlatformStatus> {
    Json(state.registry.snapshot(now_millis()))
}

async fn heartbeat(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NodeHeartbeatInput>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.heartbeat_token)?;
    let media_node = media_node_heartbeat(&node_id, &input);
    let service_node = service_node_heartbeat(&node_id, &input);
    state
        .registry
        .upsert(node_id, input, now_millis())
        .map_err(registry_error)?;
    if let (Some(store), Some(media_node)) = (&state.control_store, media_node) {
        store
            .upsert_media_node(&media_node)
            .await
            .map_err(ApiError::from)?;
    }
    if let (Some(store), Some(service_node)) = (&state.control_store, service_node) {
        store
            .upsert_service_node(&service_node)
            .await
            .map_err(ApiError::from)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn service_node_heartbeat(
    node_id: &str,
    input: &NodeHeartbeatInput,
) -> Option<ServiceNodeHeartbeat> {
    let endpoint = input.control_endpoint.clone()?;
    (input.service != ServiceKind::MediaNode && input.capacity.jobs_limit > 0).then(|| {
        ServiceNodeHeartbeat {
            node_id: node_id.to_owned(),
            service_kind: input.service.as_str().to_owned(),
            region: input.region.clone(),
            endpoint,
            healthy: input.healthy,
            draining: input.draining,
            jobs_used: input.capacity.jobs_used,
            jobs_limit: input.capacity.jobs_limit,
            metadata: serde_json::json!({
                "version": input.version,
                "assets": input.capacity.assets,
                "live_streams": input.capacity.live_streams
            }),
        }
    })
}

fn media_node_heartbeat(node_id: &str, input: &NodeHeartbeatInput) -> Option<MediaNodeHeartbeat> {
    let endpoint = input.control_endpoint.clone()?;
    (input.service == ServiceKind::MediaNode).then(|| MediaNodeHeartbeat {
        node_id: node_id.to_owned(),
        region: input.region.clone(),
        endpoint,
        ice_candidate: input.media_candidate.clone(),
        healthy: input.healthy,
        draining: input.draining,
        rooms_used: input.capacity.rooms_used,
        rooms_limit: input.capacity.rooms_limit,
        sessions_used: input.capacity.sessions_used,
        sessions_limit: input.capacity.sessions_limit,
        publisher_tracks: input.capacity.publisher_tracks,
        metadata: serde_json::json!({"version": input.version}),
    })
}

async fn initialize_control_store() -> Option<PostgresStore> {
    let Ok(database_url) = env::var("FLUVORA_DATABASE_URL") else {
        return None;
    };
    let maximum_connections = env::var("FLUVORA_DATABASE_MAX_CONNECTIONS")
        .map_or(Ok(8), |value| value.parse::<u32>())
        .expect("FLUVORA_DATABASE_MAX_CONNECTIONS must be an integer");
    let store = PostgresStore::connect(&database_url, maximum_connections)
        .await
        .expect("connect FLUVORA_DATABASE_URL");
    store.migrate().await.expect("apply PostgreSQL migrations");
    Some(store)
}

async fn metrics(State(state): State<AppState>) -> String {
    let snapshot = state.registry.snapshot(now_millis());
    let healthy = snapshot
        .nodes
        .values()
        .filter(|node| node.healthy && !node.draining)
        .count();
    let mut output = format!(
        "# TYPE fluvora_status_nodes gauge\nfluvora_status_nodes {}\n\
         # TYPE fluvora_status_healthy_nodes gauge\nfluvora_status_healthy_nodes {healthy}\n\
         # TYPE fluvora_status_sessions gauge\nfluvora_status_sessions {}\n\
         # TYPE fluvora_status_rooms gauge\nfluvora_status_rooms {}\n",
        snapshot.nodes.len(),
        snapshot.capacity.sessions_used,
        snapshot.capacity.rooms_used
    );
    for (service, summary) in &snapshot.services {
        use std::fmt::Write as _;
        let _ = writeln!(
            output,
            "fluvora_status_service_instances{{service=\"{service}\",state=\"total\"}} {}",
            summary.total
        );
        let _ = writeln!(
            output,
            "fluvora_status_service_instances{{service=\"{service}\",state=\"available\"}} {}",
            summary.available
        );
        let _ = writeln!(
            output,
            "fluvora_status_service_instances{{service=\"{service}\",state=\"draining\"}} {}",
            summary.draining
        );
    }
    output
}

fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied == Some(expected) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "invalid heartbeat bearer token".to_owned(),
        })
    }
}

fn registry_error(error: RegistryError) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_heartbeat",
        message: error.to_string(),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
