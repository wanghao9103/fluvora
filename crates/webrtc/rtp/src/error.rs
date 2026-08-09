use std::fmt;

/// RTP parsing and encoding failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtpError {
    /// The datagram cannot contain the fixed RTP header.
    PacketTooShort(usize),
    /// RTP version was not 2.
    UnsupportedVersion(u8),
    /// A declared CSRC or extension crossed the datagram boundary.
    TruncatedHeader,
    /// Padding was enabled but absent, zero-length, or larger than the payload area.
    InvalidPadding(usize),
    /// More than 15 CSRC identifiers were supplied.
    TooManyCsrcs(usize),
    /// RTP payload type exceeded the seven-bit field.
    InvalidPayloadType(u8),
    /// A header-extension element was truncated.
    TruncatedExtension,
    /// An extension identifier is invalid for the selected format.
    InvalidExtensionId {
        /// Requested format.
        format: super::ExtensionFormat,
        /// Supplied identifier.
        id: u8,
    },
    /// An extension value is invalid for the selected format.
    InvalidExtensionLength {
        /// Requested format.
        format: super::ExtensionFormat,
        /// Supplied byte length.
        length: usize,
    },
    /// The extension block exceeded the 16-bit word count.
    ExtensionBlockTooLarge,
    /// RTP padding cannot exceed 255 bytes.
    PaddingTooLarge(usize),
    /// A complete RTP packet exceeded the UDP payload limit.
    PacketTooLarge,
    /// Header extensions cannot be rewritten because the packet uses no known RFC 8285 profile.
    UnsupportedExtensionRewrite,
}

impl fmt::Display for RtpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort(length) => {
                write!(formatter, "RTP packet is shorter than 12 bytes: {length}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported RTP version {version}")
            }
            Self::TruncatedHeader => formatter.write_str("RTP header is truncated"),
            Self::InvalidPadding(length) => {
                write!(formatter, "invalid RTP padding length {length}")
            }
            Self::TooManyCsrcs(count) => write!(formatter, "too many RTP CSRCs: {count}"),
            Self::InvalidPayloadType(payload_type) => {
                write!(formatter, "invalid RTP payload type {payload_type}")
            }
            Self::TruncatedExtension => formatter.write_str("RTP header extension is truncated"),
            Self::InvalidExtensionId { format, id } => {
                write!(formatter, "invalid {format:?} RTP extension id {id}")
            }
            Self::InvalidExtensionLength { format, length } => {
                write!(
                    formatter,
                    "invalid {format:?} RTP extension length {length}"
                )
            }
            Self::ExtensionBlockTooLarge => {
                formatter.write_str("RTP extension block exceeds 65535 words")
            }
            Self::PaddingTooLarge(length) => {
                write!(formatter, "RTP padding exceeds 255 bytes: {length}")
            }
            Self::PacketTooLarge => formatter.write_str("RTP packet exceeds UDP payload limit"),
            Self::UnsupportedExtensionRewrite => {
                formatter.write_str("RTP header extensions cannot be rewritten")
            }
        }
    }
}

impl std::error::Error for RtpError {}
