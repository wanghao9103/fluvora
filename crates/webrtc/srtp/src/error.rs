use std::fmt;

use fluvora_rtcp::RtcpError;
use fluvora_rtp::RtpError;

/// SRTP key, authentication, replay, and packet failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrtpError {
    /// Master key was not 128 bits.
    InvalidMasterKeyLength(usize),
    /// Master salt was not 112 bits.
    InvalidMasterSaltLength(usize),
    /// The authentication tag did not verify.
    AuthenticationFailed,
    /// The packet index is already present in the replay window.
    ReplayDetected,
    /// The packet index predates the replay window.
    PacketTooOld,
    /// Outbound RTP sequence numbers must advance to prevent keystream reuse.
    NonMonotonicSequence,
    /// The 48-bit SRTP or 31-bit SRTCP packet index was exhausted.
    PacketIndexExhausted,
    /// An SRTCP packet was shorter than its clear header, index, and tag.
    SrtcpPacketTooShort(usize),
    /// RTP parsing failed.
    Rtp(RtpError),
    /// RTCP parsing failed.
    Rtcp(RtcpError),
}

impl fmt::Display for SrtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMasterKeyLength(length) => {
                write!(
                    formatter,
                    "SRTP master key must be 16 bytes, received {length}"
                )
            }
            Self::InvalidMasterSaltLength(length) => {
                write!(
                    formatter,
                    "SRTP master salt must be 14 bytes, received {length}"
                )
            }
            Self::AuthenticationFailed => formatter.write_str("SRTP authentication failed"),
            Self::ReplayDetected => formatter.write_str("SRTP replay detected"),
            Self::PacketTooOld => formatter.write_str("SRTP packet predates replay window"),
            Self::NonMonotonicSequence => {
                formatter.write_str("outbound SRTP sequence number did not advance")
            }
            Self::PacketIndexExhausted => formatter.write_str("SRTP packet index exhausted"),
            Self::SrtcpPacketTooShort(length) => {
                write!(formatter, "SRTCP packet is too short: {length}")
            }
            Self::Rtp(error) => error.fmt(formatter),
            Self::Rtcp(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SrtpError {}

impl From<RtpError> for SrtpError {
    fn from(value: RtpError) -> Self {
        Self::Rtp(value)
    }
}

impl From<RtcpError> for SrtpError {
    fn from(value: RtcpError) -> Self {
        Self::Rtcp(value)
    }
}
