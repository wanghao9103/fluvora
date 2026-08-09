//! Constant-time-prefix classification for multiplexed WebRTC UDP sockets.

/// Protocol family selected from a shared UDP datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatagramKind {
    /// RFC 8489 STUN message.
    Stun,
    /// TURN channel data with the `01` prefix.
    TurnChannelData,
    /// DTLS record.
    Dtls,
    /// RTP media packet.
    Rtp,
    /// RTCP control packet multiplexed with RTP.
    Rtcp,
    /// Truncated or unrecognized input.
    Unknown,
}

/// Classifies a datagram using RFC 7983 and the stricter STUN magic-cookie check.
///
/// The function does not claim packet validity. The selected protocol codec must still perform
/// complete length and semantic validation.
#[must_use]
pub fn classify(input: &[u8]) -> DatagramKind {
    let Some(&first) = input.first() else {
        return DatagramKind::Unknown;
    };
    match first {
        0..=3 if is_stun(input) => DatagramKind::Stun,
        64..=79 if is_turn_channel_data(input) => DatagramKind::TurnChannelData,
        20..=63 => DatagramKind::Dtls,
        128..=191 if input.len() >= 2 => {
            if (192..=223).contains(&input[1]) {
                DatagramKind::Rtcp
            } else {
                DatagramKind::Rtp
            }
        }
        _ => DatagramKind::Unknown,
    }
}

/// Returns whether an RTP payload type is safe when RTP and RTCP share a port.
///
/// Payload types 64 through 95 can make the second RTP byte collide with RTCP packet types when
/// the RTP marker is set.
#[must_use]
pub const fn is_rtcp_mux_safe_payload_type(payload_type: u8) -> bool {
    payload_type <= 127 && !(payload_type >= 64 && payload_type <= 95)
}

fn is_stun(input: &[u8]) -> bool {
    input.len() >= 20 && input.get(4..8) == Some([0x21, 0x12, 0xa4, 0x42].as_slice())
}

fn is_turn_channel_data(input: &[u8]) -> bool {
    let Some(channel_bytes) = input.get(..2) else {
        return false;
    };
    let channel = u16::from_be_bytes([channel_bytes[0], channel_bytes[1]]);
    (0x4000..=0x7fff).contains(&channel)
}

#[cfg(test)]
mod tests {
    use super::{DatagramKind, classify, is_rtcp_mux_safe_payload_type};

    #[test]
    fn classifies_supported_protocol_prefixes() {
        let mut stun = [0_u8; 20];
        stun[4..8].copy_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
        assert_eq!(classify(&stun), DatagramKind::Stun);
        assert_eq!(
            classify(&[0x40, 0x01, 0, 3, 1, 2, 3]),
            DatagramKind::TurnChannelData
        );
        assert_eq!(classify(&[22, 0xfe, 0xfd]), DatagramKind::Dtls);
        assert_eq!(classify(&[0x80, 111]), DatagramKind::Rtp);
        assert_eq!(classify(&[0x80, 200]), DatagramKind::Rtcp);
    }

    #[test]
    fn does_not_misclassify_truncated_or_cookie_less_input() {
        assert_eq!(classify(&[]), DatagramKind::Unknown);
        assert_eq!(classify(&[0, 1, 2]), DatagramKind::Unknown);
        assert_eq!(classify(&[0x80]), DatagramKind::Unknown);
        assert_eq!(classify(&[4; 20]), DatagramKind::Unknown);
    }

    #[test]
    fn enforces_rtcp_mux_payload_type_exclusions() {
        assert!(is_rtcp_mux_safe_payload_type(63));
        assert!(!is_rtcp_mux_safe_payload_type(64));
        assert!(!is_rtcp_mux_safe_payload_type(95));
        assert!(is_rtcp_mux_safe_payload_type(96));
        assert!(!is_rtcp_mux_safe_payload_type(128));
    }
}
