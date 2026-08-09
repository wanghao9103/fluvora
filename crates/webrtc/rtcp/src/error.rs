use std::fmt;

/// RTCP parsing and encoding failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcpError {
    /// Fewer than four common-header bytes remain.
    PacketTooShort(usize),
    /// RTCP version was not 2.
    UnsupportedVersion(u8),
    /// The common-header length crossed the datagram boundary.
    InvalidPacketLength {
        /// Declared complete packet bytes.
        declared: usize,
        /// Remaining datagram bytes.
        remaining: usize,
    },
    /// A packet-specific fixed field or report block was truncated.
    TruncatedPacket,
    /// Padding was zero, crossed the packet, or contained non-zero alignment bytes.
    InvalidPadding(usize),
    /// A packet-specific body length did not match its header count or format.
    InvalidBodyLength {
        /// RTCP packet type.
        packet_type: u8,
        /// Body byte length.
        length: usize,
    },
    /// An SR or RR loss count did not fit signed 24-bit representation.
    InvalidCumulativeLoss(i32),
    /// More than 31 report blocks or SDES chunks were supplied.
    CountTooLarge(usize),
    /// An SDES item exceeded its one-byte wire length.
    SdesItemTooLarge(usize),
    /// A transport-wide feedback status symbol was reserved.
    ReservedTwccStatus(u8),
    /// TWCC chunks did not describe exactly the declared packet status count.
    InvalidTwccStatusCount,
    /// TWCC receive delta bytes were truncated or had unexpected trailing data.
    InvalidTwccDeltas,
    /// The TWCC reference time did not fit signed 24-bit representation.
    InvalidReferenceTime(i32),
    /// A packet or compound datagram exceeded RTCP wire limits.
    PacketTooLarge,
}

impl fmt::Display for RtcpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort(length) => {
                write!(formatter, "RTCP packet is shorter than 4 bytes: {length}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported RTCP version {version}")
            }
            Self::InvalidPacketLength {
                declared,
                remaining,
            } => write!(
                formatter,
                "RTCP packet declares {declared} bytes with only {remaining} remaining"
            ),
            Self::TruncatedPacket => formatter.write_str("RTCP packet body is truncated"),
            Self::InvalidPadding(length) => write!(formatter, "invalid RTCP padding {length}"),
            Self::InvalidBodyLength {
                packet_type,
                length,
            } => write!(
                formatter,
                "invalid RTCP type {packet_type} body length {length}"
            ),
            Self::InvalidCumulativeLoss(value) => {
                write!(formatter, "RTCP cumulative loss does not fit i24: {value}")
            }
            Self::CountTooLarge(count) => write!(formatter, "RTCP count exceeds 31: {count}"),
            Self::SdesItemTooLarge(length) => write!(formatter, "SDES item is too large: {length}"),
            Self::ReservedTwccStatus(status) => {
                write!(formatter, "reserved TWCC packet status {status}")
            }
            Self::InvalidTwccStatusCount => {
                formatter.write_str("TWCC chunks do not match packet status count")
            }
            Self::InvalidTwccDeltas => formatter.write_str("TWCC receive deltas are malformed"),
            Self::InvalidReferenceTime(value) => {
                write!(formatter, "TWCC reference time does not fit i24: {value}")
            }
            Self::PacketTooLarge => formatter.write_str("RTCP packet exceeds wire limits"),
        }
    }
}

impl std::error::Error for RtcpError {}
