use std::collections::HashMap;
use std::net::SocketAddr;

use axum::http::StatusCode;
use fluvora_control_store::ServicePlacement;
use fluvora_domain::RoomId;
use fluvora_transcode_bridge::{
    JobId as TranscodeJobId, JobState as TranscodeJobState, NegotiationDecision,
    NegotiationRequest, SourceDescriptor, TranscodeSpec, negotiate,
};

use crate::control_client::{
    internal_delete, media_control_delete_json, media_control_internal_delete,
    media_control_json_post, media_control_post, remove_worker_placement,
    remove_worker_placement_generation, worker_control_json_post, worker_control_placement,
};
use crate::error::{ApiError, internal_error, lock_error};
use crate::models::{
    ActiveTranscode, AppState, CreateMediaTranscodeIngress, CreateRealtimeWorkerJob,
    MediaRecordingSink, MediaTranscodeIngressResponse, RealtimeWorkerJobResponse,
    RealtimeWorkerSource, RealtimeWorkerTarget, RegisteredTrack, SelectedMediaPath,
    SubscribeTrackRequest,
};
use crate::runtime::{format_id, random_u64};
use crate::validation::{
    media_codec_name, parse_media_codec, parse_network_quality, validate_fallback_url,
};

pub(crate) async fn select_media_path(
    state: &AppState,
    room_id: RoomId,
    request: &SubscribeTrackRequest,
    source: &RegisteredTrack,
) -> Result<SelectedMediaPath, ApiError> {
    let subscriber_codecs = if request.subscriber_codecs.is_empty() {
        vec![source.codec]
    } else {
        request
            .subscriber_codecs
            .iter()
            .map(|codec| parse_media_codec(codec))
            .collect::<Result<Vec<_>, _>>()?
    };
    if subscriber_codecs
        .iter()
        .any(|codec| codec.is_audio() != source.codec.is_audio())
    {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "subscriber_codec_media_kind_mismatch",
            message: "subscriber codecs must match the source media kind".to_owned(),
        });
    }
    validate_fallback_url(request.hls_fallback_url.as_deref())?;
    let default_bitrate = source
        .encodings
        .iter()
        .map(|encoding| encoding.max_bitrate_bps)
        .max()
        .unwrap_or(500_000);
    let decision = negotiate(&NegotiationRequest {
        source: SourceDescriptor {
            source_id: format!("{}:{}", format_id(room_id.0), request.track_id),
            codec: source.codec,
            width: source.width,
            height: source.height,
            frames_per_second: source.frames_per_second,
        },
        subscriber_codecs,
        network_quality: parse_network_quality(request.network_quality.as_deref())?,
        allow_transcoding: request.allow_transcoding,
        hls_fallback_url: request.hls_fallback_url.clone(),
        target_width: request.target_width.unwrap_or(source.width),
        target_height: request.target_height.unwrap_or(source.height),
        target_frames_per_second: request
            .target_frames_per_second
            .unwrap_or(source.frames_per_second),
        target_bitrate_bps: request.target_bitrate_bps.unwrap_or(default_bitrate),
    });
    match decision {
        NegotiationDecision::DirectForward { codec } => Ok(SelectedMediaPath::Realtime {
            path: "direct",
            track_id: request.track_id,
            codec,
            transcode_job_id: None,
        }),
        NegotiationDecision::Transcode(specification) => {
            let (job_id, active) =
                acquire_transcode(state, room_id, request.track_id, source, specification).await?;
            Ok(SelectedMediaPath::Realtime {
                path: "transcode",
                track_id: active.output_track_id,
                codec: active.output_codec,
                transcode_job_id: Some(job_id),
            })
        }
        NegotiationDecision::HlsFallback { url } => Ok(SelectedMediaPath::Hls { url }),
        NegotiationDecision::RejectCodecIncompatible => Err(ApiError {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "codec_incompatible",
            message: "no direct, bounded transcode, or HLS media path is available".to_owned(),
        }),
    }
}

async fn acquire_transcode(
    state: &AppState,
    room_id: RoomId,
    source_track_id: u64,
    source: &RegisteredTrack,
    specification: TranscodeSpec,
) -> Result<(TranscodeJobId, ActiveTranscode), ApiError> {
    let mut registry = state.transcodes.lock().await;
    let allocation = registry
        .coordinator
        .acquire(format_id(room_id.0), specification.clone())
        .map_err(|error| transcode_error(&error))?;
    if allocation.reused {
        let active = registry
            .active
            .get(&allocation.job_id)
            .cloned()
            .ok_or(ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "transcode_state_inconsistent",
                message: "shared transcoder allocation is not ready".to_owned(),
            })?;
        return Ok((allocation.job_id, active));
    }
    match provision_transcode(
        state,
        room_id,
        source_track_id,
        source,
        &specification,
        allocation.job_id,
        &registry.active,
    )
    .await
    {
        Ok(active) => {
            registry
                .coordinator
                .update_state(
                    allocation.job_id,
                    TranscodeJobState::Ready {
                        output_id: active.output_track_id.to_string(),
                    },
                )
                .map_err(|error| transcode_error(&error))?;
            registry.active.insert(allocation.job_id, active.clone());
            state.metrics.transcoder_jobs.add(1);
            Ok((allocation.job_id, active))
        }
        Err(error) => {
            let _ = registry.coordinator.release(allocation.job_id);
            Err(error)
        }
    }
}

async fn provision_transcode(
    state: &AppState,
    room_id: RoomId,
    source_track_id: u64,
    source: &RegisteredTrack,
    specification: &TranscodeSpec,
    job_id: TranscodeJobId,
    active: &HashMap<TranscodeJobId, ActiveTranscode>,
) -> Result<ActiveTranscode, ApiError> {
    let output_payload_type = 120;
    let target_codec = specification.target_codec;
    let (output_track_id, output_ssrc, ingress) = create_transcode_ingress(
        state,
        room_id,
        source,
        specification,
        active,
        output_payload_type,
    )
    .await?;
    let placement_id = format!("{}-{}", format_id(room_id.0), job_id.0);
    let placement = match worker_control_placement(state, &placement_id).await {
        Ok(placement) => placement,
        Err(error) => {
            media_control_internal_delete(
                state,
                room_id,
                &format!("/v1/sfu/transcode-ingresses/{}", ingress.ingress_id),
            )
            .await;
            return Err(error);
        }
    };
    let worker_request = TranscodeWorkerRequest {
        room_id,
        source_track_id,
        source,
        specification,
        job_id,
        placement: &placement,
        placement_id: &placement_id,
        destination: ingress.destination,
        payload_type: output_payload_type,
        output_ssrc,
    };
    let worker = match start_transcode_worker(state, worker_request).await {
        Ok(worker) => worker,
        Err(error) => {
            remove_worker_placement_generation(state, &placement_id, placement.generation).await;
            media_control_internal_delete(
                state,
                room_id,
                &format!("/v1/sfu/transcode-ingresses/{}", ingress.ingress_id),
            )
            .await;
            return Err(error);
        }
    };
    let source_ssrc = highest_source_ssrc(source);
    if let Err(error) = media_control_post(
        state,
        room_id,
        "/v1/sfu/recordings",
        &MediaRecordingSink {
            room_id: format_id(room_id.0),
            track_id: source_track_id,
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
        remove_worker_placement_generation(state, &placement_id, placement.generation).await;
        media_control_internal_delete(
            state,
            room_id,
            &format!("/v1/sfu/transcode-ingresses/{}", ingress.ingress_id),
        )
        .await;
        return Err(error);
    }
    Ok(ActiveTranscode {
        room_id,
        worker_job_id: worker.job_id,
        worker_endpoint: placement.endpoint,
        worker_placement_id: placement_id,
        worker_placement_generation: placement.generation,
        ingress_id: ingress.ingress_id,
        output_track_id,
        output_ssrc,
        output_codec: target_codec,
        source_track_id,
        source_destination: worker.source_destination,
        output_destination: ingress.destination,
    })
}

pub(crate) fn highest_source_ssrc(source: &RegisteredTrack) -> Option<u32> {
    source
        .encodings
        .iter()
        .max_by_key(|encoding| (encoding.spatial_layer, encoding.max_bitrate_bps))
        .map(|encoding| encoding.ssrc)
}

struct TranscodeWorkerRequest<'a> {
    room_id: RoomId,
    source_track_id: u64,
    source: &'a RegisteredTrack,
    specification: &'a TranscodeSpec,
    job_id: TranscodeJobId,
    placement: &'a ServicePlacement,
    placement_id: &'a str,
    destination: SocketAddr,
    payload_type: u8,
    output_ssrc: u32,
}

async fn start_transcode_worker(
    state: &AppState,
    request: TranscodeWorkerRequest<'_>,
) -> Result<RealtimeWorkerJobResponse, ApiError> {
    let TranscodeWorkerRequest {
        room_id,
        source_track_id,
        source,
        specification,
        job_id,
        placement,
        placement_id,
        destination,
        payload_type,
        output_ssrc,
    } = request;
    worker_control_json_post(
        state,
        &placement.endpoint,
        "/v1/realtime-jobs",
        &CreateRealtimeWorkerJob {
            job_key: format!("rtc-{}-{}", format_id(room_id.0), job_id.0),
            placement_resource_id: placement_id.to_owned(),
            placement_generation: placement.generation,
            source: RealtimeWorkerSource {
                track_id: source_track_id,
                kind: source.kind.clone(),
                codec: source.codec_name.clone(),
                payload_type: source.payload_type,
                clock_rate: source.clock_rate,
                channels: source.codec.is_audio().then_some(2),
                fmtp: None,
            },
            target: RealtimeWorkerTarget {
                codec: media_codec_name(specification.target_codec),
                destination,
                payload_type,
                ssrc: output_ssrc,
                width: specification.width,
                height: specification.height,
                frames_per_second: specification.frames_per_second,
                bitrate_bps: specification.bitrate_bps,
            },
        },
    )
    .await
}

async fn create_transcode_ingress(
    state: &AppState,
    room_id: RoomId,
    source: &RegisteredTrack,
    specification: &TranscodeSpec,
    active: &HashMap<TranscodeJobId, ActiveTranscode>,
    output_payload_type: u8,
) -> Result<(u64, u32, MediaTranscodeIngressResponse), ApiError> {
    let output_track_id = unique_transcode_track_id(state, active)?;
    let output_ssrc = nonzero_random_u32()?;
    let target_codec = specification.target_codec;
    let ingress = media_control_json_post(
        state,
        room_id,
        "/v1/sfu/transcode-ingresses",
        &CreateMediaTranscodeIngress {
            room_id: format_id(room_id.0),
            participant_id: format_id(source.participant),
            track_id: output_track_id,
            kind: source.kind.clone(),
            codec: media_codec_name(target_codec),
            clock_rate: if target_codec.is_audio() {
                48_000
            } else {
                90_000
            },
            payload_type: output_payload_type,
            ssrc: output_ssrc,
            max_bitrate_bps: specification.bitrate_bps,
        },
    )
    .await?;
    Ok((output_track_id, output_ssrc, ingress))
}

pub(crate) async fn release_transcode(state: &AppState, job_id: TranscodeJobId) {
    let (active, remaining) = {
        let mut registry = state.transcodes.lock().await;
        let final_reference = registry.coordinator.references(job_id) == Some(1);
        let _ = registry.coordinator.release(job_id);
        if final_reference {
            registry.health_failures.remove(&job_id);
        }
        (
            final_reference
                .then(|| registry.active.remove(&job_id))
                .flatten(),
            registry.active.len(),
        )
    };
    state
        .metrics
        .transcoder_jobs
        .set(i64::try_from(remaining).unwrap_or(i64::MAX));
    if let Some(active) = active {
        teardown_active_transcode(state, active).await;
    }
}

pub(crate) async fn teardown_transcodes_for_source(
    state: &AppState,
    room_id: RoomId,
    source_track_id: u64,
) {
    let removed = {
        let mut registry = state.transcodes.lock().await;
        let job_ids = registry
            .active
            .iter()
            .filter_map(|(job_id, active)| {
                (active.room_id == room_id && active.source_track_id == source_track_id)
                    .then_some(*job_id)
            })
            .collect::<Vec<_>>();
        registry
            .subscriptions
            .retain(|_, job_id| !job_ids.contains(job_id));
        let mut removed = Vec::with_capacity(job_ids.len());
        for job_id in job_ids {
            while registry
                .coordinator
                .references(job_id)
                .is_some_and(|count| count > 0)
            {
                let _ = registry.coordinator.release(job_id);
            }
            if let Some(active) = registry.active.remove(&job_id) {
                removed.push(active);
            }
            registry.health_failures.remove(&job_id);
        }
        state
            .metrics
            .transcoder_jobs
            .set(i64::try_from(registry.active.len()).unwrap_or(i64::MAX));
        removed
    };
    for active in removed {
        teardown_active_transcode(state, active).await;
    }
}

async fn teardown_active_transcode(state: &AppState, active: ActiveTranscode) {
    let _ = media_control_delete_json(
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
    .await;
    internal_delete(
        state,
        &active.worker_endpoint,
        &state.worker_control_token,
        &format!("/v1/realtime-jobs/{}", active.worker_job_id),
    )
    .await;
    remove_worker_placement(state, &active.worker_placement_id).await;
    media_control_internal_delete(
        state,
        active.room_id,
        &format!("/v1/sfu/transcode-ingresses/{}", active.ingress_id),
    )
    .await;
}

fn unique_transcode_track_id(
    state: &AppState,
    active: &HashMap<TranscodeJobId, ActiveTranscode>,
) -> Result<u64, ApiError> {
    for _ in 0..8 {
        let candidate = random_u64()?;
        if candidate != 0
            && !state
                .tracks
                .read()
                .map_err(lock_error)?
                .keys()
                .any(|(_, track_id)| *track_id == candidate)
            && active
                .values()
                .all(|transcode| transcode.output_track_id != candidate)
        {
            return Ok(candidate);
        }
    }
    Err(ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "transcode_track_id_collision",
        message: "could not allocate a unique transcode output track".to_owned(),
    })
}

fn nonzero_random_u32() -> Result<u32, ApiError> {
    for _ in 0..8 {
        let candidate =
            u32::try_from(random_u64()? & u64::from(u32::MAX)).map_err(internal_error)?;
        if candidate != 0 {
            return Ok(candidate);
        }
    }
    Err(ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "transcode_ssrc_exhausted",
        message: "could not allocate a non-zero transcode SSRC".to_owned(),
    })
}

fn transcode_error(error: &fluvora_transcode_bridge::TranscodeError) -> ApiError {
    let status = match error {
        fluvora_transcode_bridge::TranscodeError::GlobalQuota
        | fluvora_transcode_bridge::TranscodeError::TenantQuota => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    ApiError {
        status,
        code: "transcode_allocation_failed",
        message: error.to_string(),
    }
}

pub(crate) fn transcode_job_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "transcode_job_not_found",
        message: "active transcode job does not exist".to_owned(),
    }
}
