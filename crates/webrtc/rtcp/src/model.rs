/// One reception report block shared by sender and receiver reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportBlock {
    /// Source being reported.
    pub ssrc: u32,
    /// Fraction lost since the previous report, in 1/256 units.
    pub fraction_lost: u8,
    /// Signed cumulative packets lost.
    pub cumulative_lost: i32,
    /// Extended highest received sequence number.
    pub extended_highest_sequence: u32,
    /// Interarrival jitter.
    pub jitter: u32,
    /// Middle 32 bits of the last sender-report NTP timestamp.
    pub last_sender_report: u32,
    /// Delay since the last sender report in 1/65536 seconds.
    pub delay_since_last_sender_report: u32,
}

/// RTCP sender report (PT 200).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderReport {
    /// Sender SSRC.
    pub sender_ssrc: u32,
    /// 64-bit NTP timestamp.
    pub ntp_timestamp: u64,
    /// Corresponding RTP timestamp.
    pub rtp_timestamp: u32,
    /// Total RTP packets sent.
    pub sender_packet_count: u32,
    /// Total RTP payload octets sent.
    pub sender_octet_count: u32,
    /// Reception report blocks.
    pub reports: Vec<ReportBlock>,
}

/// RTCP receiver report (PT 201).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverReport {
    /// Reporter SSRC.
    pub sender_ssrc: u32,
    /// Reception report blocks.
    pub reports: Vec<ReportBlock>,
}

/// One SDES item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdesItem {
    /// SDES item type, such as 1 for CNAME.
    pub item_type: u8,
    /// Opaque item value.
    pub value: Vec<u8>,
}

/// One SSRC chunk in a source-description packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdesChunk {
    /// Described SSRC.
    pub ssrc: u32,
    /// Items before the mandatory END marker.
    pub items: Vec<SdesItem>,
}

/// RTCP source description (PT 202).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescription {
    /// Source chunks.
    pub chunks: Vec<SdesChunk>,
}

/// One PID/BLP pair in generic negative acknowledgement feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NackEntry {
    /// First missing RTP sequence number.
    pub packet_id: u16,
    /// Bitmask for the next 16 sequence numbers.
    pub lost_packet_bitmask: u16,
}

/// Generic NACK RTP feedback (PT 205, FMT 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericNack {
    /// Feedback sender SSRC.
    pub sender_ssrc: u32,
    /// Media source SSRC.
    pub media_ssrc: u32,
    /// Missing packet groups.
    pub entries: Vec<NackEntry>,
}

/// Picture-loss indication payload feedback (PT 206, FMT 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureLossIndication {
    /// Feedback sender SSRC.
    pub sender_ssrc: u32,
    /// Media source SSRC.
    pub media_ssrc: u32,
}

/// One packet symbol and optional delta in a transport-wide feedback report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwccStatus {
    /// Packet was not received.
    NotReceived,
    /// Packet arrived with an unsigned 250-microsecond delta.
    ReceivedSmallDelta(u8),
    /// Packet arrived with a signed 250-microsecond delta.
    ReceivedLargeDelta(i16),
}

/// Transport-wide congestion-control feedback (PT 205, FMT 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportWideFeedback {
    /// Feedback sender SSRC.
    pub sender_ssrc: u32,
    /// Media source SSRC, conventionally zero for transport feedback.
    pub media_ssrc: u32,
    /// Sequence number represented by the first status.
    pub base_sequence_number: u16,
    /// Signed 24-bit reference time in 64-millisecond units.
    pub reference_time: i32,
    /// Feedback packet sequence counter.
    pub feedback_packet_count: u8,
    /// Consecutive statuses beginning at `base_sequence_number`.
    pub statuses: Vec<TwccStatus>,
}

/// An unrecognized RTCP packet retained for observability and forwarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPacket {
    /// Five-bit common-header count or feedback format.
    pub count: u8,
    /// RTCP packet type.
    pub packet_type: u8,
    /// Body without common-header padding.
    pub body: Vec<u8>,
}

/// One decoded or encodable RTCP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Sender report.
    SenderReport(SenderReport),
    /// Receiver report.
    ReceiverReport(ReceiverReport),
    /// Source description.
    SourceDescription(SourceDescription),
    /// Generic NACK.
    GenericNack(GenericNack),
    /// Picture loss indication.
    PictureLossIndication(PictureLossIndication),
    /// Transport-wide congestion-control feedback.
    TransportWideFeedback(TransportWideFeedback),
    /// Unknown packet type or feedback format.
    Raw(RawPacket),
}
