use crate::extension::{encode_extensions, parse_extensions};
use crate::{ExtensionFormat, HeaderExtension, OwnedHeaderExtension, RtpError};

const FIXED_HEADER_LEN: usize = 12;
const MAX_PACKET_LEN: usize = 65_535;

/// Validates the clear RTP header and returns its complete byte length.
///
/// Unlike [`Packet::parse`], this function does not inspect payload padding and is therefore safe
/// to use before SRTP payload decryption.
///
/// # Errors
///
/// Returns [`RtpError`] when fixed, CSRC, or extension header fields are truncated or malformed.
pub fn parse_header_length(input: &[u8]) -> Result<usize, RtpError> {
    if input.len() < FIXED_HEADER_LEN {
        return Err(RtpError::PacketTooShort(input.len()));
    }
    let first = input[0];
    let version = first >> 6;
    if version != 2 {
        return Err(RtpError::UnsupportedVersion(version));
    }
    let csrc_count = usize::from(first & 0x0f);
    let mut position = FIXED_HEADER_LEN
        .checked_add(csrc_count * 4)
        .ok_or(RtpError::TruncatedHeader)?;
    if input.get(..position).is_none() {
        return Err(RtpError::TruncatedHeader);
    }
    if first & 0x10 != 0 {
        let words = usize::from(read_u16(input, position + 2)?);
        position = position
            .checked_add(4)
            .and_then(|value| value.checked_add(words * 4))
            .ok_or(RtpError::TruncatedHeader)?;
        if input.get(..position).is_none() {
            return Err(RtpError::TruncatedHeader);
        }
    }
    Ok(position)
}

/// Decoded fixed and variable RTP header fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// RTP marker bit.
    pub marker: bool,
    /// Seven-bit RTP payload type.
    pub payload_type: u8,
    /// RTP sequence number.
    pub sequence_number: u16,
    /// RTP media timestamp.
    pub timestamp: u32,
    /// Synchronization source.
    pub ssrc: u32,
    /// Contributing sources in wire order.
    pub csrcs: Vec<u32>,
}

/// One negotiated per-subscriber RTP header-extension transformation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRewrite {
    /// Publisher-side extension identifier.
    pub source_id: u8,
    /// Subscriber-side identifier, or `None` to remove the element.
    pub destination_id: Option<u8>,
    /// Replacement value, or `None` to preserve the publisher value.
    pub replacement: Option<Vec<u8>>,
}

/// A validated borrowed RTP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<'a> {
    header: Header,
    header_len: usize,
    extension_format: Option<ExtensionFormat>,
    extension_data: Option<&'a [u8]>,
    extensions: Vec<HeaderExtension<'a>>,
    payload: &'a [u8],
    padding_len: usize,
}

impl<'a> Packet<'a> {
    /// Parses one complete RTP datagram.
    ///
    /// # Errors
    ///
    /// Returns [`RtpError`] when header lengths, extensions, or padding are malformed.
    pub fn parse(input: &'a [u8]) -> Result<Self, RtpError> {
        if input.len() < FIXED_HEADER_LEN {
            return Err(RtpError::PacketTooShort(input.len()));
        }
        let first = input[0];
        let version = first >> 6;
        if version != 2 {
            return Err(RtpError::UnsupportedVersion(version));
        }
        let has_padding = first & 0x20 != 0;
        let has_extension = first & 0x10 != 0;
        let csrc_count = usize::from(first & 0x0f);
        let mut position = FIXED_HEADER_LEN;
        let mut csrcs = Vec::with_capacity(csrc_count);
        for _ in 0..csrc_count {
            csrcs.push(read_u32(input, position)?);
            position += 4;
        }

        let (extension_format, extension_data, extensions) = if has_extension {
            let profile = read_u16(input, position)?;
            let words = usize::from(read_u16(input, position + 2)?);
            position += 4;
            let byte_len = words.checked_mul(4).ok_or(RtpError::TruncatedHeader)?;
            let end = position
                .checked_add(byte_len)
                .ok_or(RtpError::TruncatedHeader)?;
            let data = input.get(position..end).ok_or(RtpError::TruncatedHeader)?;
            position = end;
            let format = ExtensionFormat::from_profile(profile);
            let parsed = parse_extensions(format, data)?;
            (Some(format), Some(data), parsed)
        } else {
            (None, None, Vec::new())
        };

        let payload_and_padding = input.get(position..).ok_or(RtpError::TruncatedHeader)?;
        let padding_len = parse_padding(has_padding, payload_and_padding)?;
        let payload_len = payload_and_padding
            .len()
            .checked_sub(padding_len)
            .ok_or(RtpError::InvalidPadding(padding_len))?;
        let payload = payload_and_padding
            .get(..payload_len)
            .ok_or(RtpError::InvalidPadding(padding_len))?;

        Ok(Self {
            header: Header {
                marker: input[1] & 0x80 != 0,
                payload_type: input[1] & 0x7f,
                sequence_number: read_u16(input, 2)?,
                timestamp: read_u32(input, 4)?,
                ssrc: read_u32(input, 8)?,
                csrcs,
            },
            header_len: position,
            extension_format,
            extension_data,
            extensions,
            payload,
            padding_len,
        })
    }

    /// Returns the decoded RTP header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// Returns the complete clear-text RTP header length.
    #[must_use]
    pub const fn header_len(&self) -> usize {
        self.header_len
    }

    /// Returns the negotiated extension block format.
    #[must_use]
    pub const fn extension_format(&self) -> Option<ExtensionFormat> {
        self.extension_format
    }

    /// Returns raw, word-padded extension block bytes.
    #[must_use]
    pub const fn extension_data(&self) -> Option<&'a [u8]> {
        self.extension_data
    }

    /// Returns parsed extension elements. Opaque profiles have no parsed elements.
    #[must_use]
    pub fn extensions(&self) -> &[HeaderExtension<'a>] {
        &self.extensions
    }

    /// Returns media payload without RTP padding.
    #[must_use]
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Returns the validated trailing padding byte count.
    #[must_use]
    pub const fn padding_len(&self) -> usize {
        self.padding_len
    }
}

/// Rebuilds an RTP packet after applying negotiated RFC 8285 extension transformations.
///
/// This is used to replace MID, remove RID, and remap transport-wide sequence extension IDs
/// between independently negotiated publisher and subscriber peer connections.
///
/// # Errors
///
/// Returns [`RtpError`] for malformed packets, opaque extension profiles, or invalid rewritten
/// element identifiers/lengths.
pub fn rewrite_header_extensions(
    input: &[u8],
    rewrites: &[ExtensionRewrite],
) -> Result<Vec<u8>, RtpError> {
    if rewrites.is_empty() {
        return Ok(input.to_vec());
    }
    let packet = Packet::parse(input)?;
    let format = packet
        .extension_format()
        .filter(|format| !matches!(format, ExtensionFormat::Opaque(_)))
        .ok_or(RtpError::UnsupportedExtensionRewrite)?;
    let extensions = packet
        .extensions()
        .iter()
        .filter_map(|extension| {
            let rewrite = rewrites
                .iter()
                .find(|rewrite| rewrite.source_id == extension.id);
            match rewrite {
                Some(ExtensionRewrite {
                    destination_id: None,
                    ..
                }) => None,
                Some(rewrite) => Some(OwnedHeaderExtension {
                    id: rewrite.destination_id.unwrap_or(extension.id),
                    value: rewrite
                        .replacement
                        .clone()
                        .unwrap_or_else(|| extension.value.to_vec()),
                }),
                None => Some(OwnedHeaderExtension {
                    id: extension.id,
                    value: extension.value.to_vec(),
                }),
            }
        })
        .collect::<Vec<_>>();
    PacketBuilder::new(
        packet.header().payload_type,
        packet.header().sequence_number,
        packet.header().timestamp,
        packet.header().ssrc,
        packet.payload(),
    )
    .marker(packet.header().marker)
    .csrcs(packet.header().csrcs.clone())
    .extensions(format, extensions)
    .padding(packet.padding_len())
    .build()
}

/// Fluent encoder for a complete RTP packet.
#[derive(Debug, Clone)]
pub struct PacketBuilder<'a> {
    header: Header,
    payload: &'a [u8],
    extension_format: Option<ExtensionFormat>,
    extensions: Vec<OwnedHeaderExtension>,
    padding_len: usize,
}

impl<'a> PacketBuilder<'a> {
    /// Creates an RTP packet with no CSRCs, extensions, marker, or padding.
    #[must_use]
    pub const fn new(
        payload_type: u8,
        sequence_number: u16,
        timestamp: u32,
        ssrc: u32,
        payload: &'a [u8],
    ) -> Self {
        Self {
            header: Header {
                marker: false,
                payload_type,
                sequence_number,
                timestamp,
                ssrc,
                csrcs: Vec::new(),
            },
            payload,
            extension_format: None,
            extensions: Vec::new(),
            padding_len: 0,
        }
    }

    /// Sets the RTP marker.
    #[must_use]
    pub const fn marker(mut self, marker: bool) -> Self {
        self.header.marker = marker;
        self
    }

    /// Sets CSRC identifiers.
    #[must_use]
    pub fn csrcs(mut self, csrcs: impl Into<Vec<u32>>) -> Self {
        self.header.csrcs = csrcs.into();
        self
    }

    /// Sets RFC 8285 extension elements.
    #[must_use]
    pub fn extensions(
        mut self,
        format: ExtensionFormat,
        extensions: impl Into<Vec<OwnedHeaderExtension>>,
    ) -> Self {
        self.extension_format = Some(format);
        self.extensions = extensions.into();
        self
    }

    /// Sets trailing RTP padding bytes.
    #[must_use]
    pub const fn padding(mut self, padding_len: usize) -> Self {
        self.padding_len = padding_len;
        self
    }

    /// Encodes the packet.
    ///
    /// # Errors
    ///
    /// Returns [`RtpError`] for invalid field ranges or an oversized packet.
    pub fn build(self) -> Result<Vec<u8>, RtpError> {
        validate_builder(&self)?;
        let extension_data = self
            .extension_format
            .map(|format| encode_extensions(format, &self.extensions))
            .transpose()?;
        let extension_len = extension_data.as_ref().map_or(0, Vec::len);
        let padded_extension_len = extension_len
            .checked_add((4 - extension_len % 4) % 4)
            .ok_or(RtpError::ExtensionBlockTooLarge)?;
        let extension_words = u16::try_from(padded_extension_len / 4)
            .map_err(|_| RtpError::ExtensionBlockTooLarge)?;
        let total_len = FIXED_HEADER_LEN
            .checked_add(self.header.csrcs.len() * 4)
            .and_then(|length| {
                length.checked_add(if extension_data.is_some() {
                    4 + padded_extension_len
                } else {
                    0
                })
            })
            .and_then(|length| length.checked_add(self.payload.len()))
            .and_then(|length| length.checked_add(self.padding_len))
            .ok_or(RtpError::PacketTooLarge)?;
        if total_len > MAX_PACKET_LEN {
            return Err(RtpError::PacketTooLarge);
        }

        let mut output = Vec::with_capacity(total_len);
        encode_fixed_header(
            &mut output,
            &self.header,
            extension_data.is_some(),
            self.padding_len,
        );
        for csrc in self.header.csrcs {
            output.extend_from_slice(&csrc.to_be_bytes());
        }
        if let (Some(format), Some(data)) = (self.extension_format, extension_data) {
            output.extend_from_slice(&format.profile().to_be_bytes());
            output.extend_from_slice(&extension_words.to_be_bytes());
            output.extend_from_slice(&data);
            output.resize(output.len() + (padded_extension_len - data.len()), 0);
        }
        output.extend_from_slice(self.payload);
        if self.padding_len > 0 {
            output.resize(total_len, 0);
            let last = output.last_mut().ok_or(RtpError::PacketTooLarge)?;
            *last = u8::try_from(self.padding_len)
                .map_err(|_| RtpError::PaddingTooLarge(self.padding_len))?;
        }
        Ok(output)
    }
}

/// Optional SFU rewrites applied without reallocating the datagram.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rewrite {
    /// Replacement marker bit.
    pub marker: Option<bool>,
    /// Replacement payload type.
    pub payload_type: Option<u8>,
    /// Replacement sequence number.
    pub sequence_number: Option<u16>,
    /// Replacement RTP timestamp.
    pub timestamp: Option<u32>,
    /// Replacement SSRC.
    pub ssrc: Option<u32>,
}

impl Rewrite {
    /// Validates and rewrites fixed header fields in place.
    ///
    /// # Errors
    ///
    /// Returns [`RtpError`] if the original packet is malformed or payload type exceeds 127.
    pub fn apply(self, packet: &mut [u8]) -> Result<(), RtpError> {
        Packet::parse(packet)?;
        if let Some(payload_type) = self.payload_type {
            if payload_type > 127 {
                return Err(RtpError::InvalidPayloadType(payload_type));
            }
            packet[1] = (packet[1] & 0x80) | payload_type;
        }
        if let Some(marker) = self.marker {
            packet[1] = (packet[1] & 0x7f) | (u8::from(marker) << 7);
        }
        if let Some(sequence_number) = self.sequence_number {
            packet[2..4].copy_from_slice(&sequence_number.to_be_bytes());
        }
        if let Some(timestamp) = self.timestamp {
            packet[4..8].copy_from_slice(&timestamp.to_be_bytes());
        }
        if let Some(ssrc) = self.ssrc {
            packet[8..12].copy_from_slice(&ssrc.to_be_bytes());
        }
        Ok(())
    }
}

fn validate_builder(builder: &PacketBuilder<'_>) -> Result<(), RtpError> {
    if builder.header.payload_type > 127 {
        return Err(RtpError::InvalidPayloadType(builder.header.payload_type));
    }
    if builder.header.csrcs.len() > 15 {
        return Err(RtpError::TooManyCsrcs(builder.header.csrcs.len()));
    }
    if builder.padding_len > usize::from(u8::MAX) {
        return Err(RtpError::PaddingTooLarge(builder.padding_len));
    }
    Ok(())
}

fn encode_fixed_header(
    output: &mut Vec<u8>,
    header: &Header,
    has_extension: bool,
    padding_len: usize,
) {
    let mut first = 2 << 6;
    first |= u8::from(padding_len > 0) << 5;
    first |= u8::from(has_extension) << 4;
    first |= u8::try_from(header.csrcs.len()).unwrap_or_default();
    output.push(first);
    output.push((u8::from(header.marker) << 7) | header.payload_type);
    output.extend_from_slice(&header.sequence_number.to_be_bytes());
    output.extend_from_slice(&header.timestamp.to_be_bytes());
    output.extend_from_slice(&header.ssrc.to_be_bytes());
}

fn parse_padding(has_padding: bool, payload: &[u8]) -> Result<usize, RtpError> {
    if !has_padding {
        return Ok(0);
    }
    let padding_len = usize::from(*payload.last().ok_or(RtpError::InvalidPadding(0))?);
    if padding_len == 0 || padding_len > payload.len() {
        Err(RtpError::InvalidPadding(padding_len))
    } else {
        Ok(padding_len)
    }
}

fn read_u16(input: &[u8], position: usize) -> Result<u16, RtpError> {
    let bytes: [u8; 2] = input
        .get(position..position.saturating_add(2))
        .ok_or(RtpError::TruncatedHeader)?
        .try_into()
        .map_err(|_| RtpError::TruncatedHeader)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(input: &[u8], position: usize) -> Result<u32, RtpError> {
    let bytes: [u8; 4] = input
        .get(position..position.saturating_add(4))
        .ok_or(RtpError::TruncatedHeader)?
        .try_into()
        .map_err(|_| RtpError::TruncatedHeader)?;
    Ok(u32::from_be_bytes(bytes))
}
