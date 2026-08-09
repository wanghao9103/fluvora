use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use fluvora_auth::Scopes;
use fluvora_domain::RoomId;

use crate::control_client::{delete_media_session, media_control_delete_json, media_control_post};
use crate::error::{ApiError, lock_error};
use crate::models::{
    AppState, LayerRequest, MediaLayerRequest, MediaPublishTrack, MediaSubscribeTrack,
    MediaUnpublishTrack, MediaUnsubscribeTrack, PublishTrackRequest, RegisteredSubscription,
    RegisteredTrack, SelectedMediaPath, SubscribeTrackRequest, SubscribeTrackResponse,
};
use crate::runtime::format_id;
use crate::services::{
    authenticate, release_transcode, remember_side_effect, require_publishing,
    require_realtime_server_room, require_room_member, select_media_path, side_effect_applied,
    teardown_transcodes_for_source,
};
use crate::signals::append_signal;
use crate::validation::{
    idempotency_key, media_codec_name, parse_media_codec, parse_room_id, validate_publish_track,
};

pub(crate) async fn cleanup_publisher_tracks(state: &AppState, room_id: RoomId, participant: u128) {
    let track_ids = state
        .tracks
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|((track_room, track_id), track)| {
            (*track_room == room_id && track.participant == participant).then_some(*track_id)
        })
        .collect::<Vec<_>>();
    for track_id in track_ids {
        if let Err(error) = remove_source_track(state, room_id, participant, track_id).await {
            eprintln!(
                "failed to clean publisher track {} in room {}: {}",
                track_id,
                format_id(room_id.0),
                error.message
            );
        }
    }
}

pub(crate) async fn cleanup_participant_media(
    state: &AppState,
    room_id: RoomId,
    participant: u128,
) {
    cleanup_publisher_tracks(state, room_id, participant).await;
    let subscriptions = state
        .subscriptions
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .filter_map(|(subscription_room, subscriber, subscription_id)| {
            (*subscription_room == room_id && *subscriber == participant)
                .then_some(*subscription_id)
        })
        .collect::<Vec<_>>();
    for subscription_id in subscriptions {
        cleanup_subscription(state, room_id, participant, subscription_id).await;
    }
    let session_ids = state
        .protocol_sessions
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|(session_id, session)| {
            (session.room_id == room_id && session.participant == participant)
                .then_some(*session_id)
        })
        .collect::<Vec<_>>();
    for session_id in session_ids {
        if let Err(error) = delete_media_session(state, room_id, session_id).await {
            eprintln!(
                "failed to clean media session {session_id}: {}",
                error.message
            );
            continue;
        }
        state
            .protocol_sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }
    state.metrics.active_sessions.set(
        i64::try_from(
            state
                .protocol_sessions
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
        )
        .unwrap_or(i64::MAX),
    );
}

async fn cleanup_subscription(
    state: &AppState,
    room_id: RoomId,
    participant: u128,
    subscription_id: u64,
) {
    let result = media_control_delete_json(
        state,
        room_id,
        &format!("/v1/sfu/subscriptions/{subscription_id}"),
        &MediaUnsubscribeTrack {
            room_id: format_id(room_id.0),
            participant_id: format_id(participant),
        },
    )
    .await;
    if let Err(error) = result {
        eprintln!(
            "failed to clean subscription {subscription_id} in room {}: {}",
            format_id(room_id.0),
            error.message
        );
    }
    let job_id = state.transcodes.lock().await.subscriptions.remove(&(
        room_id,
        participant,
        subscription_id,
    ));
    state
        .subscriptions
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(room_id, participant, subscription_id));
    if let Some(job_id) = job_id {
        release_transcode(state, job_id).await;
    }
}

pub(crate) async fn cleanup_room_media(state: &AppState, room_id: RoomId) {
    let mut participants = state
        .tracks
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|((track_room, _), track)| {
            (*track_room == room_id).then_some(track.participant)
        })
        .collect::<HashSet<_>>();
    participants.extend(
        state
            .subscriptions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter_map(|(subscription_room, participant, _)| {
                (*subscription_room == room_id).then_some(*participant)
            }),
    );
    participants.extend(
        state
            .protocol_sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter_map(|session| (session.room_id == room_id).then_some(session.participant)),
    );
    for participant in participants {
        cleanup_participant_media(state, room_id, participant).await;
    }
}

pub(crate) async fn register_track(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PublishTrackRequest>,
) -> Result<StatusCode, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH),
        Some(room_id),
    )?;
    require_realtime_server_room(&state, room_id)?;
    require_publishing(&state, room_id, claims.subject)?;
    let command_id = idempotency_key(&headers)?;
    if side_effect_applied(&state, room_id, command_id).await? {
        return Ok(StatusCode::OK);
    }
    validate_publish_track(&request)?;
    media_control_post(
        &state,
        room_id,
        "/v1/sfu/tracks",
        &MediaPublishTrack {
            room_id: format_id(room_id.0),
            participant_id: format_id(claims.subject),
            track_id: request.track_id,
            kind: request.kind.clone(),
            codec: request.codec.clone(),
            clock_rate: request.clock_rate,
            payload_type: request.payload_type,
            encodings: request.encodings.clone(),
        },
    )
    .await?;
    let codec = parse_media_codec(&request.codec)?;
    state.tracks.write().map_err(lock_error)?.insert(
        (room_id, request.track_id),
        RegisteredTrack {
            participant: claims.subject,
            kind: request.kind.clone(),
            codec,
            codec_name: request.codec.to_ascii_lowercase(),
            clock_rate: request.clock_rate,
            payload_type: request.payload_type,
            encodings: request.encodings.clone(),
            width: request.width,
            height: request.height,
            frames_per_second: request.frames_per_second,
        },
    );
    remember_side_effect(&state, room_id, command_id).await?;
    append_signal(
        &state,
        room_id,
        command_id,
        claims.subject,
        None,
        "media.track_published".to_owned(),
        serde_json::json!({
            "track_id": request.track_id,
            "kind": request.kind,
            "codec": request.codec,
            "encodings": request.encodings,
        }),
    )
    .await?;
    Ok(StatusCode::CREATED)
}

pub(crate) async fn unpublish_track(
    State(state): State<AppState>,
    Path((room_id, track_id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(
        &state,
        &headers,
        Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH),
        Some(room_id),
    )?;
    let command_id = idempotency_key(&headers)?;
    let track = state
        .tracks
        .read()
        .map_err(lock_error)?
        .get(&(room_id, track_id))
        .cloned()
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "track_not_found",
            message: "published source track does not exist".to_owned(),
        })?;
    if track.participant != claims.subject {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "track_not_owned",
            message: "published track belongs to another participant".to_owned(),
        });
    }
    remove_source_track(&state, room_id, claims.subject, track_id).await?;
    append_signal(
        &state,
        room_id,
        command_id,
        claims.subject,
        None,
        "media.track_unpublished".to_owned(),
        serde_json::json!({ "track_id": track_id }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_source_track(
    state: &AppState,
    room_id: RoomId,
    participant: u128,
    track_id: u64,
) -> Result<(), ApiError> {
    media_control_delete_json(
        state,
        room_id,
        &format!("/v1/sfu/tracks/{track_id}"),
        &MediaUnpublishTrack {
            room_id: format_id(room_id.0),
            participant_id: format_id(participant),
        },
    )
    .await?;
    state
        .tracks
        .write()
        .map_err(lock_error)?
        .remove(&(room_id, track_id));
    state.subscriptions.write().map_err(lock_error)?.retain(
        |(subscription_room, _, _), subscription| {
            *subscription_room != room_id || subscription.source_track_id != track_id
        },
    );
    teardown_transcodes_for_source(state, room_id, track_id).await;
    Ok(())
}

pub(crate) async fn subscribe_track(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SubscribeTrackRequest>,
) -> Result<(StatusCode, Json<SubscribeTrackResponse>), ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_realtime_server_room(&state, room_id)?;
    require_room_member(&state, room_id, claims.subject)?;
    let command_id = idempotency_key(&headers)?;
    if side_effect_applied(&state, room_id, command_id).await? {
        return Ok(duplicate_subscription_response(request.track_id));
    }
    let source = state
        .tracks
        .read()
        .map_err(lock_error)?
        .get(&(room_id, request.track_id))
        .cloned()
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "track_not_found",
            message: "published source track does not exist".to_owned(),
        })?;
    let selected = select_media_path(&state, room_id, &request, &source).await?;
    let SelectedMediaPath::Realtime {
        path,
        track_id: selected_track_id,
        codec,
        transcode_job_id,
    } = selected
    else {
        if let SelectedMediaPath::Hls { url } = selected {
            remember_side_effect(&state, room_id, command_id).await?;
            return Ok(hls_subscription_response(request.track_id, url));
        }
        unreachable!("selected media path variants are exhaustive");
    };
    let subscribe_result = media_control_post(
        &state,
        room_id,
        "/v1/sfu/subscriptions",
        &MediaSubscribeTrack {
            room_id: format_id(room_id.0),
            participant_id: format_id(claims.subject),
            subscription_id: request.subscription_id,
            track_id: selected_track_id,
            output_ssrc: request.output_ssrc,
            output_payload_type: request.output_payload_type,
            spatial_layer: if transcode_job_id.is_some() {
                0
            } else {
                request.spatial_layer
            },
            temporal_layer: request.temporal_layer,
            initial_sequence_number: request.initial_sequence_number,
            initial_timestamp: request.initial_timestamp,
            extension_rewrites: request.extension_rewrites,
            transport_wide_extension_id: request.transport_wide_extension_id,
        },
    )
    .await;
    if let Err(error) = subscribe_result {
        if let Some(job_id) = transcode_job_id {
            release_transcode(&state, job_id).await;
        }
        return Err(error);
    }
    if let Some(job_id) = transcode_job_id {
        state
            .transcodes
            .lock()
            .await
            .subscriptions
            .insert((room_id, claims.subject, request.subscription_id), job_id);
    }
    state.subscriptions.write().map_err(lock_error)?.insert(
        (room_id, claims.subject, request.subscription_id),
        RegisteredSubscription {
            source_track_id: request.track_id,
        },
    );
    remember_side_effect(&state, room_id, command_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(SubscribeTrackResponse {
            path,
            source_track_id: request.track_id,
            selected_track_id: Some(selected_track_id),
            codec: Some(media_codec_name(codec)),
            transcode_job_id: transcode_job_id.map(|job_id| job_id.0),
            fallback_url: None,
        }),
    ))
}

fn hls_subscription_response(
    source_track_id: u64,
    url: String,
) -> (StatusCode, Json<SubscribeTrackResponse>) {
    (
        StatusCode::OK,
        Json(SubscribeTrackResponse {
            path: "hls",
            source_track_id,
            selected_track_id: None,
            codec: None,
            transcode_job_id: None,
            fallback_url: Some(url),
        }),
    )
}

fn duplicate_subscription_response(
    source_track_id: u64,
) -> (StatusCode, Json<SubscribeTrackResponse>) {
    (
        StatusCode::OK,
        Json(SubscribeTrackResponse {
            path: "existing",
            source_track_id,
            selected_track_id: None,
            codec: None,
            transcode_job_id: None,
            fallback_url: None,
        }),
    )
}

pub(crate) async fn unsubscribe_track(
    State(state): State<AppState>,
    Path((room_id, subscription_id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    media_control_delete_json(
        &state,
        room_id,
        &format!("/v1/sfu/subscriptions/{subscription_id}"),
        &MediaUnsubscribeTrack {
            room_id: format_id(room_id.0),
            participant_id: format_id(claims.subject),
        },
    )
    .await?;
    let job_id = state.transcodes.lock().await.subscriptions.remove(&(
        room_id,
        claims.subject,
        subscription_id,
    ));
    if let Some(job_id) = job_id {
        release_transcode(&state, job_id).await;
    }
    state.subscriptions.write().map_err(lock_error)?.remove(&(
        room_id,
        claims.subject,
        subscription_id,
    ));
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn set_subscription_layer(
    State(state): State<AppState>,
    Path((room_id, subscription_id)): Path<(String, u64)>,
    headers: HeaderMap,
    Json(request): Json<LayerRequest>,
) -> Result<StatusCode, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_realtime_server_room(&state, room_id)?;
    let command_id = idempotency_key(&headers)?;
    if side_effect_applied(&state, room_id, command_id).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    media_control_post(
        &state,
        room_id,
        &format!("/v1/sfu/subscriptions/{subscription_id}/layer"),
        &MediaLayerRequest {
            room_id: format_id(room_id.0),
            participant_id: format_id(claims.subject),
            spatial_layer: request.spatial_layer,
            temporal_layer: request.temporal_layer,
        },
    )
    .await?;
    remember_side_effect(&state, room_id, command_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
