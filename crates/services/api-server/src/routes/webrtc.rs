use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, ETAG};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fluvora_auth::Scopes;
use fluvora_domain::RoomId;
use fluvora_sdp::{AnswerConfig, CodecCapability, SessionDescription, create_sfu_answer};

use crate::control_client::{delete_media_session, media_candidate, media_control_post};
use crate::error::{ApiError, internal_error, lock_error};
use crate::models::{
    AppState, MediaSessionIceRestart, MediaSessionProvision, NegotiatedSession, OfferRequest,
    OfferResponse,
};
use crate::protocol::{
    MAX_SDP_BODY_BYTES, MAX_TRICKLE_FRAGMENT_BYTES, ProtocolSession, TrickleFragment,
    WebRtcHttpProtocol, invalid_sdp_body, payload_too_large, protocol_session_not_found,
    require_content_type, require_current_etag, validate_protocol_direction,
    validate_trickle_fragment,
};
use crate::runtime::{
    MAX_PROTOCOL_SESSIONS, format_id, random_credential, random_sdp_session_id, random_u64,
};
use crate::services::{
    authenticate, authorized_protocol_session, protocol_created_response, provision_media_session,
    require_publishing, require_realtime_server_room, require_room_member,
};
use crate::validation::{invalid_sdp_field, parse_room_id, supported_extension_uris};

pub(crate) async fn answer_offer(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<OfferRequest>,
) -> Result<Json<OfferResponse>, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    require_realtime_server_room(&state, room_id)?;
    if request.sdp.len() > MAX_SDP_BODY_BYTES {
        return Err(payload_too_large(MAX_SDP_BODY_BYTES));
    }
    let negotiated = negotiate_offer(&state, room_id, claims.subject, &request.sdp, None).await?;
    Ok(Json(OfferResponse {
        session_id: negotiated.session_id.to_string(),
        answer_sdp: negotiated.answer_sdp,
    }))
}

pub(crate) async fn create_whip_session(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    create_protocol_session(state, room_id, headers, body, WebRtcHttpProtocol::Whip).await
}

pub(crate) async fn create_whep_session(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    create_protocol_session(state, room_id, headers, body, WebRtcHttpProtocol::Whep).await
}

async fn create_protocol_session(
    state: AppState,
    room_id: String,
    headers: HeaderMap,
    body: Body,
    protocol: WebRtcHttpProtocol,
) -> Result<Response, ApiError> {
    require_content_type(&headers, "application/sdp")?;
    let room_id = parse_room_id(&room_id)?;
    let scopes = if protocol == WebRtcHttpProtocol::Whip {
        Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH)
    } else {
        Scopes::ROOM_JOIN
    };
    let claims = authenticate(&state, &headers, scopes, Some(room_id))?;
    require_room_member(&state, room_id, claims.subject)?;
    require_realtime_server_room(&state, room_id)?;
    if protocol == WebRtcHttpProtocol::Whip {
        require_publishing(&state, room_id, claims.subject)?;
    }
    let bytes = to_bytes(body, MAX_SDP_BODY_BYTES)
        .await
        .map_err(|_| payload_too_large(MAX_SDP_BODY_BYTES))?;
    let offer = std::str::from_utf8(&bytes).map_err(|_| invalid_sdp_body())?;
    let negotiated =
        negotiate_offer(&state, room_id, claims.subject, offer, Some(protocol)).await?;
    let session_id = negotiated.session_id;
    let inserted = {
        let mut sessions = state.protocol_sessions.write().map_err(lock_error)?;
        if sessions.len() >= MAX_PROTOCOL_SESSIONS {
            false
        } else {
            sessions.insert(
                session_id,
                ProtocolSession {
                    room_id,
                    participant: claims.subject,
                    protocol,
                    local_username_fragment: negotiated.local_username_fragment,
                    local_password: negotiated.local_password,
                    remote_username_fragment: negotiated.remote_username_fragment,
                    remote_password: negotiated.remote_password,
                    etag_version: 1,
                },
            );
            true
        }
    };
    if !inserted {
        if let Err(error) = delete_media_session(&state, room_id, session_id).await {
            eprintln!(
                "failed to roll back media session {session_id} after protocol capacity rejection: {}",
                error.message
            );
        }
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "protocol_session_capacity",
            message: "WHIP/WHEP session registry is full".to_owned(),
        });
    }
    protocol_created_response(room_id, session_id, protocol, negotiated.answer_sdp)
}

async fn negotiate_offer(
    state: &AppState,
    room_id: RoomId,
    participant: u128,
    sdp: &str,
    protocol: Option<WebRtcHttpProtocol>,
) -> Result<NegotiatedSession, ApiError> {
    let offer = SessionDescription::parse(sdp)
        .and_then(|offer| {
            offer.validate_webrtc_offer()?;
            Ok(offer)
        })
        .map_err(|error| ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_sdp_offer",
            message: error.to_string(),
        })?;
    if let Some(protocol) = protocol {
        validate_protocol_direction(&offer, protocol)?;
    }
    let media = offer
        .media
        .iter()
        .find(|media| media.port != 0)
        .ok_or_else(|| ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "inactive_sdp_offer",
            message: "SDP offer has no active media section".to_owned(),
        })?;
    let remote_username_fragment = offer
        .effective_attribute(media, "ice-ufrag")
        .ok_or_else(|| invalid_sdp_field("ice-ufrag"))?
        .to_owned();
    let remote_password = offer
        .effective_attribute(media, "ice-pwd")
        .ok_or_else(|| invalid_sdp_field("ice-pwd"))?
        .to_owned();
    let fingerprint = offer
        .effective_attribute(media, "fingerprint")
        .ok_or_else(|| invalid_sdp_field("fingerprint"))?;
    let (fingerprint_algorithm, expected_peer_fingerprint) = fingerprint
        .split_once(char::is_whitespace)
        .ok_or_else(|| invalid_sdp_field("fingerprint"))?;
    if !fingerprint_algorithm.eq_ignore_ascii_case("sha-256") {
        return Err(invalid_sdp_field("fingerprint algorithm"));
    }
    let session_id = random_sdp_session_id()?;
    let local_username_fragment = random_credential(8)?;
    let local_password = random_credential(24)?;
    let tie_breaker = random_u64()?;
    let mut config = AnswerConfig::mvp(
        session_id,
        local_username_fragment.clone(),
        local_password.clone(),
        state.dtls_fingerprint.to_string(),
    );
    config.audio_codecs = vec![CodecCapability::new("opus", 48_000, 2)];
    config.video_codecs = vec![
        CodecCapability::new("VP8", 90_000, 1),
        CodecCapability::new("VP9", 90_000, 1),
        CodecCapability::new("H264", 90_000, 1),
        CodecCapability::new("AV1", 90_000, 1),
    ];
    config.extension_uris = supported_extension_uris();
    config.accept_data_channel = true;
    if let Some(candidate) = media_candidate(state, room_id).await? {
        config.candidates.push(candidate);
    }
    let answer_sdp = create_sfu_answer(&offer, &config).map_err(|error| ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "sdp_negotiation_failed",
        message: error.to_string(),
    })?;
    provision_media_session(
        state,
        room_id,
        MediaSessionProvision {
            session_id: session_id.to_string(),
            room_id: format_id(room_id.0),
            participant_id: format_id(participant),
            local_username_fragment: local_username_fragment.clone(),
            local_password: local_password.clone(),
            remote_username_fragment: remote_username_fragment.clone(),
            remote_password: remote_password.clone(),
            expected_peer_fingerprint: expected_peer_fingerprint.trim().to_owned(),
            tie_breaker,
        },
    )
    .await?;
    state.metrics.active_sessions.add(1);
    Ok(NegotiatedSession {
        session_id,
        answer_sdp,
        local_username_fragment,
        local_password,
        remote_username_fragment,
        remote_password,
    })
}

pub(crate) async fn patch_whip_session(
    State(state): State<AppState>,
    Path((room_id, session_id)): Path<(String, u64)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    patch_protocol_session(
        state,
        room_id,
        session_id,
        headers,
        body,
        WebRtcHttpProtocol::Whip,
    )
    .await
}

pub(crate) async fn patch_whep_session(
    State(state): State<AppState>,
    Path((room_id, session_id)): Path<(String, u64)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    patch_protocol_session(
        state,
        room_id,
        session_id,
        headers,
        body,
        WebRtcHttpProtocol::Whep,
    )
    .await
}

async fn patch_protocol_session(
    state: AppState,
    room_id: String,
    session_id: u64,
    headers: HeaderMap,
    body: Body,
    protocol: WebRtcHttpProtocol,
) -> Result<Response, ApiError> {
    let _update_guard = state.protocol_updates.lock().await;
    require_content_type(&headers, "application/trickle-ice-sdpfrag")?;
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    let session =
        authorized_protocol_session(&state, room_id, session_id, claims.subject, protocol)?;
    require_current_etag(&headers, session.etag_version)?;
    let bytes = to_bytes(body, MAX_TRICKLE_FRAGMENT_BYTES)
        .await
        .map_err(|_| payload_too_large(MAX_TRICKLE_FRAGMENT_BYTES))?;
    let fragment = std::str::from_utf8(&bytes).map_err(|_| invalid_sdp_body())?;
    match validate_trickle_fragment(fragment, &session)? {
        TrickleFragment::Candidates => Ok(StatusCode::NO_CONTENT.into_response()),
        TrickleFragment::IceRestart {
            remote_username_fragment,
            remote_password,
        } => {
            restart_protocol_ice(
                &state,
                session_id,
                &session,
                remote_username_fragment,
                remote_password,
            )
            .await
        }
    }
}

async fn restart_protocol_ice(
    state: &AppState,
    session_id: u64,
    previous: &ProtocolSession,
    remote_username_fragment: String,
    remote_password: String,
) -> Result<Response, ApiError> {
    let local_username_fragment = random_credential(8)?;
    let local_password = random_credential(24)?;
    media_control_post(
        state,
        previous.room_id,
        &format!("/v1/sessions/{session_id}/ice-restart"),
        &MediaSessionIceRestart {
            local_username_fragment: local_username_fragment.clone(),
            local_password: local_password.clone(),
            remote_username_fragment: remote_username_fragment.clone(),
            remote_password: remote_password.clone(),
            tie_breaker: random_u64()?,
        },
    )
    .await?;
    let etag_version = {
        let mut sessions = state.protocol_sessions.write().map_err(lock_error)?;
        let current = sessions
            .get_mut(&session_id)
            .ok_or_else(protocol_session_not_found)?;
        if current.etag_version != previous.etag_version {
            return Err(ApiError {
                status: StatusCode::PRECONDITION_FAILED,
                code: "stale_session_etag",
                message: "WHIP/WHEP resource changed during ICE restart".to_owned(),
            });
        }
        current
            .local_username_fragment
            .clone_from(&local_username_fragment);
        current.local_password.clone_from(&local_password);
        current.remote_username_fragment = remote_username_fragment;
        current.remote_password = remote_password;
        current.etag_version = current.etag_version.checked_add(1).ok_or(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "session_etag_exhausted",
            message: "WHIP/WHEP resource version exhausted".to_owned(),
        })?;
        current.etag_version
    };
    let answer = format!("a=ice-ufrag:{local_username_fragment}\r\na=ice-pwd:{local_password}\r\n");
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/trickle-ice-sdpfrag")
        .header(ETAG, format!("\"{etag_version}\""))
        .body(Body::from(answer))
        .map_err(internal_error)
}

pub(crate) async fn delete_whip_session(
    State(state): State<AppState>,
    Path((room_id, session_id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    delete_protocol_session(
        state,
        room_id,
        session_id,
        headers,
        WebRtcHttpProtocol::Whip,
    )
    .await
}

pub(crate) async fn delete_whep_session(
    State(state): State<AppState>,
    Path((room_id, session_id)): Path<(String, u64)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    delete_protocol_session(
        state,
        room_id,
        session_id,
        headers,
        WebRtcHttpProtocol::Whep,
    )
    .await
}

async fn delete_protocol_session(
    state: AppState,
    room_id: String,
    session_id: u64,
    headers: HeaderMap,
    protocol: WebRtcHttpProtocol,
) -> Result<StatusCode, ApiError> {
    let room_id = parse_room_id(&room_id)?;
    let claims = authenticate(&state, &headers, Scopes::ROOM_JOIN, Some(room_id))?;
    authorized_protocol_session(&state, room_id, session_id, claims.subject, protocol)?;
    delete_media_session(&state, room_id, session_id).await?;
    state
        .protocol_sessions
        .write()
        .map_err(lock_error)?
        .remove(&session_id);
    state.metrics.active_sessions.add(-1);
    Ok(StatusCode::NO_CONTENT)
}
