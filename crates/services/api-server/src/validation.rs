use std::collections::HashSet;

use axum::http::{HeaderMap, StatusCode};
use fluvora_domain::{CommandId, MemberRole, RoomId, RoomMode};
use fluvora_transcode_bridge::{MediaCodec, NetworkQuality};

use crate::error::ApiError;
use crate::models::PublishTrackRequest;

pub(super) fn parse_media_codec(value: &str) -> Result<MediaCodec, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "opus" => Ok(MediaCodec::Opus),
        "vp8" => Ok(MediaCodec::Vp8),
        "vp9" => Ok(MediaCodec::Vp9),
        "h264" => Ok(MediaCodec::H264),
        "av1" => Ok(MediaCodec::Av1),
        _ => Err(ApiError {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "unsupported_codec",
            message: "codec must be opus, vp8, vp9, h264, or av1".to_owned(),
        }),
    }
}

pub(super) const fn media_codec_name(codec: MediaCodec) -> &'static str {
    match codec {
        MediaCodec::Opus => "opus",
        MediaCodec::Aac => "aac",
        MediaCodec::Vp8 => "vp8",
        MediaCodec::Vp9 => "vp9",
        MediaCodec::H264 => "h264",
        MediaCodec::Av1 => "av1",
    }
}

pub(super) fn parse_network_quality(value: Option<&str>) -> Result<NetworkQuality, ApiError> {
    match value.unwrap_or("good") {
        "good" => Ok(NetworkQuality::Good),
        "constrained" => Ok(NetworkQuality::Constrained),
        "critical" => Ok(NetworkQuality::Critical),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_network_quality",
            message: "network_quality must be good, constrained, or critical".to_owned(),
        }),
    }
}

pub(super) fn validate_fallback_url(value: Option<&str>) -> Result<(), ApiError> {
    if value.is_none_or(valid_fallback_url) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_hls_fallback_url",
            message: "HLS fallback URL must be HTTPS (or loopback HTTP) and at most 2048 bytes"
                .to_owned(),
        })
    }
}

fn valid_fallback_url(value: &str) -> bool {
    if value.is_empty() || value.len() > 2_048 || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    if parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    match parsed.scheme() {
        "https" => true,
        "http" => match parsed.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

pub(super) fn validate_publish_track(request: &PublishTrackRequest) -> Result<(), ApiError> {
    let media_kind_valid = matches!(request.kind.as_str(), "audio" | "video");
    let codec_valid = matches!(
        request.codec.to_ascii_lowercase().as_str(),
        "opus" | "vp8" | "vp9" | "h264" | "av1"
    );
    let payload_type_valid =
        request.payload_type <= 127 && !(64..=79).contains(&request.payload_type);
    let encodings_valid = !request.encodings.is_empty()
        && request.encodings.len() <= 8
        && request
            .encodings
            .iter()
            .all(|encoding| encoding.max_bitrate_bps > 0);
    let dimensions_valid = match request.kind.as_str() {
        "audio" => request.width == 0 && request.height == 0 && request.frames_per_second == 0,
        "video" => {
            (request.width == 0 && request.height == 0 && request.frames_per_second == 0)
                || ((16..=7_680).contains(&request.width)
                    && (16..=4_320).contains(&request.height)
                    && request.width.is_multiple_of(2)
                    && request.height.is_multiple_of(2)
                    && (1..=120).contains(&request.frames_per_second))
        }
        _ => false,
    };
    if media_kind_valid
        && codec_valid
        && payload_type_valid
        && encodings_valid
        && dimensions_valid
        && request.clock_rate > 0
    {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_track",
            message: "track codec, dimensions, payload type, clock rate, or encodings are invalid"
                .to_owned(),
        })
    }
}

pub(super) fn invalid_sdp_field(field: &'static str) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "invalid_sdp_offer",
        message: format!("SDP offer has invalid {field}"),
    }
}

pub(super) fn supported_extension_uris() -> HashSet<String> {
    [
        "urn:ietf:params:rtp-hdrext:sdes:mid",
        "urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id",
        "urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id",
        "http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time",
        "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub(super) fn parse_mode(value: &str) -> Result<RoomMode, ApiError> {
    match value {
        "sfu" => Ok(RoomMode::Sfu),
        "p2p" => Ok(RoomMode::P2p),
        "live" => Ok(RoomMode::Live),
        "vod" => Ok(RoomMode::Vod),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_room_mode",
            message: "mode must be sfu, p2p, live, or vod".to_owned(),
        }),
    }
}

pub(super) const fn mode_name(mode: RoomMode) -> &'static str {
    match mode {
        RoomMode::Sfu => "sfu",
        RoomMode::P2p => "p2p",
        RoomMode::Live => "live",
        RoomMode::Vod => "vod",
    }
}

pub(super) fn parse_role(value: &str) -> Result<MemberRole, ApiError> {
    match value {
        "co_host" => Ok(MemberRole::CoHost),
        "publisher" => Ok(MemberRole::Publisher),
        "audience" => Ok(MemberRole::Audience),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_member_role",
            message: "role must be co_host, publisher, or audience".to_owned(),
        }),
    }
}

pub(super) fn idempotency_key(headers: &HeaderMap) -> Result<CommandId, ApiError> {
    let value = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "missing_idempotency_key",
            message: "Idempotency-Key header is required for write operations".to_owned(),
        })?;
    parse_id(value).map(CommandId)
}

pub(super) fn parse_room_id(value: &str) -> Result<RoomId, ApiError> {
    parse_id(value).map(RoomId)
}

pub(super) fn parse_id(value: &str) -> Result<u128, ApiError> {
    u128::from_str_radix(value, 16).map_err(|_| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_id",
        message: "identifier must be hexadecimal".to_owned(),
    })
}

pub(super) fn validate_media_identifier(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_media_identifier",
            message: "media identifier must be 1..=128 safe ASCII characters".to_owned(),
        })
    } else {
        Ok(())
    }
}

pub(super) fn percent_encode_query(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(super) fn validate_signal_kind(kind: &str) -> Result<(), ApiError> {
    if matches!(
        kind,
        "offer" | "answer" | "ice-candidate" | "ice-restart" | "renegotiate" | "bye"
    ) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_signal_kind",
            message: "unsupported P2P signaling message".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use fluvora_transcode_bridge::{MediaCodec, NetworkQuality};

    use super::{
        idempotency_key, parse_media_codec, parse_network_quality, percent_encode_query,
        validate_fallback_url, validate_media_identifier, validate_signal_kind,
    };

    #[test]
    fn validates_media_path_negotiation_inputs() {
        assert_eq!(parse_media_codec("H264").expect("codec"), MediaCodec::H264);
        assert_eq!(
            parse_network_quality(Some("critical")).expect("quality"),
            NetworkQuality::Critical
        );
        assert!(validate_fallback_url(Some("https://cdn.example/live/index.m3u8")).is_ok());
        assert!(validate_fallback_url(Some("http://127.0.0.1/live/index.m3u8")).is_ok());
        assert!(validate_fallback_url(Some("http://[::1]/live/index.m3u8")).is_ok());
        assert!(validate_fallback_url(Some("http://attacker.example/live.m3u8")).is_err());
        assert!(
            validate_fallback_url(Some("http://127.0.0.1.attacker.example/live.m3u8")).is_err()
        );
        assert!(validate_fallback_url(Some("https://token@cdn.example/live.m3u8")).is_err());
        assert!(validate_fallback_url(Some("https://cdn.example/live\nindex.m3u8")).is_err());
    }

    #[test]
    fn validates_identifiers_signals_and_idempotency_keys() {
        assert!(validate_media_identifier("live_2026-08-07").is_ok());
        assert!(validate_media_identifier("../manifest").is_err());
        assert!(validate_media_identifier(&"a".repeat(129)).is_err());
        assert!(validate_signal_kind("ice-restart").is_ok());
        assert!(validate_signal_kind("arbitrary-command").is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static("0000000000000000000000000000002a"),
        );
        assert_eq!(idempotency_key(&headers).expect("key").0, 42);
    }

    #[test]
    fn percent_encodes_query_values_as_utf8_bytes() {
        assert_eq!(percent_encode_query("safe-._~"), "safe-._~");
        assert_eq!(
            percent_encode_query("2026-08-07 19:00+08:00"),
            "2026-08-07%2019%3A00%2B08%3A00"
        );
        assert_eq!(percent_encode_query("直播"), "%E7%9B%B4%E6%92%AD");
    }
}
