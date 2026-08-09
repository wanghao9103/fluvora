use std::collections::{HashMap, HashSet};

use crate::{SdpError, SdpErrorKind};

/// An SDP attribute split into name and optional value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    name: String,
    value: Option<String>,
}

impl Attribute {
    /// Creates an attribute.
    #[must_use]
    pub fn new(name: impl Into<String>, value: Option<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            value: value.map(Into::into),
        }
    }

    /// Returns the attribute name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional attribute value.
    #[must_use]
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

/// Media kind from an `m=` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaKind {
    /// Audio RTP.
    Audio,
    /// Video RTP.
    Video,
    /// WebRTC data transport.
    Application,
    /// A media kind not understood by Fluvora.
    Other(String),
}

impl MediaKind {
    /// Parses an SDP media token.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "audio" => Self::Audio,
            "video" => Self::Video,
            "application" => Self::Application,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Returns the SDP media token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Application => "application",
            Self::Other(value) => value,
        }
    }

    /// Returns whether this media section carries RTP.
    #[must_use]
    pub const fn is_rtp(&self) -> bool {
        matches!(self, Self::Audio | Self::Video)
    }
}

/// RTP transceiver direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Send and receive.
    SendRecv,
    /// Send only.
    SendOnly,
    /// Receive only.
    RecvOnly,
    /// Neither send nor receive.
    Inactive,
}

impl Direction {
    /// Returns the answer-side reciprocal direction.
    #[must_use]
    pub const fn reciprocal(self) -> Self {
        match self {
            Self::SendRecv => Self::SendRecv,
            Self::SendOnly => Self::RecvOnly,
            Self::RecvOnly => Self::SendOnly,
            Self::Inactive => Self::Inactive,
        }
    }

    /// Returns the SDP attribute name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendRecv => "sendrecv",
            Self::SendOnly => "sendonly",
            Self::RecvOnly => "recvonly",
            Self::Inactive => "inactive",
        }
    }
}

/// DTLS setup role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRole {
    /// Either active or passive, valid in an offer.
    ActPass,
    /// DTLS client.
    Active,
    /// DTLS server.
    Passive,
    /// Hold connection.
    HoldConn,
}

impl SetupRole {
    /// Parses an SDP setup token.
    #[must_use]
    pub const fn parse(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"actpass" => Some(Self::ActPass),
            b"active" => Some(Self::Active),
            b"passive" => Some(Self::Passive),
            b"holdconn" => Some(Self::HoldConn),
            _ => None,
        }
    }
}

/// A DTLS certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Lowercase algorithm name, normally `sha-256`.
    pub algorithm: String,
    /// Colon-separated fingerprint value.
    pub value: String,
}

/// An RTP codec advertised by a media section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpCodec {
    /// Payload type from the media line.
    pub payload_type: u8,
    /// Codec encoding name.
    pub name: String,
    /// RTP clock rate.
    pub clock_rate: u32,
    /// Audio channel count. Video normally uses one.
    pub channels: u16,
    /// Optional fmtp parameters without the payload type.
    pub fmtp: Option<String>,
    /// RTCP feedback tokens without the payload type.
    pub rtcp_feedback: Vec<String>,
}

impl RtpCodec {
    /// Returns the RTX `apt` payload type when this is an RTX codec.
    #[must_use]
    pub fn associated_payload_type(&self) -> Option<u8> {
        if !self.name.eq_ignore_ascii_case("rtx") {
            return None;
        }
        self.fmtp.as_deref().and_then(|fmtp| {
            fmtp.split(';').find_map(|item| {
                let (name, value) = item.trim().split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("apt")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
        })
    }
}

/// An RTP header extension mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtMap {
    /// Offered extension ID.
    pub id: u8,
    /// Optional direction qualifier.
    pub direction: Option<Direction>,
    /// Extension URI.
    pub uri: String,
    /// Optional extension attributes.
    pub attributes: Option<String>,
}

/// A RID restriction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rid {
    /// RID identifier.
    pub id: String,
    /// RID direction.
    pub direction: Direction,
    /// Raw restrictions.
    pub restrictions: Option<String>,
}

/// One media section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// Media kind.
    pub kind: MediaKind,
    /// Offered port.
    pub port: u16,
    /// Transport profile.
    pub protocol: String,
    /// Media formats in offer order.
    pub formats: Vec<String>,
    /// Optional connection line.
    pub connection: Option<String>,
    /// Media-level attributes.
    pub attributes: Vec<Attribute>,
}

impl MediaDescription {
    /// Returns the first attribute value.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .and_then(Attribute::value)
    }

    /// Returns whether an attribute is present.
    #[must_use]
    pub fn has_attribute(&self, name: &str) -> bool {
        self.attributes
            .iter()
            .any(|attribute| attribute.name == name)
    }

    /// Returns MID.
    #[must_use]
    pub fn mid(&self) -> Option<&str> {
        self.attribute("mid")
    }

    /// Returns the offered direction, defaulting to sendrecv.
    #[must_use]
    pub fn direction(&self) -> Direction {
        for attribute in &self.attributes {
            match attribute.name.as_str() {
                "sendrecv" => return Direction::SendRecv,
                "sendonly" => return Direction::SendOnly,
                "recvonly" => return Direction::RecvOnly,
                "inactive" => return Direction::Inactive,
                _ => {}
            }
        }
        Direction::SendRecv
    }

    /// Parses all offered RTP codecs.
    ///
    /// # Errors
    ///
    /// Returns [`SdpError`] for an invalid payload type or RTPMAP.
    pub fn codecs(&self) -> Result<Vec<RtpCodec>, SdpError> {
        let mut codecs = Vec::new();
        let fmtp = self.attribute_map("fmtp");
        let feedback = self.attribute_multimap("rtcp-fb");

        for format in &self.formats {
            let payload_type = parse_payload_type(format)?;
            let Some(rtpmap) = find_prefixed_attribute(&self.attributes, "rtpmap", format) else {
                if payload_type < 96 {
                    continue;
                }
                return Err(SdpError::new(SdpErrorKind::InvalidRtpMap(format.clone())));
            };
            let (name, clock_rate, channels) = parse_rtpmap(rtpmap)?;
            let mut rtcp_feedback = feedback.get(format).cloned().unwrap_or_default();
            rtcp_feedback.extend(feedback.get("*").cloned().unwrap_or_default());
            codecs.push(RtpCodec {
                payload_type,
                name,
                clock_rate,
                channels,
                fmtp: fmtp.get(format).cloned(),
                rtcp_feedback,
            });
        }
        Ok(codecs)
    }

    /// Parses all extmap attributes.
    ///
    /// # Errors
    ///
    /// Returns [`SdpError`] for a malformed ID, direction, or URI.
    pub fn extmaps(&self) -> Result<Vec<ExtMap>, SdpError> {
        self.attributes
            .iter()
            .filter(|attribute| attribute.name == "extmap")
            .map(parse_extmap)
            .collect()
    }

    /// Parses all RID attributes.
    ///
    /// # Errors
    ///
    /// Returns [`SdpError`] for malformed RID attributes.
    pub fn rids(&self) -> Result<Vec<Rid>, SdpError> {
        self.attributes
            .iter()
            .filter(|attribute| attribute.name == "rid")
            .map(parse_rid)
            .collect()
    }

    pub(crate) fn attribute_values(&self, name: &str) -> impl Iterator<Item = &str> {
        self.attributes
            .iter()
            .filter(move |attribute| attribute.name == name)
            .filter_map(Attribute::value)
    }

    fn attribute_map(&self, name: &str) -> HashMap<String, String> {
        self.attribute_values(name)
            .filter_map(split_attribute_target)
            .map(|(target, value)| (target.to_owned(), value.to_owned()))
            .collect()
    }

    fn attribute_multimap(&self, name: &str) -> HashMap<String, Vec<String>> {
        let mut result: HashMap<String, Vec<String>> = HashMap::new();
        for (target, value) in self
            .attribute_values(name)
            .filter_map(split_attribute_target)
        {
            result
                .entry(target.to_owned())
                .or_default()
                .push(value.to_owned());
        }
        result
    }
}

/// A parsed SDP session description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    /// Version value.
    pub version: String,
    /// Origin value.
    pub origin: String,
    /// Session name.
    pub session_name: String,
    /// Timing value.
    pub timing: String,
    /// Optional session connection line.
    pub connection: Option<String>,
    /// Session-level attributes.
    pub attributes: Vec<Attribute>,
    /// Media sections in offer order.
    pub media: Vec<MediaDescription>,
}

impl SessionDescription {
    /// Parses a complete SDP document.
    ///
    /// # Errors
    ///
    /// Returns [`SdpError`] for malformed or missing mandatory lines.
    pub fn parse(input: &str) -> Result<Self, SdpError> {
        crate::parser::parse(input)
    }

    /// Returns the first session-level attribute value.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
            .and_then(Attribute::value)
    }

    /// Returns all BUNDLE mids from the first BUNDLE group.
    #[must_use]
    pub fn bundle_mids(&self) -> Vec<&str> {
        self.attributes
            .iter()
            .filter(|attribute| attribute.name == "group")
            .filter_map(Attribute::value)
            .find_map(|value| {
                let mut tokens = value.split_whitespace();
                tokens
                    .next()
                    .is_some_and(|semantics| semantics.eq_ignore_ascii_case("BUNDLE"))
                    .then(|| tokens.collect())
            })
            .unwrap_or_default()
    }

    /// Validates constraints required by a bundled browser WebRTC offer.
    ///
    /// # Errors
    ///
    /// Returns [`SdpError`] when MID/BUNDLE, ICE, DTLS, RTP muxing, or codec syntax is invalid.
    pub fn validate_webrtc_offer(&self) -> Result<(), SdpError> {
        let bundle_mids = self.bundle_mids();
        let bundle_set: HashSet<&str> = bundle_mids.iter().copied().collect();
        let mut seen_mids = HashSet::new();

        for media in &self.media {
            let mid = media
                .mid()
                .ok_or_else(|| SdpError::new(SdpErrorKind::MissingMid))?;
            if !seen_mids.insert(mid) {
                return Err(SdpError::new(SdpErrorKind::DuplicateMid(mid.to_owned())));
            }
            if !bundle_set.contains(mid) {
                return Err(SdpError::new(SdpErrorKind::MediaNotBundled(mid.to_owned())));
            }
            self.validate_media(media, mid)?;
        }

        for mid in bundle_mids {
            if !seen_mids.contains(mid) {
                return Err(SdpError::new(SdpErrorKind::UnknownBundleMid(
                    mid.to_owned(),
                )));
            }
        }
        Ok(())
    }

    /// Returns a media-level attribute or its session-level inherited value.
    #[must_use]
    pub fn effective_attribute<'a>(
        &'a self,
        media: &'a MediaDescription,
        name: &str,
    ) -> Option<&'a str> {
        media.attribute(name).or_else(|| self.attribute(name))
    }

    fn validate_media(&self, media: &MediaDescription, mid: &str) -> Result<(), SdpError> {
        if media.port == 0 {
            return Ok(());
        }
        if self.effective_attribute(media, "ice-ufrag").is_none()
            || self.effective_attribute(media, "ice-pwd").is_none()
        {
            return Err(SdpError::new(SdpErrorKind::MissingIceCredentials(
                mid.to_owned(),
            )));
        }
        if self.effective_attribute(media, "fingerprint").is_none() {
            return Err(SdpError::new(SdpErrorKind::MissingFingerprint(
                mid.to_owned(),
            )));
        }
        if !matches!(
            self.effective_attribute(media, "setup")
                .and_then(SetupRole::parse),
            Some(SetupRole::ActPass | SetupRole::Active)
        ) {
            return Err(SdpError::new(SdpErrorKind::InvalidSetupRole(
                mid.to_owned(),
            )));
        }
        if media.kind.is_rtp() {
            if !media.has_attribute("rtcp-mux") {
                return Err(SdpError::new(SdpErrorKind::MissingRtcpMux(mid.to_owned())));
            }
            media.codecs()?;
        }
        Ok(())
    }
}

fn parse_payload_type(value: &str) -> Result<u8, SdpError> {
    value
        .parse::<u8>()
        .ok()
        .filter(|payload_type| *payload_type <= 127)
        .ok_or_else(|| SdpError::new(SdpErrorKind::InvalidPayloadType(value.to_owned())))
}

fn parse_rtpmap(value: &str) -> Result<(String, u32, u16), SdpError> {
    let mut parts = value.split('/');
    let name = parts.next().filter(|name| !name.is_empty());
    let clock_rate = parts.next().and_then(|rate| rate.parse::<u32>().ok());
    let channels = parts
        .next()
        .map_or(Some(1), |value| value.parse::<u16>().ok());
    if parts.next().is_some() {
        return Err(SdpError::new(SdpErrorKind::InvalidRtpMap(value.to_owned())));
    }
    match (name, clock_rate, channels) {
        (Some(name), Some(clock_rate), Some(channels)) if clock_rate > 0 && channels > 0 => {
            Ok((name.to_owned(), clock_rate, channels))
        }
        _ => Err(SdpError::new(SdpErrorKind::InvalidRtpMap(value.to_owned()))),
    }
}

fn find_prefixed_attribute<'a>(
    attributes: &'a [Attribute],
    name: &str,
    target: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .filter(|attribute| attribute.name == name)
        .filter_map(Attribute::value)
        .filter_map(split_attribute_target)
        .find_map(|(candidate, value)| (candidate == target).then_some(value))
}

fn split_attribute_target(value: &str) -> Option<(&str, &str)> {
    value.split_once(' ')
}

fn parse_extmap(attribute: &Attribute) -> Result<ExtMap, SdpError> {
    let value = attribute.value().ok_or_else(|| {
        SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "extmap".to_owned(),
            reason: "missing value",
        })
    })?;
    let mut tokens = value.split_whitespace();
    let id_and_direction = tokens.next().unwrap_or_default();
    let uri = tokens.next().unwrap_or_default();
    if id_and_direction.is_empty() || uri.is_empty() {
        return Err(SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "extmap".to_owned(),
            reason: "expected id and URI",
        }));
    }
    let (id, direction) = match id_and_direction.split_once('/') {
        Some((id, direction)) => (id, Some(parse_direction(direction)?)),
        None => (id_and_direction, None),
    };
    let id = id.parse::<u8>().ok().filter(|id| *id > 0).ok_or_else(|| {
        SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "extmap".to_owned(),
            reason: "invalid ID",
        })
    })?;
    let attributes = {
        let remaining = tokens.collect::<Vec<_>>().join(" ");
        (!remaining.is_empty()).then_some(remaining)
    };
    Ok(ExtMap {
        id,
        direction,
        uri: uri.to_owned(),
        attributes,
    })
}

fn parse_rid(attribute: &Attribute) -> Result<Rid, SdpError> {
    let value = attribute.value().ok_or_else(|| {
        SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "rid".to_owned(),
            reason: "missing value",
        })
    })?;
    let mut tokens = value.splitn(3, ' ');
    let id = tokens.next().unwrap_or_default();
    let direction = tokens.next().unwrap_or_default();
    if id.is_empty() {
        return Err(SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "rid".to_owned(),
            reason: "missing RID ID",
        }));
    }
    let direction = match direction {
        "send" => Direction::SendOnly,
        "recv" => Direction::RecvOnly,
        _ => {
            return Err(SdpError::new(SdpErrorKind::InvalidAttribute {
                name: "rid".to_owned(),
                reason: "direction must be send or recv",
            }));
        }
    };
    Ok(Rid {
        id: id.to_owned(),
        direction,
        restrictions: tokens.next().map(str::to_owned),
    })
}

fn parse_direction(value: &str) -> Result<Direction, SdpError> {
    match value {
        "sendrecv" => Ok(Direction::SendRecv),
        "sendonly" => Ok(Direction::SendOnly),
        "recvonly" => Ok(Direction::RecvOnly),
        "inactive" => Ok(Direction::Inactive),
        _ => Err(SdpError::new(SdpErrorKind::InvalidAttribute {
            name: "direction".to_owned(),
            reason: "unknown direction",
        })),
    }
}
