use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use fluvora_media_pipeline::{
    LivePackageSpec, RealtimeCodec, RealtimeTranscodeSpec, Rendition, WorkerOperation,
    build_live_rtp_process, build_realtime_transcode_process, build_worker_process,
};
use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

mod assignment_state;
mod cleanup;

use assignment_state::{load_assignment_registry, persist_fence_snapshot};
use cleanup::remove_temporary_file;

const MAX_JOBS_RETAINED: usize = 10_000;
const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1_024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum JobState {
    Queued,
    Running,
    Succeeded {
        finished_at_millis: u64,
    },
    Failed {
        finished_at_millis: u64,
        reason: String,
    },
    Stopped {
        finished_at_millis: u64,
    },
}

enum LiveCompletion {
    Process(Result<std::process::ExitStatus, std::io::Error>),
    Stop,
}

struct LiveJobRun {
    process: fluvora_media_pipeline::ProcessSpec,
    sdp_path: PathBuf,
    reservations: Vec<UdpSocket>,
    permit: OwnedSemaphorePermit,
    cancellation: oneshot::Receiver<()>,
    readiness: Option<oneshot::Sender<Result<(), String>>>,
}

#[derive(Debug, Clone, Serialize)]
struct Job {
    id: u64,
    asset_id: String,
    created_at_millis: u64,
    #[serde(flatten)]
    state: JobState,
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    asset_id: String,
    input: String,
    output_directory: String,
    segment_duration_millis: u32,
    renditions: Vec<RenditionRequest>,
    #[serde(default)]
    placement_resource_id: Option<String>,
    #[serde(default)]
    placement_generation: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RenditionRequest {
    width: u16,
    height: u16,
    video_bitrate_bps: u64,
    audio_bitrate_bps: u32,
}

#[derive(Debug, Deserialize)]
struct CreateLiveJobRequest {
    stream_id: String,
    output_directory: String,
    segment_duration_millis: u32,
    window_segments: usize,
    tracks: Vec<LiveTrackRequest>,
    #[serde(default)]
    renditions: Vec<RenditionRequest>,
    #[serde(default)]
    placement_resource_id: Option<String>,
    #[serde(default)]
    placement_generation: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CreateRealtimeJobRequest {
    job_key: String,
    #[serde(default)]
    placement_resource_id: Option<String>,
    #[serde(default)]
    placement_generation: Option<u64>,
    source: LiveTrackRequest,
    target: RealtimeTargetRequest,
}

#[derive(Debug, Deserialize)]
struct RealtimeTargetRequest {
    codec: String,
    destination: SocketAddr,
    payload_type: u8,
    ssrc: u32,
    width: u16,
    height: u16,
    frames_per_second: u16,
    bitrate_bps: u64,
}

#[derive(Debug, Deserialize)]
struct LiveTrackRequest {
    track_id: u64,
    kind: String,
    codec: String,
    payload_type: u8,
    clock_rate: u32,
    channels: Option<u8>,
    fmtp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CreateLiveJobResponse {
    job_id: u64,
    destinations: Vec<LiveDestination>,
}

#[derive(Debug, Clone, Serialize)]
struct CreateRealtimeJobResponse {
    job_id: u64,
    source_destination: SocketAddr,
}

#[derive(Debug, Clone)]
struct WorkerAssignment {
    generation: u64,
    job_key: String,
    response: AssignmentResponse,
}

#[derive(Debug)]
enum AssignmentOutcome {
    Accepted { replaced_job_id: Option<u64> },
    Duplicate(AssignmentResponse),
}

#[derive(Debug, Clone)]
enum AssignmentResponse {
    Vod(CreateJobResponse),
    Live(CreateLiveJobResponse),
    Realtime(CreateRealtimeJobResponse),
}

impl AssignmentResponse {
    const fn job_id(&self) -> u64 {
        match self {
            Self::Vod(response) => response.job_id,
            Self::Live(response) => response.job_id,
            Self::Realtime(response) => response.job_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FenceRecord {
    generation: u64,
    job_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FenceSnapshot {
    revision: u64,
    records: HashMap<String, FenceRecord>,
}

#[derive(Debug)]
struct AssignmentRegistry {
    snapshot: FenceSnapshot,
    active: HashMap<String, WorkerAssignment>,
    directory: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct LiveDestination {
    track_id: u64,
    destination: SocketAddr,
}

#[derive(Debug, Clone, Serialize)]
struct CreateJobResponse {
    job_id: u64,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    running_or_queued: usize,
    concurrency_limit: usize,
}

#[derive(Debug, Clone, Copy)]
struct InputStreams {
    has_audio: bool,
}

#[derive(Clone)]
struct AppState {
    jobs: Arc<RwLock<HashMap<u64, Job>>>,
    next_job_id: Arc<AtomicU64>,
    permits: Arc<Semaphore>,
    concurrency_limit: usize,
    token: Arc<str>,
    ffmpeg: Arc<PathBuf>,
    ffprobe: Arc<PathBuf>,
    input_root: Arc<PathBuf>,
    output_root: Arc<PathBuf>,
    live_output_root: Arc<PathBuf>,
    cancellations: Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>,
    assignments: Arc<Mutex<AssignmentRegistry>>,
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

#[tokio::main]
async fn main() {
    let bind = env::var("FLUVORA_WORKER_BIND").unwrap_or_else(|_| "127.0.0.1:8091".to_owned());
    let address: SocketAddr = bind.parse().expect("FLUVORA_WORKER_BIND must be host:port");
    let token = env::var("FLUVORA_WORKER_TOKEN")
        .expect("FLUVORA_WORKER_TOKEN is required for internal authentication");
    assert!(
        (16..=4_096).contains(&token.len()) && !token.bytes().any(|byte| byte.is_ascii_control()),
        "FLUVORA_WORKER_TOKEN must contain 16..=4096 non-control bytes"
    );
    let concurrency_limit = env::var("FLUVORA_WORKER_CONCURRENCY")
        .map_or(Ok(2), |value| value.parse::<usize>())
        .expect("FLUVORA_WORKER_CONCURRENCY must be an integer");
    assert!(
        (1..=128).contains(&concurrency_limit),
        "FLUVORA_WORKER_CONCURRENCY must be 1..=128"
    );
    let input_root = canonical_directory_from_env("FLUVORA_WORKER_INPUT_ROOT", "./data/input")
        .await
        .expect("valid input root");
    let output_root = canonical_directory_from_env("FLUVORA_WORKER_OUTPUT_ROOT", "./data/output")
        .await
        .expect("valid output root");
    let live_output_root = canonical_directory_from_env("FLUVORA_WORKER_LIVE_ROOT", "./data/live")
        .await
        .expect("valid live output root");
    let node_id = env::var("FLUVORA_NODE_ID").unwrap_or_else(|_| "worker-local".to_owned());
    validate_identifier(&node_id).expect("FLUVORA_NODE_ID must be a safe identifier");
    let assignment_root = PathBuf::from(
        env::var("FLUVORA_WORKER_STATE_ROOT").unwrap_or_else(|_| "./data/worker-state".to_owned()),
    )
    .join(node_id);
    tokio::fs::create_dir_all(&assignment_root)
        .await
        .expect("worker assignment state directory");
    let assignment_root = tokio::fs::canonicalize(assignment_root)
        .await
        .expect("canonical worker assignment state directory");
    let assignments =
        load_assignment_registry(assignment_root).expect("valid worker assignment fence snapshots");
    let ffmpeg = resolve_executable(PathBuf::from(
        env::var("FLUVORA_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned()),
    ))
    .expect("FLUVORA_FFMPEG must be a valid executable path or PATH command");
    let ffprobe = resolve_executable(PathBuf::from(
        env::var("FLUVORA_FFPROBE").unwrap_or_else(|_| "ffprobe".to_owned()),
    ))
    .expect("FLUVORA_FFPROBE must be a valid executable path or PATH command");
    let state = AppState {
        jobs: Arc::new(RwLock::new(HashMap::new())),
        next_job_id: Arc::new(AtomicU64::new(job_id_seed())),
        permits: Arc::new(Semaphore::new(concurrency_limit)),
        concurrency_limit,
        token: Arc::from(token),
        ffmpeg: Arc::new(ffmpeg),
        ffprobe: Arc::new(ffprobe),
        input_root: Arc::new(input_root),
        output_root: Arc::new(output_root),
        live_output_root: Arc::new(live_output_root),
        cancellations: Arc::new(Mutex::new(HashMap::new())),
        assignments: Arc::new(Mutex::new(assignments)),
    };
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/jobs", post(create_job))
        .route("/v1/jobs/{job_id}", get(get_job).delete(stop_live_job))
        .route("/v1/live-jobs", post(create_live_job))
        .route("/v1/live-jobs/{job_id}", delete(stop_live_job))
        .route("/v1/realtime-jobs", post(create_realtime_job))
        .route("/v1/realtime-jobs/{job_id}", delete(stop_live_job))
        .with_state(state.clone());
    let (heartbeat, heartbeat_task) = start_worker_heartbeat(state.clone());
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("worker listener bind");
    println!(
        "{} media worker listening on {address}",
        fluvora_domain::PLATFORM_NAME
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("worker server");
    stop_worker_heartbeat(heartbeat.as_ref(), heartbeat_task, &state).await;
}

fn start_worker_heartbeat(
    state: AppState,
) -> (Option<HeartbeatClient>, Option<tokio::task::JoinHandle<()>>) {
    let client = HeartbeatClient::from_env(ServiceKind::MediaWorker)
        .expect("valid status heartbeat configuration");
    let task = client.as_ref().map(|client| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .run(|| std::future::ready(worker_capacity(&state)))
                .await;
        })
    });
    (client, task)
}

async fn stop_worker_heartbeat(
    client: Option<&HeartbeatClient>,
    task: Option<tokio::task::JoinHandle<()>>,
    state: &AppState,
) {
    if let Some(client) = client {
        client.mark_draining();
        if let Err(error) = client.report(worker_capacity(state), true).await {
            eprintln!("failed to report draining worker heartbeat: {error}");
        }
    }
    if let Some(task) = task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            eprintln!("worker heartbeat task failed during shutdown: {error}");
        }
    }
}

fn worker_capacity(state: &AppState) -> NodeCapacity {
    NodeCapacity {
        jobs_limit: u64::try_from(state.concurrency_limit).unwrap_or(u64::MAX),
        jobs_used: u64::try_from(state.concurrency_limit - state.permits.available_permits())
            .unwrap_or(u64::MAX),
        memory_bytes: process_memory_bytes(),
        ..NodeCapacity::default()
    }
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        running_or_queued: state.concurrency_limit - state.permits.available_permits(),
        concurrency_limit: state.concurrency_limit,
    })
}

async fn metrics(State(state): State<AppState>) -> String {
    let jobs = state
        .jobs
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut queued = 0_u64;
    let mut running = 0_u64;
    let mut succeeded = 0_u64;
    let mut failed = 0_u64;
    let mut stopped = 0_u64;
    for job in jobs.values() {
        match job.state {
            JobState::Queued => queued += 1,
            JobState::Running => running += 1,
            JobState::Succeeded { .. } => succeeded += 1,
            JobState::Failed { .. } => failed += 1,
            JobState::Stopped { .. } => stopped += 1,
        }
    }
    format!(
        "# HELP fluvora_worker_jobs Retained worker jobs by lifecycle state.\n\
         # TYPE fluvora_worker_jobs gauge\n\
         fluvora_worker_jobs{{state=\"queued\"}} {queued}\n\
         fluvora_worker_jobs{{state=\"running\"}} {running}\n\
         fluvora_worker_jobs{{state=\"succeeded\"}} {succeeded}\n\
         fluvora_worker_jobs{{state=\"failed\"}} {failed}\n\
         fluvora_worker_jobs{{state=\"stopped\"}} {stopped}\n\
         # HELP fluvora_worker_concurrency_limit Maximum concurrent encoder processes.\n\
         # TYPE fluvora_worker_concurrency_limit gauge\n\
         fluvora_worker_concurrency_limit {}\n",
        state.concurrency_limit
    )
}

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&request.asset_id)?;
    let (placement_resource_id, placement_generation) = job_placement(
        request.placement_resource_id.as_deref(),
        request.placement_generation,
        &request.asset_id,
    )?;
    let input = resolve_existing_file(&state.input_root, &request.input).await?;
    let output = resolve_output_directory(&state.output_root, &request.output_directory).await?;
    let streams = probe_input_streams(&state.ffprobe, &input).await?;
    let operation = WorkerOperation::PackageHls {
        renditions: request
            .renditions
            .into_iter()
            .map(|rendition| Rendition {
                width: rendition.width,
                height: rendition.height,
                video_bitrate_bps: rendition.video_bitrate_bps,
                audio_bitrate_bps: rendition.audio_bitrate_bps,
            })
            .collect(),
        segment_duration_millis: request.segment_duration_millis,
        has_audio: streams.has_audio,
    };
    let process = build_worker_process(state.ffmpeg.as_ref().clone(), &input, &output, &operation)
        .map_err(|error| ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_job",
            message: error.to_string(),
        })?;
    let job_id = state.next_job_id.fetch_add(1, Ordering::Relaxed);
    if job_id == u64::MAX {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "job_id_exhausted",
            message: "worker job identifier exhausted".to_owned(),
        });
    }
    let response = CreateJobResponse { job_id };
    insert_job(&state, job_id, request.asset_id.clone())?;
    let outcome = commit_worker_assignment(
        &state,
        "vod",
        &placement_resource_id,
        placement_generation,
        &request.asset_id,
        AssignmentResponse::Vod(response.clone()),
    );
    let replaced_job_id = match outcome {
        Ok(AssignmentOutcome::Accepted { replaced_job_id }) => replaced_job_id,
        Ok(AssignmentOutcome::Duplicate(AssignmentResponse::Vod(existing))) => {
            remove_job(&state, job_id);
            return Ok((StatusCode::OK, Json(existing)));
        }
        Ok(AssignmentOutcome::Duplicate(_)) => {
            remove_job(&state, job_id);
            return Err(assignment_type_conflict());
        }
        Err(error) => {
            remove_job(&state, job_id);
            return Err(error);
        }
    };
    cancel_replaced_job(&state, replaced_job_id)?;
    let (cancel, cancellation) = oneshot::channel();
    state
        .cancellations
        .lock()
        .map_err(lock_error)?
        .insert(job_id, cancel);
    let task_state = state.clone();
    tokio::spawn(async move {
        run_job(task_state, job_id, process, cancellation).await;
    });
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn probe_input_streams(program: &Path, input: &Path) -> Result<InputStreams, ApiError> {
    let mut command = tokio::process::Command::new(program);
    command.args([
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type",
        "-of",
        "csv=p=0",
    ]);
    command.arg(input).stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| invalid_media("media probe timed out"))?
        .map_err(|error| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "probe_unavailable",
            message: format!("failed to execute media probe: {error}"),
        })?;
    if !output.status.success() {
        return Err(invalid_media("source is not a supported media container"));
    }
    if output.stdout.len() > MAX_PROBE_OUTPUT_BYTES {
        return Err(invalid_media("source declares too many media streams"));
    }
    let stream_types = std::str::from_utf8(&output.stdout)
        .map_err(|_| invalid_media("media probe returned malformed output"))?;
    let has_video = stream_types.lines().any(|line| line.trim() == "video");
    let has_audio = stream_types.lines().any(|line| line.trim() == "audio");
    if !has_video {
        return Err(invalid_media(
            "VOD rendition ladders require a decodable video stream",
        ));
    }
    Ok(InputStreams { has_audio })
}

fn invalid_media(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_media",
        message: message.to_owned(),
    }
}

async fn create_live_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateLiveJobRequest>,
) -> Result<(StatusCode, Json<CreateLiveJobResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&request.stream_id)?;
    let (placement_resource_id, placement_generation) = job_placement(
        request.placement_resource_id.as_deref(),
        request.placement_generation,
        &request.stream_id,
    )?;
    validate_live_tracks(&request.tracks)?;
    let permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "live_worker_capacity",
            message: "no live encoder slot is currently available".to_owned(),
        })?;
    let output =
        resolve_output_directory(&state.live_output_root, &request.output_directory).await?;
    let (reservations, destinations) = allocate_live_destinations(&request.tracks).await?;
    let job_id = allocate_job_id(&state)?;
    let sdp_path = output.join(format!(".fluvora-{job_id}.sdp"));
    let sdp = render_live_sdp(&request.tracks, &destinations)?;
    let process = build_live_rtp_process(
        state.ffmpeg.as_ref().clone(),
        &sdp_path,
        &output,
        &live_package_spec(&request),
    )
    .map_err(|error| ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_live_job",
        message: error.to_string(),
    })?;
    tokio::fs::write(&sdp_path, sdp)
        .await
        .map_err(|error| io_error(&error))?;
    let response = CreateLiveJobResponse {
        job_id,
        destinations,
    };
    insert_job(&state, job_id, request.stream_id.clone())?;
    let outcome = commit_worker_assignment(
        &state,
        "live",
        &placement_resource_id,
        placement_generation,
        &request.stream_id,
        AssignmentResponse::Live(response.clone()),
    );
    let replaced_job_id = match outcome {
        Ok(AssignmentOutcome::Accepted { replaced_job_id }) => replaced_job_id,
        Ok(AssignmentOutcome::Duplicate(AssignmentResponse::Live(existing))) => {
            remove_job(&state, job_id);
            remove_temporary_file(&sdp_path).await;
            return Ok((StatusCode::OK, Json(existing)));
        }
        Ok(AssignmentOutcome::Duplicate(_)) => {
            remove_job(&state, job_id);
            remove_temporary_file(&sdp_path).await;
            return Err(assignment_type_conflict());
        }
        Err(error) => {
            remove_job(&state, job_id);
            remove_temporary_file(&sdp_path).await;
            return Err(error);
        }
    };
    cancel_replaced_job(&state, replaced_job_id)?;
    let (cancel, cancellation) = oneshot::channel();
    state
        .cancellations
        .lock()
        .map_err(lock_error)?
        .insert(job_id, cancel);
    let task_state = state.clone();
    tokio::spawn(async move {
        run_live_job(
            task_state,
            job_id,
            LiveJobRun {
                process,
                sdp_path,
                reservations,
                permit,
                cancellation,
                readiness: None,
            },
        )
        .await;
    });
    Ok((StatusCode::ACCEPTED, Json(response)))
}

fn live_package_spec(request: &CreateLiveJobRequest) -> LivePackageSpec {
    LivePackageSpec {
        has_video: request.tracks.iter().any(|track| track.kind == "video"),
        has_audio: request.tracks.iter().any(|track| track.kind == "audio"),
        segment_duration_millis: request.segment_duration_millis,
        window_segments: request.window_segments,
        renditions: request
            .renditions
            .iter()
            .map(|rendition| Rendition {
                width: rendition.width,
                height: rendition.height,
                video_bitrate_bps: rendition.video_bitrate_bps,
                audio_bitrate_bps: rendition.audio_bitrate_bps,
            })
            .collect(),
    }
}

async fn allocate_live_destinations(
    tracks: &[LiveTrackRequest],
) -> Result<(Vec<UdpSocket>, Vec<LiveDestination>), ApiError> {
    let mut reservations = Vec::with_capacity(tracks.len());
    let mut destinations = Vec::with_capacity(tracks.len());
    for track in tracks {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|error| io_error(&error))?;
        let destination = socket.local_addr().map_err(|error| io_error(&error))?;
        destinations.push(LiveDestination {
            track_id: track.track_id,
            destination,
        });
        reservations.push(socket);
    }
    Ok((reservations, destinations))
}

async fn create_realtime_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRealtimeJobRequest>,
) -> Result<(StatusCode, Json<CreateRealtimeJobResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    validate_identifier(&request.job_key)?;
    let (placement_resource_id, placement_generation) = realtime_placement(&request)?;
    let target_codec = validate_realtime_request(&request)?;
    let permit = state
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "realtime_worker_capacity",
            message: "no realtime encoder slot is currently available".to_owned(),
        })?;
    let reservation = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|error| io_error(&error))?;
    let source_destination = reservation.local_addr().map_err(|error| io_error(&error))?;
    let job_id = allocate_job_id(&state)?;
    let sdp_path = state
        .live_output_root
        .join(format!(".fluvora-realtime-{job_id}.sdp"));
    let source_descriptor = LiveDestination {
        track_id: request.source.track_id,
        destination: source_destination,
    };
    let sdp = render_live_sdp(
        std::slice::from_ref(&request.source),
        std::slice::from_ref(&source_descriptor),
    )?;
    tokio::fs::write(&sdp_path, sdp)
        .await
        .map_err(|error| io_error(&error))?;
    let process = build_realtime_transcode_process(
        state.ffmpeg.as_ref().clone(),
        &sdp_path,
        RealtimeTranscodeSpec {
            target_codec,
            destination: request.target.destination,
            payload_type: request.target.payload_type,
            ssrc: request.target.ssrc,
            width: request.target.width,
            height: request.target.height,
            frames_per_second: request.target.frames_per_second,
            bitrate_bps: request.target.bitrate_bps,
        },
    )
    .map_err(|error| ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_realtime_job",
        message: error.to_string(),
    })?;
    let response = CreateRealtimeJobResponse {
        job_id,
        source_destination,
    };
    if let Some(existing) = install_realtime_assignment(
        &state,
        &placement_resource_id,
        placement_generation,
        &request.job_key,
        &response,
        &sdp_path,
    )
    .await?
    {
        return Ok((StatusCode::OK, Json(existing)));
    }
    let (cancel, cancellation) = oneshot::channel();
    state
        .cancellations
        .lock()
        .map_err(lock_error)?
        .insert(job_id, cancel);
    let task_state = state.clone();
    let (readiness, ready) = oneshot::channel();
    tokio::spawn(async move {
        run_live_job(
            task_state,
            job_id,
            LiveJobRun {
                process,
                sdp_path,
                reservations: vec![reservation],
                permit,
                cancellation,
                readiness: Some(readiness),
            },
        )
        .await;
    });
    await_realtime_readiness(ready).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

fn realtime_placement(request: &CreateRealtimeJobRequest) -> Result<(String, u64), ApiError> {
    job_placement(
        request.placement_resource_id.as_deref(),
        request.placement_generation,
        &request.job_key,
    )
}

fn job_placement(
    resource_id: Option<&str>,
    generation: Option<u64>,
    default_resource_id: &str,
) -> Result<(String, u64), ApiError> {
    let resource_id = resource_id.unwrap_or(default_resource_id).to_owned();
    validate_identifier(&resource_id)?;
    let generation = generation.unwrap_or(1);
    if generation == 0 {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_placement_generation",
            message: "placement generation must be positive".to_owned(),
        })
    } else {
        Ok((resource_id, generation))
    }
}

fn validate_realtime_request(
    request: &CreateRealtimeJobRequest,
) -> Result<RealtimeCodec, ApiError> {
    validate_live_tracks(std::slice::from_ref(&request.source))?;
    let target_codec = parse_realtime_codec(&request.target.codec)?;
    if target_codec.is_audio() == (request.source.kind == "audio") {
        Ok(target_codec)
    } else {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "realtime_media_kind_mismatch",
            message: "realtime source and target codecs must carry the same media kind".to_owned(),
        })
    }
}

async fn install_realtime_assignment(
    state: &AppState,
    resource_id: &str,
    generation: u64,
    job_key: &str,
    response: &CreateRealtimeJobResponse,
    sdp_path: &Path,
) -> Result<Option<CreateRealtimeJobResponse>, ApiError> {
    insert_job(state, response.job_id, job_key.to_owned())?;
    let outcome = commit_worker_assignment(
        state,
        "realtime",
        resource_id,
        generation,
        job_key,
        AssignmentResponse::Realtime(response.clone()),
    );
    match outcome {
        Ok(AssignmentOutcome::Duplicate(AssignmentResponse::Realtime(existing))) => {
            remove_job(state, response.job_id);
            remove_temporary_file(sdp_path).await;
            Ok(Some(existing))
        }
        Ok(AssignmentOutcome::Duplicate(_)) => {
            remove_job(state, response.job_id);
            remove_temporary_file(sdp_path).await;
            Err(assignment_type_conflict())
        }
        Ok(AssignmentOutcome::Accepted { replaced_job_id }) => {
            cancel_replaced_job(state, replaced_job_id)?;
            Ok(None)
        }
        Err(error) => {
            remove_job(state, response.job_id);
            remove_temporary_file(sdp_path).await;
            Err(error)
        }
    }
}

async fn await_realtime_readiness(
    ready: oneshot::Receiver<Result<(), String>>,
) -> Result<(), ApiError> {
    match tokio::time::timeout(Duration::from_secs(5), ready).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(reason))) => {
            return Err(ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "realtime_encoder_start_failed",
                message: reason,
            });
        }
        Ok(Err(_)) | Err(_) => {
            return Err(ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "realtime_encoder_start_timeout",
                message: "realtime encoder did not confirm startup".to_owned(),
            });
        }
    }
    Ok(())
}

async fn stop_live_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<u64>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    let cancellation = state
        .cancellations
        .lock()
        .map_err(lock_error)?
        .remove(&job_id)
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "live_job_not_found",
            message: "unknown or completed live job".to_owned(),
        })?;
    let _ = cancellation.send(());
    Ok(StatusCode::ACCEPTED)
}

fn parse_realtime_codec(value: &str) -> Result<RealtimeCodec, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "opus" => Ok(RealtimeCodec::Opus),
        "vp8" => Ok(RealtimeCodec::Vp8),
        "vp9" => Ok(RealtimeCodec::Vp9),
        "h264" => Ok(RealtimeCodec::H264),
        "av1" => Ok(RealtimeCodec::Av1),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "unsupported_realtime_codec",
            message: "realtime codec must be opus, vp8, vp9, h264, or av1".to_owned(),
        }),
    }
}

async fn run_live_job(state: AppState, job_id: u64, run: LiveJobRun) {
    let LiveJobRun {
        process,
        sdp_path,
        reservations,
        permit: _permit,
        cancellation,
        readiness,
    } = run;
    set_state(&state, job_id, JobState::Running);
    drop(reservations);
    let mut command = tokio::process::Command::new(process.program);
    command
        .args(process.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some(working_directory) = process.working_directory {
        command.current_dir(working_directory);
    }
    let child = command.spawn();
    let Ok(mut child) = child else {
        let reason = "live encoder failed to start".to_owned();
        if let Some(readiness) = readiness {
            let _ = readiness.send(Err(reason.clone()));
        }
        set_failed(&state, job_id, &reason);
        finish_live_task(&state, job_id, &sdp_path).await;
        return;
    };
    if let Some(readiness) = readiness {
        tokio::time::sleep(Duration::from_millis(150)).await;
        match child.try_wait() {
            Ok(None) => {
                let _ = readiness.send(Ok(()));
            }
            Ok(Some(status)) => {
                let reason = format!(
                    "realtime encoder exited during startup with {}",
                    status.code().unwrap_or(-1)
                );
                let _ = readiness.send(Err(reason.clone()));
                set_failed(&state, job_id, &reason);
                finish_live_task(&state, job_id, &sdp_path).await;
                return;
            }
            Err(error) => {
                let reason = format!("realtime encoder startup check failed: {error}");
                let _ = readiness.send(Err(reason.clone()));
                set_failed(&state, job_id, &reason);
                finish_live_task(&state, job_id, &sdp_path).await;
                return;
            }
        }
    }
    let completion = tokio::select! {
        status = child.wait() => LiveCompletion::Process(status),
        _ = cancellation => LiveCompletion::Stop,
    };
    match completion {
        LiveCompletion::Process(Ok(status)) if status.success() => set_state(
            &state,
            job_id,
            JobState::Succeeded {
                finished_at_millis: now_millis(),
            },
        ),
        LiveCompletion::Process(Ok(status)) => set_failed(
            &state,
            job_id,
            &format!("live encoder exited with {}", status.code().unwrap_or(-1)),
        ),
        LiveCompletion::Process(Err(error)) => {
            set_failed(
                &state,
                job_id,
                &format!("live encoder wait failed: {error}"),
            );
        }
        LiveCompletion::Stop => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            set_state(
                &state,
                job_id,
                JobState::Stopped {
                    finished_at_millis: now_millis(),
                },
            );
        }
    }
    finish_live_task(&state, job_id, &sdp_path).await;
}

async fn finish_live_task(state: &AppState, job_id: u64, sdp_path: &Path) {
    if let Ok(mut cancellations) = state.cancellations.lock() {
        cancellations.remove(&job_id);
    }
    remove_temporary_file(sdp_path).await;
}

fn validate_live_tracks(tracks: &[LiveTrackRequest]) -> Result<(), ApiError> {
    if tracks.is_empty() || tracks.len() > 2 {
        return Err(invalid_live_tracks());
    }
    let mut audio = false;
    let mut video = false;
    let mut identifiers = std::collections::HashSet::new();
    for track in tracks {
        if !identifiers.insert(track.track_id)
            || !(96..=127).contains(&track.payload_type)
            || track.clock_rate == 0
            || track.clock_rate > 192_000
            || track.fmtp.as_ref().is_some_and(|fmtp| {
                fmtp.is_empty()
                    || fmtp.len() > 512
                    || !fmtp
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            })
        {
            return Err(invalid_live_tracks());
        }
        match (
            track.kind.as_str(),
            track.codec.to_ascii_lowercase().as_str(),
        ) {
            ("audio", "opus") if !audio && track.channels.is_none_or(|value| value <= 8) => {
                audio = true;
            }
            ("video", "h264" | "vp8" | "vp9" | "av1") if !video => {
                video = true;
            }
            _ => return Err(invalid_live_tracks()),
        }
    }
    Ok(())
}

fn render_live_sdp(
    tracks: &[LiveTrackRequest],
    destinations: &[LiveDestination],
) -> Result<String, ApiError> {
    use std::fmt::Write as _;

    let mut sdp =
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Fluvora Live\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n"
            .to_owned();
    for (track, destination) in tracks.iter().zip(destinations) {
        let codec = match track.codec.to_ascii_lowercase().as_str() {
            "opus" => "opus",
            "h264" => "H264",
            "vp8" => "VP8",
            "vp9" => "VP9",
            "av1" => "AV1",
            _ => return Err(invalid_live_tracks()),
        };
        let _ = write!(
            sdp,
            "m={} {} RTP/AVP {}\r\na=rtpmap:{} {}/{}",
            track.kind,
            destination.destination.port(),
            track.payload_type,
            track.payload_type,
            codec,
            track.clock_rate
        );
        if track.kind == "audio" {
            let _ = write!(sdp, "/{}", track.channels.unwrap_or(2));
        }
        sdp.push_str("\r\na=recvonly\r\n");
        if let Some(fmtp) = &track.fmtp {
            let _ = write!(sdp, "a=fmtp:{} {fmtp}\r\n", track.payload_type);
        }
    }
    Ok(sdp)
}

fn invalid_live_tracks() -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_live_tracks",
        message: "live job requires at most one valid audio and one valid video RTP track"
            .to_owned(),
    }
}

fn allocate_job_id(state: &AppState) -> Result<u64, ApiError> {
    let job_id = state.next_job_id.fetch_add(1, Ordering::Relaxed);
    if job_id == u64::MAX {
        Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "job_id_exhausted",
            message: "worker job identifier exhausted".to_owned(),
        })
    } else {
        Ok(job_id)
    }
}

fn commit_worker_assignment(
    state: &AppState,
    assignment_kind: &str,
    resource_id: &str,
    generation: u64,
    job_key: &str,
    response: AssignmentResponse,
) -> Result<AssignmentOutcome, ApiError> {
    let mut registry = state.assignments.lock().map_err(lock_error)?;
    commit_assignment(
        &mut registry,
        assignment_kind,
        resource_id,
        generation,
        job_key,
        response,
    )
}

fn commit_assignment(
    registry: &mut AssignmentRegistry,
    assignment_kind: &str,
    resource_id: &str,
    generation: u64,
    job_key: &str,
    response: AssignmentResponse,
) -> Result<AssignmentOutcome, ApiError> {
    let assignment_id = format!("{assignment_kind}:{resource_id}");
    if let Some(current) = registry.active.get(&assignment_id) {
        if generation < current.generation {
            return Err(stale_generation(current.generation));
        }
        if generation == current.generation {
            return if current.job_key == job_key {
                Ok(AssignmentOutcome::Duplicate(current.response.clone()))
            } else {
                Err(ApiError {
                    status: StatusCode::CONFLICT,
                    code: "placement_generation_conflict",
                    message: "placement generation is already bound to another job key".to_owned(),
                })
            };
        }
    }
    if let Some(fence) = registry.snapshot.records.get(&assignment_id) {
        if generation < fence.generation {
            return Err(stale_generation(fence.generation));
        }
        if generation == fence.generation {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "placement_generation_consumed",
                message: "placement generation was consumed before this worker restart; the coordinator must advance it"
                    .to_owned(),
            });
        }
    }
    let mut next_snapshot = registry.snapshot.clone();
    next_snapshot.revision = next_snapshot.revision.checked_add(1).ok_or(ApiError {
        status: StatusCode::INSUFFICIENT_STORAGE,
        code: "assignment_revision_exhausted",
        message: "worker assignment fence revision exhausted".to_owned(),
    })?;
    next_snapshot.records.insert(
        assignment_id.clone(),
        FenceRecord {
            generation,
            job_key: job_key.to_owned(),
        },
    );
    persist_fence_snapshot(&registry.directory, &next_snapshot)?;
    registry.snapshot = next_snapshot;
    let replaced_job_id = registry
        .active
        .insert(
            assignment_id,
            WorkerAssignment {
                generation,
                job_key: job_key.to_owned(),
                response,
            },
        )
        .map(|assignment| assignment.response.job_id());
    Ok(AssignmentOutcome::Accepted { replaced_job_id })
}

fn stale_generation(generation: u64) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "stale_placement_generation",
        message: format!("worker has already accepted placement generation {generation}"),
    }
}

fn remove_job(state: &AppState, job_id: u64) {
    if let Ok(mut jobs) = state.jobs.write() {
        jobs.remove(&job_id);
    }
}

fn cancel_replaced_job(state: &AppState, job_id: Option<u64>) -> Result<(), ApiError> {
    if let Some(job_id) = job_id
        && let Some(cancel) = state
            .cancellations
            .lock()
            .map_err(lock_error)?
            .remove(&job_id)
    {
        let _ = cancel.send(());
    }
    Ok(())
}

fn assignment_type_conflict() -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "placement_resource_type_conflict",
        message: "placement resource is already bound to another job type".to_owned(),
    }
}

fn insert_job(state: &AppState, job_id: u64, asset_id: String) -> Result<(), ApiError> {
    let mut jobs = state.jobs.write().map_err(lock_error)?;
    if jobs.len() >= MAX_JOBS_RETAINED {
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "job_registry_full",
            message: "worker job history capacity reached".to_owned(),
        });
    }
    jobs.insert(
        job_id,
        Job {
            id: job_id,
            asset_id,
            created_at_millis: now_millis(),
            state: JobState::Queued,
        },
    );
    Ok(())
}

async fn get_job(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<u64>,
    headers: HeaderMap,
) -> Result<Json<Job>, ApiError> {
    authorize(&headers, &state.token)?;
    let jobs = state.jobs.read().map_err(lock_error)?;
    jobs.get(&job_id).cloned().map(Json).ok_or(ApiError {
        status: StatusCode::NOT_FOUND,
        code: "job_not_found",
        message: "unknown worker job".to_owned(),
    })
}

async fn run_job(
    state: AppState,
    job_id: u64,
    process: fluvora_media_pipeline::ProcessSpec,
    mut cancellation: oneshot::Receiver<()>,
) {
    let permit = state.permits.clone().acquire_owned();
    let _permit = tokio::select! {
        permit = permit => {
            let Ok(permit) = permit else {
                set_failed(&state, job_id, "worker is shutting down");
                finish_job_task(&state, job_id);
                return;
            };
            permit
        }
        _ = &mut cancellation => {
            set_state(
                &state,
                job_id,
                JobState::Stopped {
                    finished_at_millis: now_millis(),
                },
            );
            finish_job_task(&state, job_id);
            return;
        }
    };
    set_state(&state, job_id, JobState::Running);
    let mut command = tokio::process::Command::new(process.program);
    command
        .args(process.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(working_directory) = process.working_directory {
        command.current_dir(working_directory);
    }
    let child = command.spawn();
    let Ok(mut child) = child else {
        set_failed(&state, job_id, "encoder failed to start");
        finish_job_task(&state, job_id);
        return;
    };
    let completion = tokio::select! {
        status = child.wait() => LiveCompletion::Process(status),
        _ = &mut cancellation => LiveCompletion::Stop,
    };
    match completion {
        LiveCompletion::Process(Ok(exit)) if exit.success() => set_state(
            &state,
            job_id,
            JobState::Succeeded {
                finished_at_millis: now_millis(),
            },
        ),
        LiveCompletion::Process(Ok(exit)) => set_failed(
            &state,
            job_id,
            &format!("encoder exited with {}", exit.code().unwrap_or(-1)),
        ),
        LiveCompletion::Process(Err(error)) => {
            set_failed(&state, job_id, &format!("encoder wait failed: {error}"));
        }
        LiveCompletion::Stop => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            set_state(
                &state,
                job_id,
                JobState::Stopped {
                    finished_at_millis: now_millis(),
                },
            );
        }
    }
    finish_job_task(&state, job_id);
}

fn finish_job_task(state: &AppState, job_id: u64) {
    if let Ok(mut cancellations) = state.cancellations.lock() {
        cancellations.remove(&job_id);
    }
}

fn set_failed(state: &AppState, job_id: u64, reason: &str) {
    set_state(
        state,
        job_id,
        JobState::Failed {
            finished_at_millis: now_millis(),
            reason: reason.chars().take(512).collect(),
        },
    );
}

fn set_state(state: &AppState, job_id: u64, job_state: JobState) {
    if let Ok(mut jobs) = state.jobs.write()
        && let Some(job) = jobs.get_mut(&job_id)
    {
        job.state = job_state;
    }
}

async fn canonical_directory_from_env(
    variable: &str,
    default: &str,
) -> Result<PathBuf, std::io::Error> {
    let path = PathBuf::from(env::var(variable).unwrap_or_else(|_| default.to_owned()));
    tokio::fs::create_dir_all(&path).await?;
    tokio::fs::canonicalize(path).await
}

async fn resolve_existing_file(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    validate_relative_path(relative)?;
    let resolved = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "input_not_found",
            message: "input media does not exist".to_owned(),
        })?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_input",
            message: "input must be a file beneath the configured root".to_owned(),
        });
    }
    Ok(resolved)
}

async fn resolve_output_directory(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    validate_relative_path(relative)?;
    let target = root.join(relative);
    tokio::fs::create_dir_all(&target)
        .await
        .map_err(|error| io_error(&error))?;
    let resolved = tokio::fs::canonicalize(target)
        .await
        .map_err(|error| io_error(&error))?;
    if !resolved.starts_with(root) {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_output",
            message: "output must remain beneath the configured root".to_owned(),
        });
    }
    Ok(resolved)
}

fn validate_relative_path(value: &str) -> Result<(), ApiError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 1_024
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_path",
            message: "path must contain only relative normal components".to_owned(),
        });
    }
    Ok(())
}

fn resolve_executable(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.components().count() == 1 {
        Ok(path)
    } else {
        std::fs::canonicalize(path)
    }
}

fn validate_identifier(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_asset_id",
            message: "asset id must be 1..=128 ASCII identifier characters".to_owned(),
        });
    }
    Ok(())
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
            message: "invalid worker bearer token".to_owned(),
        })
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "state_unavailable",
        message: "worker state lock is poisoned".to_owned(),
    }
}

fn io_error(error: &std::io::Error) -> ApiError {
    eprintln!("media worker storage operation failed: {error}");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "storage_error",
        message: "media worker storage operation failed".to_owned(),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn job_id_seed() -> u64 {
    let timestamp = now_millis().min((u64::MAX >> 16).saturating_sub(1));
    (timestamp << 16) | u64::from(std::process::id() & 0xffff)
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
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{
        AssignmentOutcome, AssignmentRegistry, AssignmentResponse, CreateRealtimeJobResponse,
        FenceSnapshot, Job, JobState, commit_assignment, load_assignment_registry,
        resolve_executable,
    };

    #[test]
    fn worker_job_response_has_flat_lifecycle_state() {
        let value = serde_json::to_value(Job {
            id: 7,
            asset_id: "asset".to_owned(),
            created_at_millis: 10,
            state: JobState::Failed {
                finished_at_millis: 20,
                reason: "encoder exited".to_owned(),
            },
        })
        .expect("serialize job");
        assert_eq!(value["state"], "failed");
        assert_eq!(value["reason"], "encoder exited");
        assert_eq!(value["finished_at_millis"], 20);
    }

    #[test]
    fn keeps_path_commands_and_resolves_configured_executable_files() {
        assert_eq!(
            resolve_executable(PathBuf::from("ffmpeg")).expect("PATH command"),
            Path::new("ffmpeg")
        );
        let directory = tempdir().expect("executable directory");
        let executable = directory.path().join("ffmpeg-test");
        std::fs::write(&executable, b"fixture").expect("fixture");
        assert_eq!(
            resolve_executable(executable.clone()).expect("configured executable"),
            std::fs::canonicalize(executable).expect("canonical executable")
        );
    }

    fn write_invalid_newer_snapshots(directory: &Path, snapshot: &FenceSnapshot) {
        std::fs::write(directory.join("00000000000000000099.json"), b"{not-json")
            .expect("corrupt latest snapshot");
        std::fs::write(
            directory.join("00000000000000000098.json"),
            serde_json::to_vec(snapshot).expect("forged revision"),
        )
        .expect("inconsistent latest snapshot");
    }

    #[test]
    fn fences_and_deduplicates_realtime_assignments() {
        let directory = tempdir().expect("assignment state");
        let mut assignments = AssignmentRegistry {
            snapshot: FenceSnapshot::default(),
            active: HashMap::new(),
            directory: directory.path().to_path_buf(),
        };
        let first = CreateRealtimeJobResponse {
            job_id: 7,
            source_destination: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000),
        };
        assert!(matches!(
            commit_assignment(
                &mut assignments,
                "realtime",
                "room-job",
                1,
                "job-a",
                AssignmentResponse::Realtime(first.clone()),
            )
            .expect("accept first generation"),
            AssignmentOutcome::Accepted {
                replaced_job_id: None
            }
        ));
        assert!(matches!(
            commit_assignment(
                &mut assignments,
                "realtime",
                "room-job",
                1,
                "job-a",
                AssignmentResponse::Realtime(first.clone()),
            )
            .expect("deduplicate generation"),
            AssignmentOutcome::Duplicate(AssignmentResponse::Realtime(response))
                if response.job_id == 7
        ));
        let second = CreateRealtimeJobResponse {
            job_id: 8,
            ..first.clone()
        };
        assert!(matches!(
            commit_assignment(
                &mut assignments,
                "realtime",
                "room-job",
                2,
                "job-b",
                AssignmentResponse::Realtime(second),
            )
            .expect("accept takeover"),
            AssignmentOutcome::Accepted {
                replaced_job_id: Some(7)
            }
        ));
        let error = commit_assignment(
            &mut assignments,
            "realtime",
            "room-job",
            1,
            "job-a",
            AssignmentResponse::Realtime(first.clone()),
        )
        .expect_err("reject stale owner");
        assert_eq!(error.code, "stale_placement_generation");

        write_invalid_newer_snapshots(directory.path(), &assignments.snapshot);

        let mut restarted =
            load_assignment_registry(directory.path().to_path_buf()).expect("reload fences");
        let consumed = commit_assignment(
            &mut restarted,
            "realtime",
            "room-job",
            2,
            "job-b",
            AssignmentResponse::Realtime(first.clone()),
        )
        .expect_err("same generation must not revive a job after restart");
        assert_eq!(consumed.code, "placement_generation_consumed");
        assert!(matches!(
            commit_assignment(
                &mut restarted,
                "realtime",
                "room-job",
                3,
                "job-c",
                AssignmentResponse::Realtime(first),
            )
            .expect("advance after restart"),
            AssignmentOutcome::Accepted {
                replaced_job_id: None
            }
        ));
    }
}
