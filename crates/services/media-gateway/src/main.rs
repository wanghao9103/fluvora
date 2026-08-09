use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::{Component, Path as FilePath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use fluvora_control_store::{PostgresStore, ServicePlacement};
use fluvora_media_pipeline::{AssetState, LivePlaylist, Segment, VodAsset};
use fluvora_media_store::{MediaStore, PublishLimits, StoreError};
use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

mod control_client;
mod metadata;

use control_client::{
    bounded_json, build_internal_http_client, internal_url, normalize_http_origin,
    validate_control_token,
};
use metadata::{load_assets, load_live_streams, persist_asset, persist_live_stream};

const MAX_UPLOAD_CHUNK_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_SEGMENT_BYTES: usize = 32 * 1_024 * 1_024;
const MAX_MEDIA_OBJECT_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_SOURCE_OBJECT_BYTES: u64 = 1_024 * 1_024 * 1_024 * 1_024;

#[derive(Clone)]
struct AppState {
    assets: Arc<Mutex<HashMap<String, ManagedAsset>>>,
    live_streams: Arc<Mutex<HashMap<String, LiveStream>>>,
    input_root: Arc<PathBuf>,
    output_root: Arc<PathBuf>,
    live_root: Arc<PathBuf>,
    metadata_root: Arc<PathBuf>,
    media_store: MediaStore,
    control_store: Option<PostgresStore>,
    token: Arc<str>,
    worker_url: Arc<str>,
    worker_token: Arc<str>,
    public_base_url: Arc<str>,
    media_node_url: Arc<str>,
    media_node_token: Arc<str>,
    vod_retention: Option<Duration>,
    live_retention: Option<Duration>,
    retention_interval: Duration,
    region: Arc<str>,
    http: reqwest::Client,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManagedAsset {
    asset: VodAsset,
    job_id: Option<u64>,
    revision: u64,
    #[serde(default = "now_millis")]
    created_at_millis: u64,
    #[serde(default = "now_millis")]
    updated_at_millis: u64,
    #[serde(default)]
    worker_endpoint: Option<String>,
    #[serde(default)]
    placement_generation: Option<u64>,
    #[serde(default)]
    job_spec: Option<VodJobSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VodJobSpec {
    segment_duration_millis: u32,
    renditions: Vec<RenditionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiveStream {
    playlist: LivePlaylist,
    revision: u64,
    worker_job_id: Option<u64>,
    worker_active: bool,
    recording_bindings: Vec<RecordingBinding>,
    #[serde(default = "now_millis")]
    created_at_millis: u64,
    #[serde(default = "now_millis")]
    updated_at_millis: u64,
    #[serde(default)]
    finished_at_millis: Option<u64>,
    #[serde(default)]
    deleted_at_millis: Option<u64>,
    #[serde(default)]
    purged_at_millis: Option<u64>,
    #[serde(default)]
    worker_endpoint: Option<String>,
    #[serde(default)]
    placement_generation: Option<u64>,
    #[serde(default)]
    job_spec: Option<LiveJobSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LiveJobSpec {
    segment_duration_millis: u32,
    window_segments: usize,
    tracks: Vec<LiveSourceTrack>,
    #[serde(default)]
    renditions: Vec<RenditionRequest>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedLiveStream {
    stream_id: String,
    stream: LiveStream,
}

#[derive(Debug, Deserialize)]
struct CreateAssetRequest {
    asset_id: String,
    tenant_id: String,
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    offset: u64,
}

#[derive(Debug, Deserialize)]
struct CompleteAssetRequest {
    source_bytes: u64,
    #[serde(default = "default_segment_duration")]
    segment_duration_millis: u32,
    renditions: Vec<RenditionRequest>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RenditionRequest {
    width: u16,
    height: u16,
    video_bitrate_bps: u64,
    audio_bitrate_bps: u32,
}

#[derive(Debug, Serialize)]
struct WorkerJobRequest {
    asset_id: String,
    input: String,
    output_directory: String,
    segment_duration_millis: u32,
    renditions: Vec<RenditionRequest>,
    placement_resource_id: String,
    placement_generation: u64,
}

#[derive(Debug, Deserialize)]
struct WorkerJobResponse {
    job_id: u64,
}

#[derive(Debug, Deserialize)]
struct WorkerJob {
    state: String,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct AssetResponse {
    asset_id: String,
    tenant_id: String,
    version: u64,
    state: &'static str,
    received_bytes: Option<u64>,
    source_bytes: Option<u64>,
    manifest_url: Option<String>,
    duration_millis: Option<u64>,
    failure_reason: Option<String>,
    retryable: Option<bool>,
    job_id: Option<u64>,
    created_at_millis: u64,
    updated_at_millis: u64,
}

#[derive(Debug, Deserialize)]
struct CreateLiveRequest {
    #[serde(default = "default_live_window")]
    window_segments: usize,
    #[serde(default)]
    first_sequence: u64,
    #[serde(default = "default_segment_duration")]
    segment_duration_millis: u32,
    #[serde(default)]
    source_tracks: Vec<LiveSourceTrack>,
    #[serde(default)]
    renditions: Vec<RenditionRequest>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LiveSourceTrack {
    room_id: String,
    track_id: u64,
    kind: String,
    codec: String,
    payload_type: u8,
    clock_rate: u32,
    channels: Option<u8>,
    fmtp: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerLiveJobRequest {
    stream_id: String,
    output_directory: String,
    segment_duration_millis: u32,
    window_segments: usize,
    tracks: Vec<LiveSourceTrack>,
    renditions: Vec<RenditionRequest>,
    placement_resource_id: String,
    placement_generation: u64,
}

#[derive(Debug, Deserialize)]
struct WorkerLiveJobResponse {
    job_id: u64,
    destinations: Vec<WorkerLiveDestination>,
}

#[derive(Debug, Deserialize)]
struct WorkerLiveDestination {
    track_id: u64,
    destination: SocketAddr,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RecordingBinding {
    room_id: String,
    track_id: u64,
    destination: SocketAddr,
}

#[derive(Debug, Deserialize)]
struct SegmentQuery {
    duration_millis: u64,
    #[serde(default)]
    discontinuity: bool,
    program_date_time: Option<String>,
}

#[derive(Debug, Serialize)]
struct LiveResponse {
    stream_id: String,
    next_sequence: u64,
    manifest_url: String,
    worker_job_id: Option<u64>,
    finished_at_millis: Option<u64>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    assets: usize,
    live_streams: usize,
    object_store_backend: &'static str,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
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

async fn connect_control_store() -> Result<Option<PostgresStore>, String> {
    let Some(database_url) = env::var("FLUVORA_DATABASE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let maximum_connections = env::var("FLUVORA_DATABASE_MAX_CONNECTIONS")
        .unwrap_or_else(|_| "8".to_owned())
        .parse::<u32>()
        .map_err(|_| "FLUVORA_DATABASE_MAX_CONNECTIONS must be an integer".to_owned())?;
    let store = PostgresStore::connect(&database_url, maximum_connections)
        .await
        .map_err(|error| error.to_string())?;
    store.migrate().await.map_err(|error| error.to_string())?;
    Ok(Some(store))
}

#[tokio::main]
async fn main() {
    let bind = env::var("FLUVORA_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:8093".to_owned());
    let address: SocketAddr = bind
        .parse()
        .expect("FLUVORA_GATEWAY_BIND must be host:port");
    let token = env::var("FLUVORA_GATEWAY_TOKEN").expect("FLUVORA_GATEWAY_TOKEN is required");
    let worker_token = env::var("FLUVORA_WORKER_TOKEN").expect("FLUVORA_WORKER_TOKEN is required");
    let media_node_token =
        env::var("FLUVORA_MEDIA_CONTROL_TOKEN").expect("FLUVORA_MEDIA_CONTROL_TOKEN is required");
    validate_control_token(&token).expect("FLUVORA_GATEWAY_TOKEN is invalid");
    validate_control_token(&worker_token).expect("FLUVORA_WORKER_TOKEN is invalid");
    validate_control_token(&media_node_token).expect("FLUVORA_MEDIA_CONTROL_TOKEN is invalid");
    let storage_root =
        PathBuf::from(env::var("FLUVORA_STORAGE_ROOT").unwrap_or_else(|_| "./data".to_owned()));
    let input_root = create_canonical_directory(storage_root.join("input"))
        .await
        .expect("input storage directory");
    let output_root = create_canonical_directory(storage_root.join("output"))
        .await
        .expect("output storage directory");
    let live_root = create_canonical_directory(storage_root.join("live"))
        .await
        .expect("live storage directory");
    let metadata_root = create_canonical_directory(storage_root.join("metadata"))
        .await
        .expect("metadata storage directory");
    let media_store = MediaStore::from_env(storage_root.join("objects"))
        .expect("valid media object store configuration");
    media_store
        .healthcheck()
        .await
        .expect("media object store readiness");
    let control_store = connect_control_store()
        .await
        .expect("valid gateway PostgreSQL configuration");
    let assets = load_assets(&metadata_root).expect("load VOD metadata");
    let live_streams = load_live_streams(&metadata_root).expect("load live metadata");
    let vod_retention =
        retention_from_env("FLUVORA_VOD_RETENTION_HOURS").expect("valid VOD retention");
    let live_retention =
        retention_from_env("FLUVORA_LIVE_RETENTION_HOURS").expect("valid live retention");
    let retention_interval = retention_interval_from_env().expect("valid retention interval");
    let state = AppState {
        assets: Arc::new(Mutex::new(assets)),
        live_streams: Arc::new(Mutex::new(live_streams)),
        input_root: Arc::new(input_root),
        output_root: Arc::new(output_root),
        live_root: Arc::new(live_root),
        metadata_root: Arc::new(metadata_root),
        media_store,
        control_store,
        token: Arc::from(token),
        worker_url: Arc::from(
            normalize_http_origin(
                &env::var("FLUVORA_WORKER_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8091".to_owned()),
            )
            .expect("FLUVORA_WORKER_URL is invalid"),
        ),
        worker_token: Arc::from(worker_token),
        public_base_url: Arc::from(
            normalize_http_origin(
                &env::var("FLUVORA_PUBLIC_MEDIA_BASE_URL")
                    .unwrap_or_else(|_| format!("http://{address}")),
            )
            .expect("FLUVORA_PUBLIC_MEDIA_BASE_URL is invalid"),
        ),
        media_node_url: Arc::from(
            normalize_http_origin(
                &env::var("FLUVORA_MEDIA_CONTROL_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8092".to_owned()),
            )
            .expect("FLUVORA_MEDIA_CONTROL_URL is invalid"),
        ),
        media_node_token: Arc::from(media_node_token),
        vod_retention,
        live_retention,
        retention_interval,
        region: Arc::from(env::var("FLUVORA_REGION").unwrap_or_else(|_| "default".to_owned())),
        http: build_internal_http_client(),
    };
    resume_worker_monitors(&state).await;
    spawn_retention_task(state.clone());
    let app = build_router(state.clone());
    let (heartbeat, heartbeat_task) = start_gateway_heartbeat(state.clone());
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("gateway listener bind");
    println!(
        "{} media gateway listening on {address}",
        fluvora_domain::PLATFORM_NAME
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("media gateway");
    stop_gateway_heartbeat(heartbeat.as_ref(), heartbeat_task, &state).await;
}

fn build_router(state: AppState) -> Router {
    apply_cors(
        Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(health))
            .route("/metrics", get(metrics))
            .route("/v1/assets", post(create_asset))
            .route("/v1/assets/{asset_id}", get(get_asset).delete(delete_asset))
            .route("/v1/assets/{asset_id}/source", patch(upload_source_chunk))
            .route("/v1/assets/{asset_id}/complete", post(complete_asset))
            .route(
                "/v1/live/{stream_id}",
                post(create_live).get(get_live).delete(delete_live),
            )
            .route("/v1/live/{stream_id}/init", put(upload_live_init))
            .route(
                "/v1/live/{stream_id}/segments/{sequence}",
                put(upload_live_segment),
            )
            .route("/v1/live/{stream_id}/finish", post(finish_live))
            .route("/media/vod/{asset_id}/{*object}", get(serve_vod))
            .route("/media/live/{stream_id}/{*object}", get(serve_live))
            .with_state(state),
    )
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
            AUTHORIZATION,
            CONTENT_TYPE,
            RANGE,
            axum::http::header::IF_MATCH,
        ])
        .expose_headers([
            ACCEPT_RANGES,
            CONTENT_LENGTH,
            CONTENT_RANGE,
            CONTENT_TYPE,
            ETAG,
        ])
        .max_age(Duration::from_mins(10));
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

fn start_gateway_heartbeat(
    state: AppState,
) -> (Option<HeartbeatClient>, Option<tokio::task::JoinHandle<()>>) {
    let client = HeartbeatClient::from_env(ServiceKind::MediaGateway)
        .expect("valid status heartbeat configuration");
    let task = client.as_ref().map(|client| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .run(|| {
                    let state = state.clone();
                    async move { gateway_capacity(&state).await }
                })
                .await;
        })
    });
    (client, task)
}

async fn stop_gateway_heartbeat(
    client: Option<&HeartbeatClient>,
    task: Option<tokio::task::JoinHandle<()>>,
    state: &AppState,
) {
    if let Some(client) = client {
        client.mark_draining();
        if let Err(error) = client.report(gateway_capacity(state).await, true).await {
            eprintln!("failed to report draining gateway heartbeat: {error}");
        }
    }
    if let Some(task) = task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            eprintln!("gateway heartbeat task failed during shutdown: {error}");
        }
    }
}

async fn gateway_capacity(state: &AppState) -> NodeCapacity {
    NodeCapacity {
        assets: u64::try_from(state.assets.lock().await.len()).unwrap_or(u64::MAX),
        live_streams: u64::try_from(state.live_streams.lock().await.len()).unwrap_or(u64::MAX),
        memory_bytes: process_memory_bytes(),
        ..NodeCapacity::default()
    }
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn metrics(State(state): State<AppState>) -> String {
    let capacity = gateway_capacity(&state).await;
    format!(
        "# HELP fluvora_gateway_assets Retained VOD assets.\n\
         # TYPE fluvora_gateway_assets gauge\n\
         fluvora_gateway_assets {}\n\
         # HELP fluvora_gateway_live_streams Retained live stream records.\n\
         # TYPE fluvora_gateway_live_streams gauge\n\
         fluvora_gateway_live_streams {}\n",
        capacity.assets, capacity.live_streams
    )
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    state
        .media_store
        .head("_system/healthcheck-v1")
        .await
        .map_err(store_error)?;
    if let Some(store) = &state.control_store {
        store.healthcheck().await.map_err(control_store_error)?;
    }
    Ok(Json(HealthResponse {
        assets: state.assets.lock().await.len(),
        live_streams: state.live_streams.lock().await.len(),
        object_store_backend: state.media_store.backend(),
    }))
}

async fn create_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAssetRequest>,
) -> Result<(StatusCode, Json<AssetResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    let asset = VodAsset::create(request.asset_id.clone(), request.tenant_id)
        .map_err(|error| pipeline_error(&error))?;
    let mut assets = state.assets.lock().await;
    if assets.contains_key(&request.asset_id) {
        return Err(conflict("asset already exists"));
    }
    tokio::fs::create_dir_all(state.input_root.join(&request.asset_id))
        .await
        .map_err(io_error)?;
    let now = now_millis();
    let managed = ManagedAsset {
        asset,
        job_id: None,
        revision: 1,
        created_at_millis: now,
        updated_at_millis: now,
        worker_endpoint: None,
        placement_generation: None,
        job_spec: None,
    };
    let response = asset_response(&managed, &state.public_base_url);
    persist_asset(&state, &managed).await?;
    assets.insert(request.asset_id, managed);
    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<AssetResponse>, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&asset_id)?;
    let assets = state.assets.lock().await;
    let asset = assets.get(&asset_id).ok_or_else(asset_not_found)?;
    Ok(Json(asset_response(asset, &state.public_base_url)))
}

async fn delete_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&asset_id)?;
    delete_asset_storage(&state, &asset_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_asset_storage(state: &AppState, asset_id: &str) -> Result<(), ApiError> {
    {
        let mut assets = state.assets.lock().await;
        let managed = assets.get_mut(asset_id).ok_or_else(asset_not_found)?;
        if matches!(
            managed.asset.state,
            AssetState::Probing | AssetState::Transcoding { .. }
        ) {
            return Err(conflict("an active media job cannot be deleted"));
        }
        if matches!(managed.asset.state, AssetState::Deleted) {
            // Retry the idempotent physical cleanup after a partial prior attempt.
        } else if !matches!(managed.asset.state, AssetState::Deleting) {
            managed
                .asset
                .start_delete()
                .map_err(|error| pipeline_error(&error))?;
            managed.revision = managed
                .revision
                .checked_add(1)
                .ok_or_else(revision_exhausted)?;
            managed.updated_at_millis = now_millis();
            persist_asset(state, managed).await?;
        }
    }
    state
        .media_store
        .delete_prefix(&format!("vod/{asset_id}"))
        .await
        .map_err(store_error)?;
    state
        .media_store
        .delete_prefix(&format!("source/{asset_id}"))
        .await
        .map_err(store_error)?;
    release_worker_placement(state, "vod_transcode", asset_id).await;
    remove_namespace_directory(&state.input_root, asset_id).await?;
    remove_namespace_directory(&state.output_root, asset_id).await?;
    let mut assets = state.assets.lock().await;
    let managed = assets.get_mut(asset_id).ok_or_else(asset_not_found)?;
    if matches!(managed.asset.state, AssetState::Deleting) {
        managed
            .asset
            .finish_delete()
            .map_err(|error| pipeline_error(&error))?;
        managed.job_id = None;
        managed.revision = managed
            .revision
            .checked_add(1)
            .ok_or_else(revision_exhausted)?;
        managed.updated_at_millis = now_millis();
        persist_asset(state, managed).await?;
    }
    Ok(())
}

async fn upload_source_chunk(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<AssetResponse>, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&asset_id)?;
    let bytes = to_bytes(body, MAX_UPLOAD_CHUNK_BYTES)
        .await
        .map_err(|_| payload_too_large(MAX_UPLOAD_CHUNK_BYTES))?;
    if bytes.is_empty() {
        return Err(unprocessable("upload chunk cannot be empty"));
    }
    let mut assets = state.assets.lock().await;
    let managed = assets.get_mut(&asset_id).ok_or_else(asset_not_found)?;
    let expected = match &managed.asset.state {
        AssetState::Created => 0,
        AssetState::Uploading { received_bytes } => *received_bytes,
        _ => return Err(conflict("asset is not accepting upload bytes")),
    };
    if query.offset != expected {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "upload_offset_mismatch",
            message: format!("expected byte offset {expected}"),
        });
    }
    let source = state.input_root.join(&asset_id).join("source.bin");
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(source)
        .await
        .map_err(io_error)?;
    file.write_all(&bytes).await.map_err(io_error)?;
    file.sync_data().await.map_err(io_error)?;
    let received = expected
        .checked_add(u64::try_from(bytes.len()).map_err(internal_error)?)
        .ok_or_else(|| unprocessable("source size overflow"))?;
    managed
        .asset
        .upload_progress(received)
        .map_err(|error| pipeline_error(&error))?;
    managed.revision = managed
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    managed.updated_at_millis = now_millis();
    persist_asset(&state, managed).await?;
    Ok(Json(asset_response(managed, &state.public_base_url)))
}

async fn complete_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CompleteAssetRequest>,
) -> Result<(StatusCode, Json<AssetResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&asset_id)?;
    let job_spec = validate_vod_job_request(&request)?;
    publish_source_object(&state, &asset_id, request.source_bytes).await?;
    let placement = place_worker(&state, "vod_transcode", &asset_id, false).await?;
    {
        let mut assets = state.assets.lock().await;
        let managed = assets.get_mut(&asset_id).ok_or_else(asset_not_found)?;
        managed
            .asset
            .complete_upload(request.source_bytes)
            .and_then(|()| managed.asset.start_probe())
            .and_then(|()| managed.asset.start_transcode())
            .map_err(|error| pipeline_error(&error))?;
        managed.revision = managed
            .revision
            .checked_add(1)
            .ok_or_else(revision_exhausted)?;
        managed.updated_at_millis = now_millis();
        managed.worker_endpoint = Some(placement.endpoint.clone());
        managed.placement_generation = Some(placement.generation);
        managed.job_spec = Some(job_spec.clone());
        persist_asset(&state, managed).await?;
    }
    let output_directory = vod_output_directory(&asset_id, placement.generation);
    tokio::fs::create_dir_all(state.output_root.join(&output_directory))
        .await
        .map_err(io_error)?;
    let job = match submit_vod_job(&state, &asset_id, &job_spec, &placement, output_directory).await
    {
        Ok(job) => job,
        Err(error) => {
            mark_asset_failed(&state, &asset_id, &error.message).await;
            release_worker_placement(&state, "vod_transcode", &asset_id).await;
            return Err(error);
        }
    };
    let response = {
        let mut assets = state.assets.lock().await;
        let managed = assets.get_mut(&asset_id).ok_or_else(asset_not_found)?;
        managed.job_id = Some(job.job_id);
        managed.revision = managed
            .revision
            .checked_add(1)
            .ok_or_else(revision_exhausted)?;
        managed.updated_at_millis = now_millis();
        persist_asset(&state, managed).await?;
        asset_response(managed, &state.public_base_url)
    };
    let monitor_state = state.clone();
    let monitor_asset_id = asset_id;
    let monitor_endpoint = placement.endpoint;
    let monitor_generation = placement.generation;
    tokio::spawn(async move {
        monitor_worker_job(
            monitor_state,
            monitor_asset_id,
            job.job_id,
            monitor_endpoint,
            monitor_generation,
        )
        .await;
    });
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn submit_vod_job(
    state: &AppState,
    asset_id: &str,
    job_spec: &VodJobSpec,
    placement: &ServicePlacement,
    output_directory: String,
) -> Result<WorkerJobResponse, ApiError> {
    let response = state
        .http
        .post(internal_url(&placement.endpoint, "/v1/jobs")?)
        .bearer_auth(state.worker_token.as_ref())
        .json(&WorkerJobRequest {
            asset_id: asset_id.to_owned(),
            input: format!("{asset_id}/source.bin"),
            output_directory,
            segment_duration_millis: job_spec.segment_duration_millis,
            renditions: job_spec.renditions.clone(),
            placement_resource_id: asset_id.to_owned(),
            placement_generation: placement.generation,
        })
        .send()
        .await
        .map_err(worker_unavailable)?;
    if !response.status().is_success() {
        return Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "worker_rejected_job",
            message: format!("media worker returned {}", response.status()),
        });
    }
    bounded_json(response, "worker_invalid_response").await
}

fn validate_vod_job_request(request: &CompleteAssetRequest) -> Result<VodJobSpec, ApiError> {
    if !(1_000..=10_000).contains(&request.segment_duration_millis) {
        return Err(unprocessable(
            "VOD segment duration must be between 1000 and 10000 milliseconds",
        ));
    }
    validate_rendition_ladder(&request.renditions, false)?;
    Ok(VodJobSpec {
        segment_duration_millis: request.segment_duration_millis,
        renditions: request.renditions.clone(),
    })
}

fn validate_live_job_request(request: &CreateLiveRequest) -> Result<(), ApiError> {
    if !(1_000..=10_000).contains(&request.segment_duration_millis) {
        return Err(unprocessable(
            "live segment duration must be between 1000 and 10000 milliseconds",
        ));
    }
    validate_rendition_ladder(&request.renditions, true)?;
    if !request.renditions.is_empty() {
        if request.source_tracks.is_empty() {
            return Err(unprocessable(
                "live ABR renditions require worker-backed source tracks",
            ));
        }
        if !request
            .source_tracks
            .iter()
            .any(|track| track.kind == "video")
        {
            return Err(unprocessable(
                "live ABR renditions require a video source track",
            ));
        }
    }
    Ok(())
}

fn validate_rendition_ladder(
    renditions: &[RenditionRequest],
    allow_empty: bool,
) -> Result<(), ApiError> {
    if renditions.len() > 8 || (!allow_empty && renditions.is_empty()) {
        return Err(unprocessable("rendition ladder must contain 1..=8 outputs"));
    }
    if renditions.iter().any(|rendition| {
        rendition.width < 16
            || rendition.height < 16
            || rendition.width > 7_680
            || rendition.height > 4_320
            || !rendition.width.is_multiple_of(2)
            || !rendition.height.is_multiple_of(2)
            || !(50_000..=100_000_000).contains(&rendition.video_bitrate_bps)
            || !(16_000..=1_000_000).contains(&rendition.audio_bitrate_bps)
    }) {
        return Err(unprocessable(
            "rendition dimensions or bitrates are outside supported bounds",
        ));
    }
    Ok(())
}

async fn publish_source_object(
    state: &AppState,
    asset_id: &str,
    source_bytes: u64,
) -> Result<(), ApiError> {
    {
        let assets = state.assets.lock().await;
        let managed = assets.get(asset_id).ok_or_else(asset_not_found)?;
        let AssetState::Uploading { received_bytes } = managed.asset.state else {
            return Err(conflict("asset upload is not ready to complete"));
        };
        if received_bytes != source_bytes || source_bytes == 0 {
            return Err(unprocessable(
                "declared source size does not match uploaded bytes",
            ));
        }
    }
    let source_path = state.input_root.join(asset_id).join("source.bin");
    let source_metadata = tokio::fs::metadata(&source_path).await.map_err(io_error)?;
    if !source_metadata.is_file() || source_metadata.len() != source_bytes {
        return Err(conflict(
            "staged source file does not match the recorded upload",
        ));
    }
    state
        .media_store
        .put_file(
            &format!("source/{asset_id}/source.bin"),
            source_path,
            MAX_SOURCE_OBJECT_BYTES,
        )
        .await
        .map(|_| ())
        .map_err(store_error)
}

async fn place_worker(
    state: &AppState,
    resource_kind: &str,
    resource_id: &str,
    advance: bool,
) -> Result<ServicePlacement, ApiError> {
    let Some(store) = &state.control_store else {
        return Ok(ServicePlacement {
            resource_kind: resource_kind.to_owned(),
            resource_id: resource_id.to_owned(),
            node_id: "static-worker".to_owned(),
            endpoint: state.worker_url.to_string(),
            generation: 1,
        });
    };
    let mut placement = (if advance {
        store
            .advance_service_placement(
                resource_kind,
                resource_id,
                ServiceKind::MediaWorker.as_str(),
                &state.region,
                Duration::from_secs(15),
            )
            .await
    } else {
        store
            .place_service_resource(
                resource_kind,
                resource_id,
                ServiceKind::MediaWorker.as_str(),
                &state.region,
                Duration::from_secs(15),
            )
            .await
    })
    .map_err(control_store_error)?;
    placement.endpoint =
        normalize_http_origin(&placement.endpoint).map_err(|message| ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "worker_invalid_endpoint",
            message: message.to_owned(),
        })?;
    Ok(placement)
}

async fn release_worker_placement(state: &AppState, resource_kind: &str, resource_id: &str) {
    let Some(store) = &state.control_store else {
        return;
    };
    if let Err(error) = store
        .remove_service_placement(resource_kind, resource_id)
        .await
    {
        eprintln!("failed to release {resource_kind} placement for {resource_id}: {error}");
    }
}

async fn create_live(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateLiveRequest>,
) -> Result<(StatusCode, Json<LiveResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    validate_live_job_request(&request)?;
    let playlist = LivePlaylist::new("init.mp4", request.window_segments, request.first_sequence)
        .map_err(|error| pipeline_error(&error))?;
    let mut streams = state.live_streams.lock().await;
    if streams.contains_key(&stream_id) {
        return Err(conflict("live stream already exists"));
    }
    tokio::fs::create_dir_all(state.live_root.join(&stream_id))
        .await
        .map_err(io_error)?;
    let live_job_spec = (!request.source_tracks.is_empty()).then(|| LiveJobSpec {
        segment_duration_millis: request.segment_duration_millis,
        window_segments: request.window_segments,
        tracks: request.source_tracks.clone(),
        renditions: request.renditions.clone(),
    });
    let (worker_job_id, recording_bindings, worker_endpoint, placement_generation) =
        if request.source_tracks.is_empty() {
            (None, Vec::new(), None, None)
        } else {
            let placement = place_worker(&state, "live_package", &stream_id, false).await?;
            let bridge = start_live_rtp_bridge(
                &state,
                &stream_id,
                request.segment_duration_millis,
                request.window_segments,
                request.source_tracks,
                request.renditions,
                &placement,
            )
            .await;
            match bridge {
                Ok((job_id, bindings)) => (
                    Some(job_id),
                    bindings,
                    Some(placement.endpoint),
                    Some(placement.generation),
                ),
                Err(error) => {
                    release_worker_placement(&state, "live_package", &stream_id).await;
                    return Err(error);
                }
            }
        };
    let now = now_millis();
    let stream = LiveStream {
        playlist,
        revision: 1,
        worker_job_id,
        worker_active: worker_job_id.is_some(),
        recording_bindings,
        created_at_millis: now,
        updated_at_millis: now,
        finished_at_millis: None,
        deleted_at_millis: None,
        purged_at_millis: None,
        worker_endpoint,
        placement_generation,
        job_spec: live_job_spec,
    };
    let response = live_response(&stream_id, &stream, &state.public_base_url);
    persist_live_playlist_object(&state, &stream_id, &stream).await?;
    persist_live_stream(&state, &stream_id, &stream).await?;
    let start_publisher = stream.worker_active;
    streams.insert(stream_id.clone(), stream);
    drop(streams);
    if start_publisher {
        spawn_live_publisher(state.clone(), stream_id);
    }
    Ok((StatusCode::CREATED, Json(response)))
}

async fn get_live(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<LiveResponse>, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    let streams = state.live_streams.lock().await;
    let stream = streams.get(&stream_id).ok_or_else(stream_not_found)?;
    if stream.deleted_at_millis.is_some() {
        return Err(stream_not_found());
    }
    Ok(Json(live_response(
        &stream_id,
        stream,
        &state.public_base_url,
    )))
}

async fn upload_live_init(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    let bytes = to_bytes(body, MAX_SEGMENT_BYTES)
        .await
        .map_err(|_| payload_too_large(MAX_SEGMENT_BYTES))?;
    if bytes.is_empty() {
        return Err(unprocessable("initialization segment cannot be empty"));
    }
    let mut streams = state.live_streams.lock().await;
    let stream = streams.get_mut(&stream_id).ok_or_else(stream_not_found)?;
    if stream.finished_at_millis.is_some() || stream.deleted_at_millis.is_some() {
        return Err(conflict("live stream is no longer accepting media"));
    }
    tokio::fs::write(state.live_root.join(&stream_id).join("init.mp4"), &bytes)
        .await
        .map_err(io_error)?;
    state
        .media_store
        .put(&format!("live/{stream_id}/init.mp4"), bytes)
        .await
        .map_err(store_error)?;
    stream.revision = stream
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    stream.updated_at_millis = now_millis();
    persist_live_stream(&state, &stream_id, stream).await?;
    Ok(StatusCode::CREATED)
}

async fn upload_live_segment(
    State(state): State<AppState>,
    Path((stream_id, sequence)): Path<(String, u64)>,
    Query(query): Query<SegmentQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<LiveResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    let bytes = to_bytes(body, MAX_SEGMENT_BYTES)
        .await
        .map_err(|_| payload_too_large(MAX_SEGMENT_BYTES))?;
    if bytes.is_empty() {
        return Err(unprocessable("media segment cannot be empty"));
    }
    let mut streams = state.live_streams.lock().await;
    let stream = streams.get_mut(&stream_id).ok_or_else(stream_not_found)?;
    if stream.finished_at_millis.is_some() || stream.deleted_at_millis.is_some() {
        return Err(conflict("live stream is no longer accepting media"));
    }
    if stream.playlist.next_sequence() != sequence {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "segment_sequence_mismatch",
            message: format!("expected segment {}", stream.playlist.next_sequence()),
        });
    }
    let uri = format!("segment-{sequence}.m4s");
    tokio::fs::write(state.live_root.join(&stream_id).join(&uri), &bytes)
        .await
        .map_err(io_error)?;
    state
        .media_store
        .put(&format!("live/{stream_id}/{uri}"), bytes)
        .await
        .map_err(store_error)?;
    stream
        .playlist
        .push(Segment {
            sequence,
            duration: Duration::from_millis(query.duration_millis),
            uri,
            discontinuity: query.discontinuity,
            program_date_time: query.program_date_time,
        })
        .map_err(|error| pipeline_error(&error))?;
    stream.revision = stream
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    stream.updated_at_millis = now_millis();
    persist_live_playlist_object(&state, &stream_id, stream).await?;
    persist_live_stream(&state, &stream_id, stream).await?;
    Ok((
        StatusCode::CREATED,
        Json(live_response(&stream_id, stream, &state.public_base_url)),
    ))
}

async fn finish_live(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    let mut streams = state.live_streams.lock().await;
    let stream = streams.get_mut(&stream_id).ok_or_else(stream_not_found)?;
    stop_live_rtp_bridge(&state, stream).await?;
    stream.playlist.finish();
    stream.revision = stream
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    let now = now_millis();
    stream.updated_at_millis = now;
    stream.finished_at_millis = Some(now);
    persist_live_playlist_object(&state, &stream_id, stream).await?;
    persist_live_stream(&state, &stream_id, stream).await?;
    drop(streams);
    release_worker_placement(&state, "live_package", &stream_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_live(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&stream_id)?;
    delete_live_storage(&state, &stream_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_live_storage(state: &AppState, stream_id: &str) -> Result<(), ApiError> {
    {
        let mut streams = state.live_streams.lock().await;
        let stream = streams.get_mut(stream_id).ok_or_else(stream_not_found)?;
        if stream.purged_at_millis.is_some() {
            return Ok(());
        }
        if stream.deleted_at_millis.is_none() {
            stop_live_rtp_bridge(state, stream).await?;
            if stream.finished_at_millis.is_none() {
                stream.playlist.finish();
                stream.finished_at_millis = Some(now_millis());
            }
            let now = now_millis();
            stream.deleted_at_millis = Some(now);
            stream.updated_at_millis = now;
            stream.revision = stream
                .revision
                .checked_add(1)
                .ok_or_else(revision_exhausted)?;
            persist_live_stream(state, stream_id, stream).await?;
        }
    }
    state
        .media_store
        .delete_prefix(&format!("live/{stream_id}"))
        .await
        .map_err(store_error)?;
    release_worker_placement(state, "live_package", stream_id).await;
    remove_namespace_directory(&state.live_root, stream_id).await?;
    let mut streams = state.live_streams.lock().await;
    let stream = streams.get_mut(stream_id).ok_or_else(stream_not_found)?;
    if stream.purged_at_millis.is_none() {
        let now = now_millis();
        stream.purged_at_millis = Some(now);
        stream.updated_at_millis = now;
        stream.revision = stream
            .revision
            .checked_add(1)
            .ok_or_else(revision_exhausted)?;
        persist_live_stream(state, stream_id, stream).await?;
    }
    Ok(())
}

async fn serve_vod(
    State(state): State<AppState>,
    Path((asset_id, object)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_identifier(&asset_id)?;
    {
        let assets = state.assets.lock().await;
        let asset = assets.get(&asset_id).ok_or_else(asset_not_found)?;
        if !matches!(asset.asset.state, AssetState::Ready { .. }) {
            return Err(ApiError {
                status: StatusCode::NOT_FOUND,
                code: "media_not_ready",
                message: "VOD output is not ready".to_owned(),
            });
        }
    }
    validate_object_path(&object)?;
    serve_stored_object(
        &state.media_store,
        &format!("vod/{asset_id}/{object}"),
        &object,
        &headers,
        true,
    )
    .await
}

async fn serve_live(
    State(state): State<AppState>,
    Path((stream_id, object)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_identifier(&stream_id)?;
    validate_object_path(&object)?;
    let worker_stream = {
        let streams = state.live_streams.lock().await;
        let stream = streams.get(&stream_id).ok_or_else(stream_not_found)?;
        if stream.deleted_at_millis.is_some() {
            return Err(stream_not_found());
        }
        stream.worker_job_id.is_some()
    };
    let key = format!("live/{stream_id}/{object}");
    match state.media_store.head(&key).await {
        Ok(_) => {}
        Err(error) if error.is_not_found() && worker_stream => {
            publish_live_snapshot(&state, &stream_id).await?;
        }
        Err(error) if error.is_not_found() => return Err(media_not_found()),
        Err(error) => return Err(store_error(error)),
    }
    serve_stored_object(
        &state.media_store,
        &key,
        &object,
        &headers,
        !FilePath::new(&object)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("m3u8")),
    )
    .await
}

async fn serve_stored_object(
    store: &MediaStore,
    key: &str,
    object: &str,
    headers: &HeaderMap,
    immutable: bool,
) -> Result<Response, ApiError> {
    let metadata = match store.head(key).await {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Err(media_not_found()),
        Err(error) => return Err(store_error(error)),
    };
    if metadata.size == 0 || metadata.size > MAX_MEDIA_OBJECT_BYTES {
        return Err(media_not_found());
    }
    let content_type = content_type(object);
    let Some(range) = headers.get(RANGE).and_then(|value| value.to_str().ok()) else {
        let bytes = store.get(key).await.map_err(store_error)?;
        return Ok(media_response(
            StatusCode::OK,
            content_type,
            bytes,
            immutable,
            None,
            metadata.e_tag,
        ));
    };
    let (start, end) = parse_range(range, metadata.size)?;
    let end_exclusive = end.checked_add(1).ok_or_else(range_not_satisfiable)?;
    let bytes = store
        .get_range(key, start..end_exclusive)
        .await
        .map_err(store_error)?;
    let total = metadata.size;
    let content_range = format!("bytes {start}-{end}/{total}");
    Ok(media_response(
        StatusCode::PARTIAL_CONTENT,
        content_type,
        bytes,
        immutable,
        Some(content_range),
        metadata.e_tag,
    ))
}

fn media_response(
    status: StatusCode,
    content_type: &'static str,
    bytes: Bytes,
    immutable: bool,
    content_range: Option<String>,
    e_tag: Option<String>,
) -> Response {
    let content_length = bytes.len();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    if let Ok(value) = HeaderValue::from_str(&content_length.to_string()) {
        headers.insert(CONTENT_LENGTH, value);
    }
    if let Some(content_range) = content_range
        && let Ok(value) = HeaderValue::from_str(&content_range)
    {
        headers.insert(CONTENT_RANGE, value);
    }
    if let Some(e_tag) = e_tag
        && let Ok(value) = HeaderValue::from_str(&e_tag)
    {
        headers.insert(ETAG, value);
    }
    response
}

async fn start_live_rtp_bridge(
    state: &AppState,
    stream_id: &str,
    segment_duration_millis: u32,
    window_segments: usize,
    tracks: Vec<LiveSourceTrack>,
    renditions: Vec<RenditionRequest>,
    placement: &ServicePlacement,
) -> Result<(u64, Vec<RecordingBinding>), ApiError> {
    if tracks.is_empty() || tracks.len() > 2 {
        return Err(unprocessable(
            "live source requires at most one audio and one video track",
        ));
    }
    for track in &tracks {
        validate_identifier(&track.room_id)?;
    }
    let response = state
        .http
        .post(internal_url(&placement.endpoint, "/v1/live-jobs")?)
        .bearer_auth(state.worker_token.as_ref())
        .json(&WorkerLiveJobRequest {
            stream_id: stream_id.to_owned(),
            output_directory: live_output_directory(stream_id, placement.generation),
            segment_duration_millis,
            window_segments,
            tracks: tracks.clone(),
            renditions,
            placement_resource_id: stream_id.to_owned(),
            placement_generation: placement.generation,
        })
        .send()
        .await
        .map_err(worker_unavailable)?;
    if !response.status().is_success() {
        return Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "live_worker_rejected",
            message: format!("media worker returned {}", response.status()),
        });
    }
    let allocation =
        bounded_json::<WorkerLiveJobResponse>(response, "live_worker_invalid_response").await?;
    let mut bindings = Vec::with_capacity(tracks.len());
    for track in tracks {
        let destination = allocation
            .destinations
            .iter()
            .find(|destination| destination.track_id == track.track_id)
            .map(|destination| destination.destination)
            .ok_or_else(|| ApiError {
                status: StatusCode::BAD_GATEWAY,
                code: "live_worker_invalid_response",
                message: "media worker omitted a track destination".to_owned(),
            })?;
        let binding = RecordingBinding {
            room_id: track.room_id,
            track_id: track.track_id,
            destination,
        };
        let response = state
            .http
            .post(internal_url(&state.media_node_url, "/v1/sfu/recordings")?)
            .bearer_auth(state.media_node_token.as_ref())
            .json(&binding)
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => bindings.push(binding),
            Ok(response) => {
                rollback_live_bridge(state, &placement.endpoint, allocation.job_id, &bindings)
                    .await;
                return Err(ApiError {
                    status: StatusCode::BAD_GATEWAY,
                    code: "media_node_recording_rejected",
                    message: format!("media node returned {}", response.status()),
                });
            }
            Err(error) => {
                rollback_live_bridge(state, &placement.endpoint, allocation.job_id, &bindings)
                    .await;
                return Err(media_node_unavailable(error));
            }
        }
    }
    Ok((allocation.job_id, bindings))
}

async fn stop_live_rtp_bridge(state: &AppState, stream: &mut LiveStream) -> Result<(), ApiError> {
    if !stream.worker_active {
        return Ok(());
    }
    let Some(job_id) = stream.worker_job_id else {
        return Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "live_bridge_state_invalid",
            message: "active live bridge has no worker job".to_owned(),
        });
    };
    let worker_endpoint = stream
        .worker_endpoint
        .as_deref()
        .unwrap_or(state.worker_url.as_ref());
    for binding in &stream.recording_bindings {
        let response = state
            .http
            .delete(internal_url(&state.media_node_url, "/v1/sfu/recordings")?)
            .bearer_auth(state.media_node_token.as_ref())
            .json(binding)
            .send()
            .await
            .map_err(media_node_unavailable)?;
        if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(ApiError {
                status: StatusCode::BAD_GATEWAY,
                code: "media_node_recording_stop_failed",
                message: format!("media node returned {}", response.status()),
            });
        }
    }
    let response = state
        .http
        .delete(internal_url(
            worker_endpoint,
            &format!("/v1/live-jobs/{job_id}"),
        )?)
        .bearer_auth(state.worker_token.as_ref())
        .send()
        .await
        .map_err(worker_unavailable)?;
    if !response.status().is_success() && response.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "live_worker_stop_failed",
            message: format!("media worker returned {}", response.status()),
        });
    }
    stream.worker_active = false;
    stream.recording_bindings.clear();
    Ok(())
}

async fn rollback_live_bridge(
    state: &AppState,
    worker_endpoint: &str,
    job_id: u64,
    bindings: &[RecordingBinding],
) {
    for binding in bindings {
        best_effort_remove_recording_binding(state, binding).await;
    }
    best_effort_stop_live_worker(state, worker_endpoint, job_id).await;
}

async fn best_effort_remove_recording_binding(state: &AppState, binding: &RecordingBinding) {
    let url = match internal_url(&state.media_node_url, "/v1/sfu/recordings") {
        Ok(url) => url,
        Err(error) => {
            eprintln!(
                "refusing invalid media-node cleanup endpoint: {}",
                error.message
            );
            return;
        }
    };
    match state
        .http
        .delete(url)
        .bearer_auth(state.media_node_token.as_ref())
        .json(binding)
        .send()
        .await
    {
        Ok(response)
            if response.status().is_success()
                || response.status() == reqwest::StatusCode::NOT_FOUND => {}
        Ok(response) => eprintln!(
            "media-node recording cleanup was rejected with {} for track {}",
            response.status(),
            binding.track_id
        ),
        Err(error) => eprintln!(
            "media-node recording cleanup failed for track {}: {error}",
            binding.track_id
        ),
    }
}

async fn best_effort_stop_live_worker(state: &AppState, worker_endpoint: &str, job_id: u64) {
    let url = match internal_url(worker_endpoint, &format!("/v1/live-jobs/{job_id}")) {
        Ok(url) => url,
        Err(error) => {
            eprintln!(
                "refusing invalid live worker cleanup endpoint: {}",
                error.message
            );
            return;
        }
    };
    match state
        .http
        .delete(url)
        .bearer_auth(state.worker_token.as_ref())
        .send()
        .await
    {
        Ok(response)
            if response.status().is_success()
                || response.status() == reqwest::StatusCode::NOT_FOUND => {}
        Ok(response) => eprintln!(
            "live worker cleanup was rejected with {} for job {job_id}",
            response.status()
        ),
        Err(error) => eprintln!("live worker cleanup failed for job {job_id}: {error}"),
    }
}

async fn monitor_worker_job(
    state: AppState,
    asset_id: String,
    mut job_id: u64,
    mut worker_endpoint: String,
    mut generation: u64,
) {
    let mut misses = 0_u32;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Ok(url) = internal_url(&worker_endpoint, &format!("/v1/jobs/{job_id}")) else {
            record_vod_monitor_miss(
                &state,
                &asset_id,
                &mut job_id,
                &mut worker_endpoint,
                &mut generation,
                &mut misses,
            )
            .await;
            continue;
        };
        let response = state
            .http
            .get(url)
            .bearer_auth(state.worker_token.as_ref())
            .send()
            .await;
        let Ok(response) = response else {
            record_vod_monitor_miss(
                &state,
                &asset_id,
                &mut job_id,
                &mut worker_endpoint,
                &mut generation,
                &mut misses,
            )
            .await;
            continue;
        };
        if !response.status().is_success() {
            record_vod_monitor_miss(
                &state,
                &asset_id,
                &mut job_id,
                &mut worker_endpoint,
                &mut generation,
                &mut misses,
            )
            .await;
            continue;
        }
        let Ok(job) = bounded_json::<WorkerJob>(response, "worker_invalid_response").await else {
            record_vod_monitor_miss(
                &state,
                &asset_id,
                &mut job_id,
                &mut worker_endpoint,
                &mut generation,
                &mut misses,
            )
            .await;
            continue;
        };
        misses = 0;
        match job.state.as_str() {
            "succeeded" => {
                finalize_vod_success(&state, &asset_id, generation).await;
                return;
            }
            "failed" => {
                mark_asset_failed(
                    &state,
                    &asset_id,
                    job.reason.as_deref().unwrap_or("media worker failed"),
                )
                .await;
                release_worker_placement(&state, "vod_transcode", &asset_id).await;
                return;
            }
            _ => {}
        }
    }
}

async fn finalize_vod_success(state: &AppState, asset_id: &str, generation: u64) {
    let output_directory = state
        .output_root
        .join(vod_output_directory(asset_id, generation));
    let playlist_path = output_directory.join("master.m3u8");
    let duration = tokio::fs::read_to_string(playlist_path)
        .await
        .ok()
        .and_then(|playlist| playlist_duration_millis(&playlist))
        .unwrap_or(1);
    let publication = state
        .media_store
        .publish_directory(
            output_directory,
            &format!("vod/{asset_id}"),
            PublishLimits {
                max_object_bytes: MAX_MEDIA_OBJECT_BYTES,
                ..PublishLimits::default()
            },
        )
        .await;
    if let Err(error) = publication {
        mark_asset_failed(
            state,
            asset_id,
            &format!("object publication failed: {error}"),
        )
        .await;
        release_worker_placement(state, "vod_transcode", asset_id).await;
        return;
    }
    let mut assets = state.assets.lock().await;
    if let Some(asset) = assets.get_mut(asset_id) {
        if let Err(error) = asset.asset.mark_ready("master.m3u8", duration) {
            eprintln!("failed to finalize VOD asset {asset_id}: {error}");
            drop(assets);
            release_worker_placement(state, "vod_transcode", asset_id).await;
            return;
        }
        let Some(revision) = asset.revision.checked_add(1) else {
            eprintln!("failed to finalize VOD asset {asset_id}: metadata revision exhausted");
            drop(assets);
            release_worker_placement(state, "vod_transcode", asset_id).await;
            return;
        };
        asset.revision = revision;
        asset.updated_at_millis = now_millis();
        if let Err(error) = persist_asset(state, asset).await {
            eprintln!(
                "failed to persist completed VOD asset {asset_id}: {}",
                error.message
            );
        }
    }
    drop(assets);
    release_worker_placement(state, "vod_transcode", asset_id).await;
}

async fn record_vod_monitor_miss(
    state: &AppState,
    asset_id: &str,
    job_id: &mut u64,
    worker_endpoint: &mut String,
    generation: &mut u64,
    misses: &mut u32,
) {
    *misses = misses.saturating_add(1);
    maybe_restart_vod_job(state, asset_id, job_id, worker_endpoint, generation, misses).await;
}

async fn maybe_restart_vod_job(
    state: &AppState,
    asset_id: &str,
    job_id: &mut u64,
    worker_endpoint: &mut String,
    generation: &mut u64,
    misses: &mut u32,
) {
    if *misses < 10 || !(*misses - 10).is_multiple_of(5) {
        return;
    }
    match restart_vod_job(state, asset_id).await {
        Ok((replacement_job_id, placement)) => {
            *job_id = replacement_job_id;
            *worker_endpoint = placement.endpoint;
            *generation = placement.generation;
            *misses = 0;
        }
        Err(error) => {
            eprintln!("VOD failover for {asset_id} failed: {}", error.message);
        }
    }
}

async fn restart_vod_job(
    state: &AppState,
    asset_id: &str,
) -> Result<(u64, ServicePlacement), ApiError> {
    let (job_spec, current_generation) = {
        let assets = state.assets.lock().await;
        let managed = assets.get(asset_id).ok_or_else(asset_not_found)?;
        if !matches!(managed.asset.state, AssetState::Transcoding { .. }) {
            return Err(conflict("VOD asset is no longer transcoding"));
        }
        (
            managed
                .job_spec
                .clone()
                .ok_or_else(|| conflict("VOD failover metadata is unavailable"))?,
            managed.placement_generation.unwrap_or(1),
        )
    };
    let placement = if state.control_store.is_some() {
        place_worker(state, "vod_transcode", asset_id, true).await?
    } else {
        ServicePlacement {
            resource_kind: "vod_transcode".to_owned(),
            resource_id: asset_id.to_owned(),
            node_id: "static-worker".to_owned(),
            endpoint: state.worker_url.to_string(),
            generation: current_generation
                .checked_add(1)
                .ok_or_else(revision_exhausted)?,
        }
    };
    let output_directory = vod_output_directory(asset_id, placement.generation);
    tokio::fs::create_dir_all(state.output_root.join(&output_directory))
        .await
        .map_err(io_error)?;
    let response = state
        .http
        .post(internal_url(&placement.endpoint, "/v1/jobs")?)
        .bearer_auth(state.worker_token.as_ref())
        .json(&WorkerJobRequest {
            asset_id: asset_id.to_owned(),
            input: format!("{asset_id}/source.bin"),
            output_directory,
            segment_duration_millis: job_spec.segment_duration_millis,
            renditions: job_spec.renditions,
            placement_resource_id: asset_id.to_owned(),
            placement_generation: placement.generation,
        })
        .send()
        .await
        .map_err(worker_unavailable)?;
    if !response.status().is_success() {
        return Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "worker_failover_rejected",
            message: format!("replacement media worker returned {}", response.status()),
        });
    }
    let response = bounded_json::<WorkerJobResponse>(response, "worker_invalid_response").await?;
    let mut assets = state.assets.lock().await;
    let managed = assets.get_mut(asset_id).ok_or_else(asset_not_found)?;
    managed.job_id = Some(response.job_id);
    managed.worker_endpoint = Some(placement.endpoint.clone());
    managed.placement_generation = Some(placement.generation);
    managed.updated_at_millis = now_millis();
    managed.revision = managed
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    persist_asset(state, managed).await?;
    Ok((response.job_id, placement))
}

fn vod_output_directory(asset_id: &str, generation: u64) -> String {
    if generation == 0 {
        asset_id.to_owned()
    } else {
        format!("{asset_id}/generation-{generation}")
    }
}

async fn mark_asset_failed(state: &AppState, asset_id: &str, reason: &str) {
    let mut assets = state.assets.lock().await;
    if let Some(asset) = assets.get_mut(asset_id) {
        if let Err(error) = asset
            .asset
            .fail(reason.chars().take(1_024).collect::<String>(), true)
        {
            eprintln!("failed to mark VOD asset {asset_id} failed: {error}");
            return;
        }
        let Some(revision) = asset.revision.checked_add(1) else {
            eprintln!("failed to mark VOD asset {asset_id} failed: metadata revision exhausted");
            return;
        };
        asset.revision = revision;
        asset.updated_at_millis = now_millis();
        if let Err(error) = persist_asset(state, asset).await {
            eprintln!(
                "failed to persist failed VOD asset {asset_id}: {}",
                error.message
            );
        }
    }
}

async fn resume_worker_monitors(state: &AppState) {
    let jobs = {
        let assets = state.assets.lock().await;
        assets
            .iter()
            .filter_map(|(asset_id, managed)| {
                (matches!(
                    managed.asset.state,
                    AssetState::Probing | AssetState::Transcoding { .. }
                ))
                .then_some(managed.job_id)
                .flatten()
                .map(|job_id| {
                    let generation = managed.placement_generation.unwrap_or(0);
                    (
                        asset_id.clone(),
                        job_id,
                        managed
                            .worker_endpoint
                            .clone()
                            .unwrap_or_else(|| state.worker_url.to_string()),
                        generation,
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    for (asset_id, job_id, worker_endpoint, generation) in jobs {
        let monitor_state = state.clone();
        tokio::spawn(async move {
            monitor_worker_job(monitor_state, asset_id, job_id, worker_endpoint, generation).await;
        });
    }
    let live_streams = {
        let streams = state.live_streams.lock().await;
        streams
            .iter()
            .filter(|(_, stream)| stream.worker_active)
            .map(|(stream_id, _)| stream_id.clone())
            .collect::<Vec<_>>()
    };
    for stream_id in live_streams {
        spawn_live_publisher(state.clone(), stream_id);
    }
}

fn spawn_retention_task(state: AppState) {
    if state.vod_retention.is_none() && state.live_retention.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(state.retention_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            run_retention_pass(&state).await;
        }
    });
}

async fn run_retention_pass(state: &AppState) {
    let now = now_millis();
    if let Some(retention) = state.vod_retention {
        let retention_millis = u64::try_from(retention.as_millis()).unwrap_or(u64::MAX);
        let candidates = {
            let assets = state.assets.lock().await;
            assets
                .iter()
                .filter(|(_, managed)| {
                    !matches!(
                        managed.asset.state,
                        AssetState::Probing | AssetState::Transcoding { .. } | AssetState::Deleted
                    ) && now.saturating_sub(managed.updated_at_millis) >= retention_millis
                })
                .map(|(asset_id, _)| asset_id.clone())
                .collect::<Vec<_>>()
        };
        for asset_id in candidates {
            if let Err(error) = delete_asset_storage(state, &asset_id).await {
                eprintln!(
                    "VOD retention cleanup for {asset_id} failed: {}",
                    error.message
                );
            }
        }
    }
    if let Some(retention) = state.live_retention {
        let retention_millis = u64::try_from(retention.as_millis()).unwrap_or(u64::MAX);
        let candidates = {
            let streams = state.live_streams.lock().await;
            streams
                .iter()
                .filter(|(_, stream)| {
                    stream.purged_at_millis.is_none()
                        && stream.finished_at_millis.is_some_and(|finished| {
                            now.saturating_sub(finished) >= retention_millis
                        })
                })
                .map(|(stream_id, _)| stream_id.clone())
                .collect::<Vec<_>>()
        };
        for stream_id in candidates {
            if let Err(error) = delete_live_storage(state, &stream_id).await {
                eprintln!(
                    "live retention cleanup for {stream_id} failed: {}",
                    error.message
                );
            }
        }
    }
}

fn spawn_live_publisher(state: AppState, stream_id: String) {
    tokio::spawn(async move {
        let mut worker_misses = 0_u32;
        loop {
            let (active, deleted) = {
                let streams = state.live_streams.lock().await;
                let Some(stream) = streams.get(&stream_id) else {
                    return;
                };
                (stream.worker_active, stream.deleted_at_millis.is_some())
            };
            if deleted {
                return;
            }
            if !active {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let deleted = {
                    let streams = state.live_streams.lock().await;
                    streams
                        .get(&stream_id)
                        .is_none_or(|stream| stream.deleted_at_millis.is_some())
                };
                if deleted {
                    return;
                }
                if let Err(error) = publish_live_snapshot(&state, &stream_id).await {
                    eprintln!(
                        "final live publication for {stream_id} failed: {}",
                        error.message
                    );
                }
                return;
            }
            if live_worker_operational(&state, &stream_id).await {
                worker_misses = 0;
            } else {
                worker_misses = worker_misses.saturating_add(1);
                if worker_misses >= 5 {
                    match restart_live_job(&state, &stream_id).await {
                        Ok(()) => worker_misses = 0,
                        Err(error) => {
                            eprintln!("live failover for {stream_id} failed: {}", error.message);
                        }
                    }
                }
            }
            if let Err(error) = publish_live_snapshot(&state, &stream_id).await {
                eprintln!("live publication for {stream_id} failed: {}", error.message);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn live_worker_operational(state: &AppState, stream_id: &str) -> bool {
    let (worker_endpoint, job_id) = {
        let streams = state.live_streams.lock().await;
        let Some(stream) = streams.get(stream_id) else {
            return false;
        };
        let Some(job_id) = stream.worker_job_id else {
            return true;
        };
        (
            stream
                .worker_endpoint
                .clone()
                .unwrap_or_else(|| state.worker_url.to_string()),
            job_id,
        )
    };
    let Ok(url) = internal_url(&worker_endpoint, &format!("/v1/jobs/{job_id}")) else {
        return false;
    };
    let Ok(response) = state
        .http
        .get(url)
        .bearer_auth(state.worker_token.as_ref())
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    bounded_json::<WorkerJob>(response, "worker_invalid_response")
        .await
        .is_ok_and(|job| matches!(job.state.as_str(), "queued" | "running"))
}

async fn restart_live_job(state: &AppState, stream_id: &str) -> Result<(), ApiError> {
    let (spec, current_generation, old_endpoint, old_job_id, old_bindings) = {
        let streams = state.live_streams.lock().await;
        let stream = streams.get(stream_id).ok_or_else(stream_not_found)?;
        if !stream.worker_active || stream.deleted_at_millis.is_some() {
            return Err(conflict("live stream is no longer active"));
        }
        (
            stream
                .job_spec
                .clone()
                .ok_or_else(|| conflict("live failover metadata is unavailable"))?,
            stream.placement_generation.unwrap_or(1),
            stream
                .worker_endpoint
                .clone()
                .unwrap_or_else(|| state.worker_url.to_string()),
            stream.worker_job_id,
            stream.recording_bindings.clone(),
        )
    };
    let placement = if state.control_store.is_some() {
        place_worker(state, "live_package", stream_id, true).await?
    } else {
        ServicePlacement {
            resource_kind: "live_package".to_owned(),
            resource_id: stream_id.to_owned(),
            node_id: "static-worker".to_owned(),
            endpoint: state.worker_url.to_string(),
            generation: current_generation
                .checked_add(1)
                .ok_or_else(revision_exhausted)?,
        }
    };
    let (job_id, bindings) = start_live_rtp_bridge(
        state,
        stream_id,
        spec.segment_duration_millis,
        spec.window_segments,
        spec.tracks,
        spec.renditions,
        &placement,
    )
    .await?;
    remove_recording_bindings(state, &old_bindings).await;
    if let Some(old_job_id) = old_job_id {
        best_effort_stop_live_worker(state, &old_endpoint, old_job_id).await;
    }
    let mut streams = state.live_streams.lock().await;
    let stream = streams.get_mut(stream_id).ok_or_else(stream_not_found)?;
    if !stream.worker_active || stream.deleted_at_millis.is_some() {
        drop(streams);
        rollback_live_bridge(state, &placement.endpoint, job_id, &bindings).await;
        return Err(conflict("live stream stopped during worker failover"));
    }
    stream.worker_job_id = Some(job_id);
    stream.worker_endpoint = Some(placement.endpoint);
    stream.placement_generation = Some(placement.generation);
    stream.recording_bindings = bindings;
    stream.updated_at_millis = now_millis();
    stream.revision = stream
        .revision
        .checked_add(1)
        .ok_or_else(revision_exhausted)?;
    persist_live_stream(state, stream_id, stream).await
}

async fn remove_recording_bindings(state: &AppState, bindings: &[RecordingBinding]) {
    for binding in bindings {
        best_effort_remove_recording_binding(state, binding).await;
    }
}

async fn publish_live_snapshot(state: &AppState, stream_id: &str) -> Result<(), ApiError> {
    let local_directory = {
        let streams = state.live_streams.lock().await;
        let stream = streams.get(stream_id).ok_or_else(stream_not_found)?;
        match (stream.worker_job_id, stream.placement_generation) {
            (Some(_), Some(generation)) => state
                .live_root
                .join(live_output_directory(stream_id, generation)),
            _ => state.live_root.join(stream_id),
        }
    };
    state
        .media_store
        .sync_hls_directory(
            local_directory,
            &format!("live/{stream_id}"),
            PublishLimits {
                max_objects: 1_024,
                max_object_bytes: MAX_MEDIA_OBJECT_BYTES,
                max_total_bytes: 8 * 1_024 * 1_024 * 1_024,
            },
        )
        .await
        .map(|_| ())
        .map_err(store_error)
}

fn live_output_directory(stream_id: &str, generation: u64) -> String {
    format!("{stream_id}/generation-{generation}")
}

async fn persist_live_playlist_object(
    state: &AppState,
    stream_id: &str,
    stream: &LiveStream,
) -> Result<(), ApiError> {
    if stream.worker_job_id.is_some() {
        return Ok(());
    }
    state
        .media_store
        .put(
            &format!("live/{stream_id}/index.m3u8"),
            Bytes::from(stream.playlist.render()),
        )
        .await
        .map(|_| ())
        .map_err(store_error)
}

async fn remove_namespace_directory(root: &FilePath, identifier: &str) -> Result<(), ApiError> {
    let target = root.join(identifier);
    let target = match tokio::fs::canonicalize(&target).await {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if target == root || !target.starts_with(root) {
        return Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "unsafe_storage_path",
            message: "refused to delete a path outside its media namespace".to_owned(),
        });
    }
    tokio::fs::remove_dir_all(target).await.map_err(io_error)
}

fn playlist_duration_millis(playlist: &str) -> Option<u64> {
    let seconds = playlist
        .lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .filter_map(|value| value.trim_end_matches(',').parse::<f64>().ok())
        .sum::<f64>();
    Duration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn asset_response(managed: &ManagedAsset, public_base_url: &str) -> AssetResponse {
    let (
        state,
        received_bytes,
        source_bytes,
        manifest_url,
        duration_millis,
        failure_reason,
        retryable,
    ) = match &managed.asset.state {
        AssetState::Created => ("created", None, None, None, None, None, None),
        AssetState::Uploading { received_bytes } => (
            "uploading",
            Some(*received_bytes),
            None,
            None,
            None,
            None,
            None,
        ),
        AssetState::Uploaded { source_bytes } => (
            "uploaded",
            None,
            Some(*source_bytes),
            None,
            None,
            None,
            None,
        ),
        AssetState::Probing => ("probing", None, None, None, None, None, None),
        AssetState::Transcoding { .. } => ("transcoding", None, None, None, None, None, None),
        AssetState::Ready {
            manifest_uri,
            duration_millis,
        } => (
            "ready",
            None,
            None,
            Some(format!(
                "{public_base_url}/media/vod/{}/{}",
                managed.asset.id, manifest_uri
            )),
            Some(*duration_millis),
            None,
            None,
        ),
        AssetState::Failed { reason, retryable } => (
            "failed",
            None,
            None,
            None,
            None,
            Some(reason.clone()),
            Some(*retryable),
        ),
        AssetState::Deleting => ("deleting", None, None, None, None, None, None),
        AssetState::Deleted => ("deleted", None, None, None, None, None, None),
    };
    AssetResponse {
        asset_id: managed.asset.id.clone(),
        tenant_id: managed.asset.tenant_id.clone(),
        version: managed.asset.version,
        state,
        received_bytes,
        source_bytes,
        manifest_url,
        duration_millis,
        failure_reason,
        retryable,
        job_id: managed.job_id,
        created_at_millis: managed.created_at_millis,
        updated_at_millis: managed.updated_at_millis,
    }
}

fn live_response(stream_id: &str, stream: &LiveStream, public_base_url: &str) -> LiveResponse {
    let manifest = live_manifest_name(stream.job_spec.as_ref());
    LiveResponse {
        stream_id: stream_id.to_owned(),
        next_sequence: stream.playlist.next_sequence(),
        manifest_url: format!("{public_base_url}/media/live/{stream_id}/{manifest}"),
        worker_job_id: stream.worker_job_id,
        finished_at_millis: stream.finished_at_millis,
    }
}

fn live_manifest_name(job_spec: Option<&LiveJobSpec>) -> &'static str {
    if job_spec.is_some_and(|spec| !spec.renditions.is_empty()) {
        "master.m3u8"
    } else {
        "index.m3u8"
    }
}

fn parse_range(value: &str, length: u64) -> Result<(u64, u64), ApiError> {
    if length == 0 {
        return Err(range_not_satisfiable());
    }
    let range = value
        .strip_prefix("bytes=")
        .and_then(|value| (!value.contains(',')).then_some(value))
        .ok_or_else(range_not_satisfiable)?;
    let (start, end) = range.split_once('-').ok_or_else(range_not_satisfiable)?;
    if start.is_empty() {
        let suffix = end
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(range_not_satisfiable)?;
        let start = length.saturating_sub(suffix.min(length));
        return Ok((start, length.saturating_sub(1)));
    }
    let start = start
        .parse::<u64>()
        .ok()
        .filter(|start| *start < length)
        .ok_or_else(range_not_satisfiable)?;
    let end = if end.is_empty() {
        length.saturating_sub(1)
    } else {
        end.parse::<u64>()
            .ok()
            .map(|end| end.min(length.saturating_sub(1)))
            .filter(|end| *end >= start)
            .ok_or_else(range_not_satisfiable)?
    };
    Ok((start, end))
}

fn content_type(object: &str) -> &'static str {
    match FilePath::new(object)
        .extension()
        .and_then(|value| value.to_str())
    {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("m4s" | "mp4") => "video/mp4",
        Some("aac") => "audio/aac",
        Some("vtt") => "text/vtt; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn validate_identifier(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(unprocessable(
            "identifier must be 1..=128 safe ASCII characters",
        ))
    } else {
        Ok(())
    }
}

fn validate_object_path(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 1_024
        || FilePath::new(value)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        Err(media_not_found())
    } else {
        Ok(())
    }
}

fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied == Some(expected) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "invalid gateway bearer token".to_owned(),
        })
    }
}

async fn create_canonical_directory(path: PathBuf) -> Result<PathBuf, std::io::Error> {
    tokio::fs::create_dir_all(&path).await?;
    tokio::fs::canonicalize(path).await
}

const fn default_segment_duration() -> u32 {
    4_000
}

const fn default_live_window() -> usize {
    6
}

fn retention_from_env(name: &str) -> Result<Option<Duration>, String> {
    let Some(value) = env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let hours = value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be a non-negative integer"))?;
    if hours == 0 {
        return Ok(None);
    }
    hours
        .checked_mul(60 * 60)
        .map(Duration::from_secs)
        .map(Some)
        .ok_or_else(|| format!("{name} is too large"))
}

fn retention_interval_from_env() -> Result<Duration, String> {
    let seconds = env::var("FLUVORA_RETENTION_INTERVAL_SECONDS")
        .unwrap_or_else(|_| "900".to_owned())
        .parse::<u64>()
        .map_err(|_| "FLUVORA_RETENTION_INTERVAL_SECONDS must be an integer".to_owned())?;
    if seconds < 60 {
        return Err("FLUVORA_RETENTION_INTERVAL_SECONDS must be at least 60".to_owned());
    }
    Ok(Duration::from_secs(seconds))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn conflict(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "state_conflict",
        message: message.to_owned(),
    }
}

fn unprocessable(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_request",
        message: message.to_owned(),
    }
}

fn asset_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "asset_not_found",
        message: "unknown VOD asset".to_owned(),
    }
}

fn stream_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "stream_not_found",
        message: "unknown live stream".to_owned(),
    }
}

fn media_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "media_not_found",
        message: "media object not found".to_owned(),
    }
}

fn range_not_satisfiable() -> ApiError {
    ApiError {
        status: StatusCode::RANGE_NOT_SATISFIABLE,
        code: "invalid_range",
        message: "requested byte range is not satisfiable".to_owned(),
    }
}

fn payload_too_large(limit: usize) -> ApiError {
    ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "payload_too_large",
        message: format!("request body exceeds {limit} bytes"),
    }
}

fn revision_exhausted() -> ApiError {
    ApiError {
        status: StatusCode::INSUFFICIENT_STORAGE,
        code: "metadata_revision_exhausted",
        message: "media metadata revision exhausted".to_owned(),
    }
}

fn pipeline_error(error: &fluvora_media_pipeline::PipelineError) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "media_pipeline_error",
        message: error.to_string(),
    }
}

fn worker_unavailable(error: reqwest::Error) -> ApiError {
    eprintln!("media worker request failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "worker_unavailable",
        message: "media worker is unavailable".to_owned(),
    }
}

fn media_node_unavailable(error: reqwest::Error) -> ApiError {
    eprintln!("media-node request failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "media_node_unavailable",
        message: "media node is unavailable".to_owned(),
    }
}

fn io_error(error: std::io::Error) -> ApiError {
    eprintln!("media storage I/O operation failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "storage_error",
        message: "media storage operation failed".to_owned(),
    }
}

fn store_error(error: StoreError) -> ApiError {
    if error.is_not_found() {
        return media_not_found();
    }
    eprintln!("media object store operation failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "object_store_unavailable",
        message: "media object store is unavailable".to_owned(),
    }
}

fn control_store_error(error: fluvora_control_store::StoreError) -> ApiError {
    eprintln!("media gateway control-store operation failed: {error}");
    drop(error);
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "control_store_unavailable",
        message: "control store is unavailable".to_owned(),
    }
}

fn internal_error(error: impl std::fmt::Display) -> ApiError {
    eprintln!("media gateway internal operation failed: {error}");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: "internal media gateway operation failed".to_owned(),
    }
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

#[cfg(test)]
mod tests {
    use super::{
        LiveJobSpec, RenditionRequest, content_type, live_manifest_name, parse_range,
        playlist_duration_millis,
    };

    #[test]
    fn parses_http_ranges_and_playlist_duration() {
        assert_eq!(parse_range("bytes=2-4", 10).expect("range"), (2, 4));
        assert_eq!(parse_range("bytes=-3", 10).expect("suffix"), (7, 9));
        assert!(parse_range("bytes=20-", 10).is_err());
        assert_eq!(
            playlist_duration_millis("#EXTINF:2.500,\na.m4s\n#EXTINF:1.250,\nb.m4s\n"),
            Some(3_750)
        );
        assert_eq!(content_type("segment.m4s"), "video/mp4");
    }

    #[test]
    fn selects_master_manifest_only_for_worker_backed_abr() {
        assert_eq!(live_manifest_name(None), "index.m3u8");
        let mut spec = LiveJobSpec {
            segment_duration_millis: 1_000,
            window_segments: 3,
            tracks: Vec::new(),
            renditions: Vec::new(),
        };
        assert_eq!(live_manifest_name(Some(&spec)), "index.m3u8");
        spec.renditions.push(RenditionRequest {
            width: 320,
            height: 180,
            video_bitrate_bps: 300_000,
            audio_bitrate_bps: 32_000,
        });
        assert_eq!(live_manifest_name(Some(&spec)), "master.m3u8");
    }
}
