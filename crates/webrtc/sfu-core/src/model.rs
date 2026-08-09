use std::time::Duration;

use fluvora_media_codec::Codec;
use fluvora_rtcp::TransportWideFeedback;

/// Stable room-local participant identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(pub u128);

/// Stable room-local published track identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u64);

/// Stable room-local subscription identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(pub u64);

/// Media behavior relevant to switching and timestamp cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Audio track.
    Audio,
    /// Video track.
    Video,
}

/// One simulcast or spatial encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoding {
    /// Incoming RTP SSRC.
    pub ssrc: u32,
    /// Negotiated RID, when present.
    pub rid: Option<String>,
    /// Zero-based spatial quality layer.
    pub spatial_layer: u8,
    /// Publisher-declared ceiling used by adaptive selection.
    pub max_bitrate_bps: u64,
}

/// One publisher track known to the SFU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedTrack {
    /// Track identifier.
    pub id: TrackId,
    /// Participant allowed to send its SSRCs.
    pub owner: ParticipantId,
    /// Audio or video.
    pub kind: MediaKind,
    /// Encoded payload family.
    pub codec: Codec,
    /// RTP clock frequency.
    pub clock_rate: u32,
    /// Incoming payload type.
    pub payload_type: u8,
    /// Available encodings.
    pub encodings: Vec<Encoding>,
}

impl PublishedTrack {
    pub(crate) fn encoding(&self, spatial_layer: u8) -> Option<&Encoding> {
        self.encodings
            .iter()
            .find(|encoding| encoding.spatial_layer == spatial_layer)
    }
}

/// Spatial and temporal forwarding target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    /// Simulcast or scalable-codec spatial layer.
    pub spatial: u8,
    /// Maximum temporal layer to forward.
    pub temporal: u8,
}

/// Parameters for one subscriber down-track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionConfig {
    /// Subscription identifier.
    pub id: SubscriptionId,
    /// Receiving participant.
    pub subscriber: ParticipantId,
    /// Published source track.
    pub track_id: TrackId,
    /// Subscriber-visible SSRC.
    pub output_ssrc: u32,
    /// Subscriber-negotiated payload type.
    pub output_payload_type: u8,
    /// Initial adaptive target.
    pub initial_layer: Layer,
    /// First subscriber-visible sequence number.
    pub initial_sequence_number: u16,
    /// First subscriber-visible timestamp.
    pub initial_timestamp: u32,
    /// Per-subscriber MID/RID/TWCC extension transformations.
    pub extension_rewrites: Vec<fluvora_rtp::ExtensionRewrite>,
}

/// Bounded room resource configuration.
#[derive(Debug, Clone)]
pub struct RoomConfig {
    /// Maximum simultaneously published tracks.
    pub max_tracks: usize,
    /// Maximum active down-track subscriptions.
    pub max_subscriptions: usize,
    /// Maximum retransmission-cache packets per subscription.
    pub retransmission_cache_packets: usize,
    /// Maximum retransmission-cache age.
    pub retransmission_cache_age: Duration,
    /// Minimum interval between upstream PLI events for one source SSRC.
    pub pli_throttle: Duration,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            max_tracks: 1_024,
            max_subscriptions: 16_384,
            retransmission_cache_packets: 2_048,
            retransmission_cache_age: Duration::from_secs(2),
            pli_throttle: Duration::from_millis(500),
        }
    }
}

/// One clear RTP packet ready for per-subscriber SRTP protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedPacket {
    /// Receiving participant.
    pub subscriber: ParticipantId,
    /// Down-track identifier.
    pub subscription_id: SubscriptionId,
    /// Rewritten clear RTP packet.
    pub packet: Vec<u8>,
    /// Source layer used by this packet.
    pub layer: Layer,
    /// Whether this packet carries a random-access frame.
    pub keyframe: bool,
    /// Whether this is a cache retransmission.
    pub retransmission: bool,
}

/// Event consumed by RTCP, congestion-control, and monitoring integrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SfuEvent {
    /// A down-track committed a keyframe-safe spatial switch.
    LayerSwitched {
        /// Subscription that switched.
        subscription_id: SubscriptionId,
        /// Previous selected spatial layer.
        from: Option<u8>,
        /// Newly selected spatial layer.
        to: u8,
    },
    /// The upstream RTCP sender should request a keyframe.
    PictureLossIndication {
        /// Published track needing a keyframe.
        track_id: TrackId,
        /// Target encoding SSRC.
        media_ssrc: u32,
    },
    /// Authenticated subscriber TWCC feedback for bandwidth estimation.
    TransportFeedback {
        /// Feedback source.
        subscriber: ParticipantId,
        /// Decoded feedback report.
        feedback: TransportWideFeedback,
    },
}

/// Result of routing one publisher RTP packet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForwardOutcome {
    /// Per-subscriber RTP outputs.
    pub packets: Vec<ForwardedPacket>,
    /// Layer-switch and upstream feedback events.
    pub events: Vec<SfuEvent>,
}

/// Result of processing one authenticated subscriber RTCP compound packet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControlOutput {
    /// Clear cached packets that should be re-protected and retransmitted.
    pub retransmissions: Vec<ForwardedPacket>,
    /// Upstream PLI and transport-feedback events.
    pub events: Vec<SfuEvent>,
}
