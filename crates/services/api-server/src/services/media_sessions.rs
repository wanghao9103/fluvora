use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, ETAG, LOCATION};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use fluvora_domain::RoomId;

use crate::control_client::{internal_url, media_control_endpoint};
use crate::error::{ApiError, internal_error, lock_error};
use crate::models::{AppState, MediaSessionProvision};
use crate::protocol::{ProtocolSession, WebRtcHttpProtocol, protocol_session_not_found};
use crate::runtime::format_id;

pub(crate) async fn provision_media_session(
    state: &AppState,
    room_id: RoomId,
    provision: MediaSessionProvision,
) -> Result<(), ApiError> {
    let endpoint = media_control_endpoint(state, room_id).await?;
    let url = internal_url(&endpoint, "/v1/sessions")?;
    let response = state
        .http_client
        .post(url)
        .bearer_auth(state.media_control_token.as_ref())
        .json(&provision)
        .send()
        .await
        .map_err(|error| {
            eprintln!("media-node session provisioning failed: {error}");
            ApiError {
                status: StatusCode::BAD_GATEWAY,
                code: "media_node_unavailable",
                message: "media node is unavailable".to_owned(),
            }
        })?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "media_session_rejected",
            message: format!("media node returned {}", response.status()),
        })
    }
}

pub(crate) fn protocol_created_response(
    room_id: RoomId,
    session_id: u64,
    protocol: WebRtcHttpProtocol,
    answer_sdp: String,
) -> Result<Response, ApiError> {
    let location = format!(
        "/v1/rooms/{}/{}/{session_id}",
        format_id(room_id.0),
        protocol.path_name()
    );
    Response::builder()
        .status(StatusCode::CREATED)
        .header(CONTENT_TYPE, "application/sdp")
        .header(
            LOCATION,
            HeaderValue::from_str(&location).map_err(internal_error)?,
        )
        .header(ETAG, "\"1\"")
        .header("accept-patch", "application/trickle-ice-sdpfrag")
        .body(Body::from(answer_sdp))
        .map_err(internal_error)
}

pub(crate) fn authorized_protocol_session(
    state: &AppState,
    room_id: RoomId,
    session_id: u64,
    participant: u128,
    protocol: WebRtcHttpProtocol,
) -> Result<ProtocolSession, ApiError> {
    let sessions = state.protocol_sessions.read().map_err(lock_error)?;
    let session = sessions
        .get(&session_id)
        .ok_or_else(protocol_session_not_found)?;
    if session.room_id != room_id
        || session.participant != participant
        || session.protocol != protocol
    {
        return Err(protocol_session_not_found());
    }
    Ok(session.clone())
}
