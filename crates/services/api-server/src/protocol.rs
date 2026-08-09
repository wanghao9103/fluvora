use axum::http::header::{CONTENT_TYPE, IF_MATCH};
use axum::http::{HeaderMap, StatusCode};
use fluvora_domain::RoomId;
use fluvora_sdp::{Direction, MediaKind, SessionDescription};

use crate::error::ApiError;

pub(super) const MAX_SDP_BODY_BYTES: usize = 256 * 1_024;
pub(super) const MAX_TRICKLE_FRAGMENT_BYTES: usize = 64 * 1_024;

const MAX_SDP_FRAGMENT_LINE_BYTES: usize = 2_048;
const MIN_ICE_USERNAME_FRAGMENT_BYTES: usize = 4;
const MAX_ICE_USERNAME_FRAGMENT_BYTES: usize = 256;
const MIN_ICE_PASSWORD_BYTES: usize = 22;
const MAX_ICE_PASSWORD_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebRtcHttpProtocol {
    Whip,
    Whep,
}

impl WebRtcHttpProtocol {
    pub(super) const fn path_name(self) -> &'static str {
        match self {
            Self::Whip => "whip",
            Self::Whep => "whep",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProtocolSession {
    pub(super) room_id: RoomId,
    pub(super) participant: u128,
    pub(super) protocol: WebRtcHttpProtocol,
    pub(super) local_username_fragment: String,
    pub(super) local_password: String,
    pub(super) remote_username_fragment: String,
    pub(super) remote_password: String,
    pub(super) etag_version: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TrickleFragment {
    Candidates,
    IceRestart {
        remote_username_fragment: String,
        remote_password: String,
    },
}

pub(super) fn validate_protocol_direction(
    offer: &SessionDescription,
    protocol: WebRtcHttpProtocol,
) -> Result<(), ApiError> {
    let expected = match protocol {
        WebRtcHttpProtocol::Whip => Direction::SendOnly,
        WebRtcHttpProtocol::Whep => Direction::RecvOnly,
    };
    let active = offer
        .media
        .iter()
        .filter(|media| {
            media.port != 0 && matches!(media.kind, MediaKind::Audio | MediaKind::Video)
        })
        .collect::<Vec<_>>();
    if active.is_empty() || active.iter().any(|media| media.direction() != expected) {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_protocol_direction",
            message: format!(
                "{} offers require active RTP sections to be {}",
                protocol.path_name(),
                expected.as_str()
            ),
        });
    }
    Ok(())
}

pub(super) fn require_content_type(
    headers: &HeaderMap,
    expected: &'static str,
) -> Result<(), ApiError> {
    let matches = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected));
    if matches {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_content_type",
            message: format!("Content-Type must be {expected}"),
        })
    }
}

pub(super) fn require_current_etag(headers: &HeaderMap, version: u64) -> Result<(), ApiError> {
    let expected = format!("\"{version}\"");
    match headers.get(IF_MATCH).and_then(|value| value.to_str().ok()) {
        Some(value) if value == expected => Ok(()),
        Some(_) => Err(ApiError {
            status: StatusCode::PRECONDITION_FAILED,
            code: "stale_session_etag",
            message: "WHIP/WHEP resource ETag does not match".to_owned(),
        }),
        None => Err(ApiError {
            status: StatusCode::PRECONDITION_REQUIRED,
            code: "session_etag_required",
            message: "If-Match is required for trickle ICE".to_owned(),
        }),
    }
}

pub(super) fn validate_trickle_fragment(
    fragment: &str,
    session: &ProtocolSession,
) -> Result<TrickleFragment, ApiError> {
    let mut useful_line = false;
    let mut remote_username_fragment = None;
    let mut remote_password = None;
    for raw_line in fragment.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_SDP_FRAGMENT_LINE_BYTES
            || !line
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        {
            return Err(invalid_trickle_fragment());
        }
        if let Some(value) = line.strip_prefix("a=ice-ufrag:") {
            if remote_username_fragment.is_some() || !valid_ice_username_fragment(value) {
                return Err(invalid_trickle_fragment());
            }
            remote_username_fragment = Some(value.to_owned());
            useful_line = true;
        } else if let Some(value) = line.strip_prefix("a=ice-pwd:") {
            if remote_password.is_some() || !valid_ice_password(value) {
                return Err(invalid_trickle_fragment());
            }
            remote_password = Some(value.to_owned());
            useful_line = true;
        } else if let Some(value) = line.strip_prefix("a=candidate:") {
            if value.is_empty() {
                return Err(invalid_trickle_fragment());
            }
            useful_line = true;
        } else if line == "a=end-of-candidates" {
            useful_line = true;
        } else if let Some(value) = line.strip_prefix("a=mid:") {
            if value.is_empty() || value.len() > 256 {
                return Err(invalid_trickle_fragment());
            }
            useful_line = true;
        } else if line
            .strip_prefix("m=")
            .or_else(|| line.strip_prefix("c="))
            .is_some_and(|value| !value.is_empty())
        {
            useful_line = true;
        } else {
            return Err(invalid_trickle_fragment());
        }
    }
    if !useful_line {
        return Err(invalid_trickle_fragment());
    }
    match (remote_username_fragment, remote_password) {
        (None, None) => Ok(TrickleFragment::Candidates),
        (Some(username), Some(password))
            if username == session.remote_username_fragment
                && password == session.remote_password =>
        {
            Ok(TrickleFragment::Candidates)
        }
        (Some(username), Some(password))
            if username != session.remote_username_fragment
                && password != session.remote_password =>
        {
            Ok(TrickleFragment::IceRestart {
                remote_username_fragment: username,
                remote_password: password,
            })
        }
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "incomplete_ice_restart",
            message: "ICE restart must replace both ice-ufrag and ice-pwd".to_owned(),
        }),
    }
}

fn valid_ice_username_fragment(value: &str) -> bool {
    (MIN_ICE_USERNAME_FRAGMENT_BYTES..=MAX_ICE_USERNAME_FRAGMENT_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_ice_password(value: &str) -> bool {
    (MIN_ICE_PASSWORD_BYTES..=MAX_ICE_PASSWORD_BYTES).contains(&value.len())
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn invalid_sdp_body() -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_sdp_body",
        message: "SDP body must be valid UTF-8".to_owned(),
    }
}

pub(super) fn payload_too_large(maximum: usize) -> ApiError {
    ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "request_body_too_large",
        message: format!("request body exceeds {maximum} bytes"),
    }
}

pub(super) fn protocol_session_not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "protocol_session_not_found",
        message: "WHIP/WHEP resource does not exist".to_owned(),
    }
}

fn invalid_trickle_fragment() -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_trickle_ice_fragment",
        message: "body is not a supported ICE SDP fragment".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use fluvora_domain::RoomId;
    use fluvora_sdp::SessionDescription;

    use super::{
        ProtocolSession, TrickleFragment, WebRtcHttpProtocol, require_content_type,
        require_current_etag, validate_protocol_direction, validate_trickle_fragment,
    };

    fn directional_offer(direction: &str) -> SessionDescription {
        SessionDescription::parse(&format!(
            "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\na={direction}\r\n"
        ))
        .expect("directional SDP")
    }

    fn protocol_session() -> ProtocolSession {
        ProtocolSession {
            room_id: RoomId(1),
            participant: 2,
            protocol: WebRtcHttpProtocol::Whip,
            local_username_fragment: "local".to_owned(),
            local_password: "local-password-1234567".to_owned(),
            remote_username_fragment: "remote".to_owned(),
            remote_password: "remote-password-123456".to_owned(),
            etag_version: 1,
        }
    }

    #[test]
    fn enforces_whip_and_whep_media_directions() {
        let send = directional_offer("sendonly");
        let receive = directional_offer("recvonly");
        assert!(validate_protocol_direction(&send, WebRtcHttpProtocol::Whip).is_ok());
        assert!(validate_protocol_direction(&receive, WebRtcHttpProtocol::Whep).is_ok());
        assert!(validate_protocol_direction(&send, WebRtcHttpProtocol::Whep).is_err());
        assert!(validate_protocol_direction(&receive, WebRtcHttpProtocol::Whip).is_err());
    }

    #[test]
    fn accepts_current_generation_trickle_and_detects_restart() {
        let session = protocol_session();
        let fragment = "a=ice-ufrag:remote\r\na=ice-pwd:remote-password-123456\r\n\
                        a=mid:0\r\na=candidate:1 1 UDP 1 192.0.2.1 5000 typ host\r\n\
                        a=end-of-candidates\r\n";
        assert_eq!(
            validate_trickle_fragment(fragment, &session).expect("candidates"),
            TrickleFragment::Candidates
        );
        let restart = "a=ice-ufrag:new-generation\r\na=ice-pwd:new-password-123456789\r\n";
        assert!(matches!(
            validate_trickle_fragment(restart, &session),
            Ok(TrickleFragment::IceRestart { .. })
        ));
        assert!(validate_trickle_fragment("a=ice-ufrag:new-generation\r\n", &session).is_err());
        assert!(validate_trickle_fragment("a=sendrecv\r\n", &session).is_err());
    }

    #[test]
    fn rejects_duplicate_or_malformed_trickle_attributes() {
        let session = protocol_session();
        assert!(
            validate_trickle_fragment(
                "a=ice-ufrag:remote\r\na=ice-ufrag:second\r\n\
                 a=ice-pwd:remote-password-123456\r\n",
                &session,
            )
            .is_err()
        );
        assert!(
            validate_trickle_fragment("a=ice-ufrag:x\r\na=ice-pwd:short\r\n", &session).is_err()
        );
        assert!(validate_trickle_fragment("a=candidate:\r\n", &session).is_err());
        assert!(validate_trickle_fragment("a=mid:\r\n", &session).is_err());
    }

    #[test]
    fn validates_protocol_content_types_and_etags() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("Application/SDP; charset=utf-8"),
        );
        headers.insert("if-match", HeaderValue::from_static("\"7\""));
        assert!(require_content_type(&headers, "application/sdp").is_ok());
        assert!(require_current_etag(&headers, 7).is_ok());

        let error = require_current_etag(&headers, 8).expect_err("stale ETag");
        assert_eq!(error.status, StatusCode::PRECONDITION_FAILED);
        headers.remove("if-match");
        let error = require_current_etag(&headers, 7).expect_err("missing ETag");
        assert_eq!(error.status, StatusCode::PRECONDITION_REQUIRED);
    }
}
