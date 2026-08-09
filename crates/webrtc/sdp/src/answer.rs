use std::collections::HashSet;
use std::fmt::Write;

use crate::{MediaDescription, MediaKind, RtpCodec, SdpError, SdpErrorKind, SessionDescription};

const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// A codec the SFU can receive and forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecCapability {
    /// Case-insensitive encoding name.
    pub name: String,
    /// RTP clock rate.
    pub clock_rate: u32,
    /// Required audio channels, normally one for video.
    pub channels: u16,
}

impl CodecCapability {
    /// Creates a capability.
    #[must_use]
    pub fn new(name: impl Into<String>, clock_rate: u32, channels: u16) -> Self {
        Self {
            name: name.into(),
            clock_rate,
            channels,
        }
    }

    fn matches(&self, codec: &RtpCodec) -> bool {
        self.name.eq_ignore_ascii_case(&codec.name)
            && self.clock_rate == codec.clock_rate
            && self.channels == codec.channels
    }
}

/// Server-controlled values used to answer a browser offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerConfig {
    /// Numeric SDP session identifier.
    pub session_id: u64,
    /// ICE-lite local username fragment.
    pub ice_ufrag: String,
    /// ICE-lite local password.
    pub ice_password: String,
    /// DTLS fingerprint algorithm, normally `sha-256`.
    pub fingerprint_algorithm: String,
    /// Colon-separated DTLS certificate fingerprint.
    pub fingerprint: String,
    /// Host candidate attributes without the `a=` prefix.
    pub candidates: Vec<String>,
    /// Accepted audio codecs.
    pub audio_codecs: Vec<CodecCapability>,
    /// Accepted video codecs.
    pub video_codecs: Vec<CodecCapability>,
    /// Accepted RTP header-extension URIs.
    pub extension_uris: HashSet<String>,
    /// Whether to accept an offered application/DataChannel section.
    pub accept_data_channel: bool,
}

impl AnswerConfig {
    /// Creates the MVP Opus/VP8 configuration.
    #[must_use]
    pub fn mvp(
        session_id: u64,
        ice_ufrag: impl Into<String>,
        ice_password: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            ice_ufrag: ice_ufrag.into(),
            ice_password: ice_password.into(),
            fingerprint_algorithm: "sha-256".to_owned(),
            fingerprint: fingerprint.into(),
            candidates: Vec::new(),
            audio_codecs: vec![CodecCapability::new("opus", 48_000, 2)],
            video_codecs: vec![CodecCapability::new("VP8", 90_000, 1)],
            extension_uris: HashSet::new(),
            accept_data_channel: false,
        }
    }
}

/// Creates a passive ICE-lite/DTLS SFU answer without SDP string munging.
///
/// # Errors
///
/// Returns [`SdpError`] when the offer is invalid or a non-rejected RTP media section has no
/// compatible codec.
pub fn create_sfu_answer(
    offer: &SessionDescription,
    config: &AnswerConfig,
) -> Result<String, SdpError> {
    offer.validate_webrtc_offer()?;
    let mut output = String::with_capacity(4096);
    writeln!(output, "v=0\r").map_err(|_| answer_too_large())?;
    writeln!(output, "o=- {} 2 IN IP4 0.0.0.0\r", config.session_id)
        .map_err(|_| answer_too_large())?;
    output.push_str("s=-\r\nt=0 0\r\na=ice-lite\r\n");
    write_bundle_line(&mut output, offer)?;
    writeln!(
        output,
        "a=fingerprint:{} {}\r",
        config.fingerprint_algorithm, config.fingerprint
    )
    .map_err(|_| answer_too_large())?;

    for media in &offer.media {
        write_media_answer(&mut output, media, config)?;
        if output.len() > MAX_ANSWER_BYTES {
            return Err(answer_too_large());
        }
    }
    Ok(output)
}

fn write_bundle_line(output: &mut String, offer: &SessionDescription) -> Result<(), SdpError> {
    output.push_str("a=group:BUNDLE");
    for media in &offer.media {
        let mid = media
            .mid()
            .ok_or_else(|| SdpError::new(SdpErrorKind::MissingMid))?;
        write!(output, " {mid}").map_err(|_| answer_too_large())?;
    }
    output.push_str("\r\n");
    Ok(())
}

fn write_media_answer(
    output: &mut String,
    media: &MediaDescription,
    config: &AnswerConfig,
) -> Result<(), SdpError> {
    let mid = media
        .mid()
        .ok_or_else(|| SdpError::new(SdpErrorKind::MissingMid))?;
    match media.kind {
        MediaKind::Audio | MediaKind::Video => {
            let selected = select_codecs(media, config)?;
            if media.port == 0 || selected.is_empty() {
                return write_rejected_media(output, media, mid);
            }
            write_rtp_media(output, media, mid, &selected, config)
        }
        MediaKind::Application if config.accept_data_channel && media.port != 0 => {
            write_application_media(output, media, mid, config)
        }
        _ => write_rejected_media(output, media, mid),
    }
}

fn select_codecs(
    media: &MediaDescription,
    config: &AnswerConfig,
) -> Result<Vec<RtpCodec>, SdpError> {
    let offered = media.codecs()?;
    let capabilities = match media.kind {
        MediaKind::Audio => &config.audio_codecs,
        MediaKind::Video => &config.video_codecs,
        _ => return Ok(Vec::new()),
    };
    let selected_primary: HashSet<u8> = offered
        .iter()
        .filter(|codec| {
            !codec.name.eq_ignore_ascii_case("rtx")
                && capabilities
                    .iter()
                    .any(|capability| capability.matches(codec))
        })
        .map(|codec| codec.payload_type)
        .collect();
    let selected: Vec<RtpCodec> = offered
        .into_iter()
        .filter(|codec| {
            selected_primary.contains(&codec.payload_type)
                || codec
                    .associated_payload_type()
                    .is_some_and(|payload_type| selected_primary.contains(&payload_type))
        })
        .collect();
    if selected_primary.is_empty() && media.port != 0 {
        let mid = media.mid().unwrap_or("unknown");
        return Err(SdpError::new(SdpErrorKind::NoCompatibleCodec(
            mid.to_owned(),
        )));
    }
    Ok(selected)
}

fn write_rtp_media(
    output: &mut String,
    media: &MediaDescription,
    mid: &str,
    codecs: &[RtpCodec],
    config: &AnswerConfig,
) -> Result<(), SdpError> {
    write!(output, "m={} 9 {}", media.kind.as_str(), media.protocol)
        .map_err(|_| answer_too_large())?;
    for codec in codecs {
        write!(output, " {}", codec.payload_type).map_err(|_| answer_too_large())?;
    }
    output.push_str("\r\nc=IN IP4 0.0.0.0\r\n");
    write_transport_attributes(output, mid, media.direction().reciprocal(), config)?;
    output.push_str("a=rtcp-mux\r\n");

    for codec in codecs {
        write_codec(output, codec)?;
    }
    write_extensions(output, media, config)?;
    write_candidates(output, config);
    Ok(())
}

fn write_codec(output: &mut String, codec: &RtpCodec) -> Result<(), SdpError> {
    write!(
        output,
        "a=rtpmap:{} {}/{}",
        codec.payload_type, codec.name, codec.clock_rate
    )
    .map_err(|_| answer_too_large())?;
    if codec.channels > 1 {
        write!(output, "/{}", codec.channels).map_err(|_| answer_too_large())?;
    }
    output.push_str("\r\n");
    if let Some(fmtp) = &codec.fmtp {
        writeln!(output, "a=fmtp:{} {fmtp}\r", codec.payload_type)
            .map_err(|_| answer_too_large())?;
    }
    for feedback in &codec.rtcp_feedback {
        writeln!(output, "a=rtcp-fb:{} {feedback}\r", codec.payload_type)
            .map_err(|_| answer_too_large())?;
    }
    Ok(())
}

fn write_extensions(
    output: &mut String,
    media: &MediaDescription,
    config: &AnswerConfig,
) -> Result<(), SdpError> {
    for extension in media.extmaps()? {
        if config.extension_uris.contains(&extension.uri) {
            write!(output, "a=extmap:{}", extension.id).map_err(|_| answer_too_large())?;
            if let Some(direction) = extension.direction {
                write!(output, "/{}", direction.reciprocal().as_str())
                    .map_err(|_| answer_too_large())?;
            }
            write!(output, " {}", extension.uri).map_err(|_| answer_too_large())?;
            if let Some(attributes) = extension.attributes {
                write!(output, " {attributes}").map_err(|_| answer_too_large())?;
            }
            output.push_str("\r\n");
        }
    }
    Ok(())
}

fn write_application_media(
    output: &mut String,
    media: &MediaDescription,
    mid: &str,
    config: &AnswerConfig,
) -> Result<(), SdpError> {
    write!(
        output,
        "m=application 9 {} {}\r\nc=IN IP4 0.0.0.0\r\n",
        media.protocol,
        media.formats.join(" ")
    )
    .map_err(|_| answer_too_large())?;
    write_transport_attributes(output, mid, media.direction().reciprocal(), config)?;
    for value in media.attribute_values("sctp-port") {
        writeln!(output, "a=sctp-port:{value}\r").map_err(|_| answer_too_large())?;
    }
    for value in media.attribute_values("max-message-size") {
        writeln!(output, "a=max-message-size:{value}\r").map_err(|_| answer_too_large())?;
    }
    write_candidates(output, config);
    Ok(())
}

fn write_transport_attributes(
    output: &mut String,
    mid: &str,
    direction: crate::Direction,
    config: &AnswerConfig,
) -> Result<(), SdpError> {
    writeln!(output, "a=mid:{mid}\r").map_err(|_| answer_too_large())?;
    writeln!(output, "a=ice-ufrag:{}\r", config.ice_ufrag).map_err(|_| answer_too_large())?;
    writeln!(output, "a=ice-pwd:{}\r", config.ice_password).map_err(|_| answer_too_large())?;
    output.push_str("a=setup:passive\r\n");
    writeln!(output, "a={}\r", direction.as_str()).map_err(|_| answer_too_large())?;
    Ok(())
}

fn write_candidates(output: &mut String, config: &AnswerConfig) {
    for candidate in &config.candidates {
        output.push_str("a=candidate:");
        output.push_str(candidate);
        output.push_str("\r\n");
    }
    output.push_str("a=end-of-candidates\r\n");
}

fn write_rejected_media(
    output: &mut String,
    media: &MediaDescription,
    mid: &str,
) -> Result<(), SdpError> {
    writeln!(
        output,
        "m={} 0 {} {}\r",
        media.kind.as_str(),
        media.protocol,
        media.formats.join(" ")
    )
    .map_err(|_| answer_too_large())?;
    output.push_str("c=IN IP4 0.0.0.0\r\n");
    writeln!(output, "a=mid:{mid}\r").map_err(|_| answer_too_large())?;
    output.push_str("a=inactive\r\n");
    Ok(())
}

const fn answer_too_large() -> SdpError {
    SdpError::new(SdpErrorKind::AnswerTooLarge)
}
