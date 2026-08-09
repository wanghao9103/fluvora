use std::net::SocketAddr;

use axum::http::StatusCode;
use fluvora_control_store::ServicePlacement;
use fluvora_domain::CommandId;
use fluvora_transcode_bridge::{JobId as TranscodeJobId, TranscodeSpec};

use crate::control_client::{
    advance_worker_placement, bounded_response_bytes, internal_delete, internal_url,
    media_control_delete_json, media_control_post, remove_worker_placement_generation,
    worker_control_json_post,
};
use crate::error::{ApiError, lock_error};
use crate::models::{
    ActiveTranscode, AppState, CreateRealtimeWorkerJob, MediaRecordingSink,
    RealtimeWorkerJobResponse, RealtimeWorkerSource, RealtimeWorkerTarget, RegisteredTrack,
    WorkerJobStatus,
};
use crate::runtime::{format_id, random_u128};
use crate::services::{highest_source_ssrc, transcode_job_not_found};
use crate::signals::append_signal;
use crate::validation::media_codec_name;

#[derive(Debug)]
enum WorkerProbe {
    Healthy,
    Terminal(String),
    Unavailable,
}

pub(super) async fn run_transcode_reconciler(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let jobs = state
            .transcodes
            .lock()
            .await
            .active
            .iter()
            .map(|(job_id, active)| {
                (
                    *job_id,
                    active.worker_job_id,
                    active.worker_endpoint.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut probes = tokio::task::JoinSet::new();
        for (job_id, worker_job_id, worker_endpoint) in jobs {
            let probe_state = state.clone();
            probes.spawn(async move {
                (
                    job_id,
                    worker_job_id,
                    probe_worker_job(&probe_state, &worker_endpoint, worker_job_id).await,
                )
            });
        }
        while let Some(Ok((job_id, worker_job_id, probe))) = probes.join_next().await {
            reconcile_worker_probe(&state, job_id, worker_job_id, probe).await;
        }
    }
}

async fn probe_worker_job(
    state: &AppState,
    worker_endpoint: &str,
    worker_job_id: u64,
) -> WorkerProbe {
    let Ok(url) = internal_url(worker_endpoint, &format!("/v1/jobs/{worker_job_id}")) else {
        eprintln!("refusing invalid realtime worker probe endpoint");
        return WorkerProbe::Unavailable;
    };
    let request = state
        .http_client
        .get(url)
        .bearer_auth(state.worker_control_token.as_ref())
        .send();
    let Ok(Ok(response)) = tokio::time::timeout(std::time::Duration::from_secs(3), request).await
    else {
        return WorkerProbe::Unavailable;
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return WorkerProbe::Terminal("worker forgot the realtime job".to_owned());
    }
    if !response.status().is_success() {
        return WorkerProbe::Unavailable;
    }
    let Ok(bytes) = bounded_response_bytes(
        response,
        "media_worker_response_too_large",
        "media_worker_invalid_response",
    )
    .await
    else {
        return WorkerProbe::Unavailable;
    };
    let Ok(status) = serde_json::from_slice::<WorkerJobStatus>(&bytes) else {
        return WorkerProbe::Unavailable;
    };
    match status.state.as_str() {
        "queued" | "running" => WorkerProbe::Healthy,
        "failed" | "stopped" | "succeeded" => WorkerProbe::Terminal(
            status
                .reason
                .unwrap_or_else(|| format!("realtime worker entered {}", status.state)),
        ),
        _ => WorkerProbe::Unavailable,
    }
}

async fn reconcile_worker_probe(
    state: &AppState,
    job_id: TranscodeJobId,
    worker_job_id: u64,
    probe: WorkerProbe,
) {
    let current = state
        .transcodes
        .lock()
        .await
        .active
        .get(&job_id)
        .is_some_and(|active| active.worker_job_id == worker_job_id);
    if !current {
        return;
    }
    match probe {
        WorkerProbe::Healthy => {
            state
                .transcodes
                .lock()
                .await
                .health_failures
                .remove(&job_id);
        }
        WorkerProbe::Unavailable => {
            let failures = {
                let mut registry = state.transcodes.lock().await;
                let failures = registry.health_failures.entry(job_id).or_default();
                *failures = failures.saturating_add(1);
                *failures
            };
            if failures == 3 {
                emit_transcode_event(
                    state,
                    job_id,
                    "media.transcode_health_unknown",
                    "media worker has missed three health probes",
                )
                .await;
            }
            if failures >= 10
                && failures.is_multiple_of(5)
                && let Err(error) = restart_transcode(state, job_id, worker_job_id).await
            {
                emit_transcode_event(
                    state,
                    job_id,
                    "media.transcode_failover_failed",
                    &format!(
                        "worker remained unavailable; failover failed: {}",
                        error.message
                    ),
                )
                .await;
            }
        }
        WorkerProbe::Terminal(reason) => {
            if let Err(error) = restart_transcode(state, job_id, worker_job_id).await {
                let failures = {
                    let mut registry = state.transcodes.lock().await;
                    let failures = registry.health_failures.entry(job_id).or_default();
                    *failures = failures.saturating_add(1);
                    *failures
                };
                if failures == 1 || failures.is_multiple_of(10) {
                    emit_transcode_event(
                        state,
                        job_id,
                        "media.transcode_restart_failed",
                        &format!("{reason}; restart failed: {}", error.message),
                    )
                    .await;
                }
            }
        }
    }
}

async fn restart_transcode(
    state: &AppState,
    job_id: TranscodeJobId,
    failed_worker_job_id: u64,
) -> Result<(), ApiError> {
    let (active, specification) = {
        let registry = state.transcodes.lock().await;
        let active = registry
            .active
            .get(&job_id)
            .filter(|active| active.worker_job_id == failed_worker_job_id)
            .cloned()
            .ok_or_else(transcode_job_not_found)?;
        let specification = registry
            .coordinator
            .specification(job_id)
            .cloned()
            .ok_or_else(transcode_job_not_found)?;
        (active, specification)
    };
    let source = state
        .tracks
        .read()
        .map_err(lock_error)?
        .get(&(active.room_id, active.source_track_id))
        .cloned()
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "transcode_source_missing",
            message: "transcode source track no longer exists".to_owned(),
        })?;
    let (worker, placement) =
        create_replacement_worker(state, job_id, &active, &source, &specification).await?;
    let updated = {
        let mut registry = state.transcodes.lock().await;
        if let Some(current) = registry.active.get_mut(&job_id)
            && current.worker_job_id == failed_worker_job_id
        {
            current.worker_job_id = worker.job_id;
            current.worker_endpoint.clone_from(&placement.endpoint);
            current.worker_placement_generation = placement.generation;
            current.source_destination = worker.source_destination;
            registry.health_failures.remove(&job_id);
            true
        } else {
            false
        }
    };
    if !updated {
        rollback_restarted_worker(
            state,
            &active,
            &placement,
            worker.job_id,
            worker.source_destination,
        )
        .await;
        return Ok(());
    }
    if let Err(error) = media_control_delete_json(
        state,
        active.room_id,
        "/v1/sfu/recordings",
        &MediaRecordingSink {
            room_id: format_id(active.room_id.0),
            track_id: active.source_track_id,
            destination: active.source_destination,
            source_ssrc: None,
        },
    )
    .await
    {
        eprintln!(
            "failed to remove previous recording sink after transcode restart: {}",
            error.message
        );
    }
    internal_delete(
        state,
        &active.worker_endpoint,
        &state.worker_control_token,
        &format!("/v1/realtime-jobs/{failed_worker_job_id}"),
    )
    .await;
    emit_transcode_event(
        state,
        job_id,
        "media.transcode_restarted",
        "realtime transcoder recovered automatically",
    )
    .await;
    Ok(())
}

async fn create_replacement_worker(
    state: &AppState,
    job_id: TranscodeJobId,
    active: &ActiveTranscode,
    source: &RegisteredTrack,
    specification: &TranscodeSpec,
) -> Result<(RealtimeWorkerJobResponse, ServicePlacement), ApiError> {
    let placement = advance_worker_placement(
        state,
        &active.worker_placement_id,
        active.worker_placement_generation,
    )
    .await?;
    let worker: RealtimeWorkerJobResponse = match worker_control_json_post(
        state,
        &placement.endpoint,
        "/v1/realtime-jobs",
        &CreateRealtimeWorkerJob {
            job_key: format!("rtc-restart-{}-{}", format_id(active.room_id.0), job_id.0),
            placement_resource_id: active.worker_placement_id.clone(),
            placement_generation: placement.generation,
            source: RealtimeWorkerSource {
                track_id: active.source_track_id,
                kind: source.kind.clone(),
                codec: source.codec_name.clone(),
                payload_type: source.payload_type,
                clock_rate: source.clock_rate,
                channels: source.codec.is_audio().then_some(2),
                fmtp: None,
            },
            target: RealtimeWorkerTarget {
                codec: media_codec_name(specification.target_codec),
                destination: active.output_destination,
                payload_type: 120,
                ssrc: active.output_ssrc,
                width: specification.width,
                height: specification.height,
                frames_per_second: specification.frames_per_second,
                bitrate_bps: specification.bitrate_bps,
            },
        },
    )
    .await
    {
        Ok(worker) => worker,
        Err(error) => {
            remove_worker_placement_generation(
                state,
                &active.worker_placement_id,
                placement.generation,
            )
            .await;
            return Err(error);
        }
    };
    let source_ssrc = highest_source_ssrc(source);
    if let Err(error) = media_control_post(
        state,
        active.room_id,
        "/v1/sfu/recordings",
        &MediaRecordingSink {
            room_id: format_id(active.room_id.0),
            track_id: active.source_track_id,
            destination: worker.source_destination,
            source_ssrc,
        },
    )
    .await
    {
        internal_delete(
            state,
            &placement.endpoint,
            &state.worker_control_token,
            &format!("/v1/realtime-jobs/{}", worker.job_id),
        )
        .await;
        remove_worker_placement_generation(
            state,
            &active.worker_placement_id,
            placement.generation,
        )
        .await;
        return Err(error);
    }
    Ok((worker, placement))
}

async fn rollback_restarted_worker(
    state: &AppState,
    active: &ActiveTranscode,
    placement: &ServicePlacement,
    worker_job_id: u64,
    source_destination: SocketAddr,
) {
    if let Err(error) = media_control_delete_json(
        state,
        active.room_id,
        "/v1/sfu/recordings",
        &MediaRecordingSink {
            room_id: format_id(active.room_id.0),
            track_id: active.source_track_id,
            destination: source_destination,
            source_ssrc: None,
        },
    )
    .await
    {
        eprintln!(
            "failed to remove superseded recording sink during transcode rollback: {}",
            error.message
        );
    }
    internal_delete(
        state,
        &placement.endpoint,
        &state.worker_control_token,
        &format!("/v1/realtime-jobs/{worker_job_id}"),
    )
    .await;
    remove_worker_placement_generation(state, &active.worker_placement_id, placement.generation)
        .await;
}

async fn emit_transcode_event(state: &AppState, job_id: TranscodeJobId, kind: &str, message: &str) {
    let room_id = state
        .transcodes
        .lock()
        .await
        .active
        .get(&job_id)
        .map(|active| active.room_id);
    if let Some(room_id) = room_id {
        let command_id = match random_u128().map(CommandId) {
            Ok(command_id) => command_id,
            Err(error) => {
                eprintln!(
                    "failed to allocate transcode event command for job {}: {}",
                    job_id.0, error.message
                );
                return;
            }
        };
        if let Err(error) = append_signal(
            state,
            room_id,
            command_id,
            0,
            None,
            kind.to_owned(),
            serde_json::json!({
                "transcode_job_id": job_id.0,
                "message": message,
            }),
        )
        .await
        {
            eprintln!(
                "failed to emit transcode event {kind} for job {}: {}",
                job_id.0, error.message
            );
        }
    }
}
