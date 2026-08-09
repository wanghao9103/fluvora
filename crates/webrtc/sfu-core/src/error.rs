use std::fmt;

use fluvora_media_codec::PayloadError;
use fluvora_rtcp::RtcpError;
use fluvora_rtp::RtpError;

use crate::{ParticipantId, SubscriptionId, TrackId};

/// SFU validation, authorization, and packet-processing failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfuError {
    /// Track identifier is already published.
    DuplicateTrack(TrackId),
    /// Subscription identifier is already in use.
    DuplicateSubscription(SubscriptionId),
    /// Track does not exist.
    UnknownTrack(TrackId),
    /// Subscription does not exist.
    UnknownSubscription(SubscriptionId),
    /// Packet SSRC does not identify a published encoding.
    UnknownSsrc(u32),
    /// Participant does not own the identified publisher or subscriber resource.
    UnauthorizedParticipant(ParticipantId),
    /// Track has no encoding or duplicate SSRC/spatial-layer declarations.
    InvalidEncodings,
    /// Payload type is not valid with RTP/RTCP multiplexing.
    InvalidPayloadType(u8),
    /// Requested spatial layer is not published.
    UnknownSpatialLayer(u8),
    /// A configured bounded room resource limit was reached.
    ResourceLimit(&'static str),
    /// RTP parsing or rewriting failed.
    Rtp(RtpError),
    /// RTCP parsing failed.
    Rtcp(RtcpError),
    /// Codec payload inspection failed.
    Payload(PayloadError),
}

impl fmt::Display for SfuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTrack(id) => write!(formatter, "duplicate track {id:?}"),
            Self::DuplicateSubscription(id) => write!(formatter, "duplicate subscription {id:?}"),
            Self::UnknownTrack(id) => write!(formatter, "unknown track {id:?}"),
            Self::UnknownSubscription(id) => write!(formatter, "unknown subscription {id:?}"),
            Self::UnknownSsrc(ssrc) => write!(formatter, "unknown RTP SSRC {ssrc}"),
            Self::UnauthorizedParticipant(id) => {
                write!(formatter, "participant {id:?} is not authorized for media")
            }
            Self::InvalidEncodings => formatter.write_str("invalid published encodings"),
            Self::InvalidPayloadType(payload_type) => {
                write!(
                    formatter,
                    "invalid RTP/RTCP-mux payload type {payload_type}"
                )
            }
            Self::UnknownSpatialLayer(layer) => write!(formatter, "unknown spatial layer {layer}"),
            Self::ResourceLimit(resource) => write!(formatter, "{resource} resource limit reached"),
            Self::Rtp(error) => error.fmt(formatter),
            Self::Rtcp(error) => error.fmt(formatter),
            Self::Payload(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SfuError {}

impl From<RtpError> for SfuError {
    fn from(value: RtpError) -> Self {
        Self::Rtp(value)
    }
}

impl From<RtcpError> for SfuError {
    fn from(value: RtcpError) -> Self {
        Self::Rtcp(value)
    }
}

impl From<PayloadError> for SfuError {
    fn from(value: PayloadError) -> Self {
        Self::Payload(value)
    }
}
