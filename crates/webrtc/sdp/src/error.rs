use core::fmt;

/// A structured SDP parsing or validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdpError {
    line: Option<usize>,
    kind: SdpErrorKind,
}

impl SdpError {
    /// Creates an error not associated with a source line.
    #[must_use]
    pub const fn new(kind: SdpErrorKind) -> Self {
        Self { line: None, kind }
    }

    /// Creates an error associated with a one-based source line.
    #[must_use]
    pub const fn at_line(line: usize, kind: SdpErrorKind) -> Self {
        Self {
            line: Some(line),
            kind,
        }
    }

    /// Returns the one-based line number, when available.
    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.line
    }

    /// Returns the stable error kind.
    #[must_use]
    pub const fn kind(&self) -> &SdpErrorKind {
        &self.kind
    }
}

impl fmt::Display for SdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(formatter, "SDP line {line}: ")?;
        }
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for SdpError {}

/// Stable categories for SDP failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdpErrorKind {
    /// The complete SDP exceeded the configured hard limit.
    DocumentTooLarge(usize),
    /// One line exceeded the configured hard limit.
    LineTooLong(usize),
    /// A line did not have the `x=value` shape.
    InvalidLine,
    /// A mandatory session line was absent.
    MissingSessionField(&'static str),
    /// The SDP version was not zero.
    UnsupportedVersion(String),
    /// A media line was malformed.
    InvalidMediaLine,
    /// A numeric token was invalid.
    InvalidNumber(String),
    /// An attribute value was malformed.
    InvalidAttribute {
        /// Attribute name.
        name: String,
        /// Human-readable reason.
        reason: &'static str,
    },
    /// A media-level line occurred before the first `m=` line.
    MediaLineWithoutMedia(char),
    /// A MID occurred more than once.
    DuplicateMid(String),
    /// A bundled media section had no MID.
    MissingMid,
    /// BUNDLE referenced a MID that does not exist.
    UnknownBundleMid(String),
    /// A media section was not included in the offered BUNDLE group.
    MediaNotBundled(String),
    /// An RTP media section omitted rtcp-mux.
    MissingRtcpMux(String),
    /// ICE credentials were absent.
    MissingIceCredentials(String),
    /// A DTLS fingerprint was absent.
    MissingFingerprint(String),
    /// The DTLS setup role was absent or invalid for an offer.
    InvalidSetupRole(String),
    /// No acceptable codec remained for a non-rejected media section.
    NoCompatibleCodec(String),
    /// A payload type was out of range or not numeric.
    InvalidPayloadType(String),
    /// An RTPMAP value was invalid.
    InvalidRtpMap(String),
    /// The generated answer exceeded a wire limit.
    AnswerTooLarge,
}

impl fmt::Display for SdpErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DocumentTooLarge(length) => {
                write!(formatter, "document too large: {length} bytes")
            }
            Self::LineTooLong(length) => write!(formatter, "line too long: {length} bytes"),
            Self::InvalidLine => formatter.write_str("invalid line shape"),
            Self::MissingSessionField(field) => {
                write!(formatter, "missing mandatory {field}= line")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported SDP version {version}")
            }
            Self::InvalidMediaLine => formatter.write_str("invalid media line"),
            Self::InvalidNumber(value) => write!(formatter, "invalid number {value}"),
            Self::InvalidAttribute { name, reason } => {
                write!(formatter, "invalid {name} attribute: {reason}")
            }
            Self::MediaLineWithoutMedia(prefix) => {
                write!(formatter, "{prefix}= is only valid after a media line")
            }
            Self::DuplicateMid(mid) => write!(formatter, "duplicate MID {mid}"),
            Self::MissingMid => formatter.write_str("bundled media section is missing MID"),
            Self::UnknownBundleMid(mid) => write!(formatter, "BUNDLE references unknown MID {mid}"),
            Self::MediaNotBundled(mid) => write!(formatter, "media MID {mid} is not bundled"),
            Self::MissingRtcpMux(mid) => {
                write!(formatter, "RTP media MID {mid} is missing rtcp-mux")
            }
            Self::MissingIceCredentials(mid) => {
                write!(formatter, "media MID {mid} is missing ICE credentials")
            }
            Self::MissingFingerprint(mid) => {
                write!(formatter, "media MID {mid} is missing a DTLS fingerprint")
            }
            Self::InvalidSetupRole(mid) => {
                write!(formatter, "media MID {mid} has an invalid DTLS setup role")
            }
            Self::NoCompatibleCodec(mid) => {
                write!(formatter, "media MID {mid} has no compatible codec")
            }
            Self::InvalidPayloadType(value) => {
                write!(formatter, "invalid RTP payload type {value}")
            }
            Self::InvalidRtpMap(value) => write!(formatter, "invalid rtpmap {value}"),
            Self::AnswerTooLarge => formatter.write_str("generated SDP answer is too large"),
        }
    }
}
