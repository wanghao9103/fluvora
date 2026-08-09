use crate::RtpError;

/// RFC 8285 header-extension wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionFormat {
    /// Compact 4-bit identifier and 4-bit length format (`0xBEDE`).
    OneByte,
    /// 8-bit identifier and 8-bit length format (`0x1000` application bits).
    TwoByte,
    /// An unrecognized extension profile retained as opaque bytes.
    Opaque(u16),
}

impl ExtensionFormat {
    pub(crate) const fn from_profile(profile: u16) -> Self {
        if profile == 0xbede {
            Self::OneByte
        } else if profile & 0xfff0 == 0x1000 {
            Self::TwoByte
        } else {
            Self::Opaque(profile)
        }
    }

    pub(crate) const fn profile(self) -> u16 {
        match self {
            Self::OneByte => 0xbede,
            Self::TwoByte => 0x1000,
            Self::Opaque(profile) => profile,
        }
    }
}

/// One borrowed RFC 8285 extension element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderExtension<'a> {
    /// Negotiated extension identifier.
    pub id: u8,
    /// Unpadded element bytes.
    pub value: &'a [u8],
}

/// One owned extension element used while building packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedHeaderExtension {
    /// Negotiated extension identifier.
    pub id: u8,
    /// Unpadded element bytes.
    pub value: Vec<u8>,
}

pub(crate) fn parse_extensions(
    format: ExtensionFormat,
    data: &[u8],
) -> Result<Vec<HeaderExtension<'_>>, RtpError> {
    match format {
        ExtensionFormat::OneByte => parse_one_byte(data),
        ExtensionFormat::TwoByte => parse_two_byte(data),
        ExtensionFormat::Opaque(_) => Ok(Vec::new()),
    }
}

fn parse_one_byte(data: &[u8]) -> Result<Vec<HeaderExtension<'_>>, RtpError> {
    let mut extensions = Vec::new();
    let mut position = 0;
    while let Some(&header) = data.get(position) {
        position += 1;
        if header == 0 {
            continue;
        }
        let id = header >> 4;
        if id == 15 {
            break;
        }
        let length = usize::from((header & 0x0f) + 1);
        let end = position
            .checked_add(length)
            .ok_or(RtpError::TruncatedExtension)?;
        let value = data
            .get(position..end)
            .ok_or(RtpError::TruncatedExtension)?;
        extensions.push(HeaderExtension { id, value });
        position = end;
    }
    Ok(extensions)
}

fn parse_two_byte(data: &[u8]) -> Result<Vec<HeaderExtension<'_>>, RtpError> {
    let mut extensions = Vec::new();
    let mut position = 0;
    while let Some(&id) = data.get(position) {
        position += 1;
        if id == 0 {
            continue;
        }
        let length = usize::from(*data.get(position).ok_or(RtpError::TruncatedExtension)?);
        position += 1;
        let end = position
            .checked_add(length)
            .ok_or(RtpError::TruncatedExtension)?;
        let value = data
            .get(position..end)
            .ok_or(RtpError::TruncatedExtension)?;
        extensions.push(HeaderExtension { id, value });
        position = end;
    }
    Ok(extensions)
}

pub(crate) fn encode_extensions(
    format: ExtensionFormat,
    extensions: &[OwnedHeaderExtension],
) -> Result<Vec<u8>, RtpError> {
    match format {
        ExtensionFormat::OneByte => encode_one_byte(extensions),
        ExtensionFormat::TwoByte => encode_two_byte(extensions),
        ExtensionFormat::Opaque(_) => {
            if extensions.is_empty() {
                Ok(Vec::new())
            } else {
                Err(RtpError::InvalidExtensionId { format, id: 0 })
            }
        }
    }
}

fn encode_one_byte(extensions: &[OwnedHeaderExtension]) -> Result<Vec<u8>, RtpError> {
    let mut output = Vec::new();
    for extension in extensions {
        if !(1..=14).contains(&extension.id) {
            return Err(RtpError::InvalidExtensionId {
                format: ExtensionFormat::OneByte,
                id: extension.id,
            });
        }
        if !(1..=16).contains(&extension.value.len()) {
            return Err(RtpError::InvalidExtensionLength {
                format: ExtensionFormat::OneByte,
                length: extension.value.len(),
            });
        }
        let encoded_length = u8::try_from(extension.value.len() - 1)
            .map_err(|_| RtpError::ExtensionBlockTooLarge)?;
        output.push((extension.id << 4) | encoded_length);
        output.extend_from_slice(&extension.value);
    }
    Ok(output)
}

fn encode_two_byte(extensions: &[OwnedHeaderExtension]) -> Result<Vec<u8>, RtpError> {
    let mut output = Vec::new();
    for extension in extensions {
        if extension.id == 0 {
            return Err(RtpError::InvalidExtensionId {
                format: ExtensionFormat::TwoByte,
                id: extension.id,
            });
        }
        let length =
            u8::try_from(extension.value.len()).map_err(|_| RtpError::InvalidExtensionLength {
                format: ExtensionFormat::TwoByte,
                length: extension.value.len(),
            })?;
        output.extend_from_slice(&[extension.id, length]);
        output.extend_from_slice(&extension.value);
    }
    Ok(output)
}
