use crate::{Attribute, MediaDescription, MediaKind, SdpError, SdpErrorKind, SessionDescription};

const MAX_SDP_BYTES: usize = 256 * 1024;
const MAX_LINE_BYTES: usize = 4096;

#[derive(Debug, Default)]
struct SessionBuilder {
    version: Option<String>,
    origin: Option<String>,
    session_name: Option<String>,
    timing: Option<String>,
    connection: Option<String>,
    attributes: Vec<Attribute>,
    media: Vec<MediaDescription>,
}

pub(crate) fn parse(input: &str) -> Result<SessionDescription, SdpError> {
    if input.len() > MAX_SDP_BYTES {
        return Err(SdpError::new(SdpErrorKind::DocumentTooLarge(input.len())));
    }

    let mut builder = SessionBuilder::default();
    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(SdpError::at_line(
                line_number,
                SdpErrorKind::LineTooLong(line.len()),
            ));
        }
        parse_line(&mut builder, line_number, line)?;
    }
    finish(builder)
}

fn parse_line(
    builder: &mut SessionBuilder,
    line_number: usize,
    line: &str,
) -> Result<(), SdpError> {
    let bytes = line.as_bytes();
    if bytes.len() < 2 || bytes.get(1) != Some(&b'=') {
        return Err(SdpError::at_line(line_number, SdpErrorKind::InvalidLine));
    }
    let prefix = char::from(bytes[0]);
    let value = line.get(2..).unwrap_or_default();

    match prefix {
        'v' => set_once(&mut builder.version, value, line_number, "v"),
        'o' => set_once(&mut builder.origin, value, line_number, "o"),
        's' => set_once(&mut builder.session_name, value, line_number, "s"),
        't' => set_once(&mut builder.timing, value, line_number, "t"),
        'c' => set_connection(builder, line_number, value),
        'a' => push_attribute(builder, value),
        'm' => push_media(builder, line_number, value),
        'i' | 'b' | 'k' => {
            if builder.media.is_empty() {
                Err(SdpError::at_line(
                    line_number,
                    SdpErrorKind::MediaLineWithoutMedia(prefix),
                ))
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

fn set_once(
    target: &mut Option<String>,
    value: &str,
    line_number: usize,
    field: &'static str,
) -> Result<(), SdpError> {
    if target.is_some() {
        return Err(SdpError::at_line(
            line_number,
            SdpErrorKind::InvalidAttribute {
                name: field.to_owned(),
                reason: "duplicate session field",
            },
        ));
    }
    *target = Some(value.to_owned());
    Ok(())
}

fn set_connection(
    builder: &mut SessionBuilder,
    line_number: usize,
    value: &str,
) -> Result<(), SdpError> {
    if let Some(media) = builder.media.last_mut() {
        if media.connection.is_some() {
            return Err(SdpError::at_line(
                line_number,
                SdpErrorKind::InvalidAttribute {
                    name: "c".to_owned(),
                    reason: "duplicate media connection",
                },
            ));
        }
        media.connection = Some(value.to_owned());
    } else {
        set_once(
            &mut builder.connection,
            value,
            line_number,
            "session connection",
        )?;
    }
    Ok(())
}

fn push_attribute(builder: &mut SessionBuilder, value: &str) -> Result<(), SdpError> {
    let (name, value) = value
        .split_once(':')
        .map_or((value, None), |(name, value)| (name, Some(value)));
    if name.is_empty() {
        return Err(SdpError::new(SdpErrorKind::InvalidAttribute {
            name: String::new(),
            reason: "empty attribute name",
        }));
    }
    let attribute = Attribute::new(name, value);
    if let Some(media) = builder.media.last_mut() {
        media.attributes.push(attribute);
    } else {
        builder.attributes.push(attribute);
    }
    Ok(())
}

fn push_media(
    builder: &mut SessionBuilder,
    line_number: usize,
    value: &str,
) -> Result<(), SdpError> {
    let mut tokens = value.split_whitespace();
    let kind = tokens.next().map(MediaKind::parse);
    let port = tokens.next().and_then(|value| value.parse::<u16>().ok());
    let protocol = tokens.next();
    let formats: Vec<String> = tokens.map(str::to_owned).collect();
    let (Some(kind), Some(port), Some(protocol)) = (kind, port, protocol) else {
        return Err(SdpError::at_line(
            line_number,
            SdpErrorKind::InvalidMediaLine,
        ));
    };
    if formats.is_empty() {
        return Err(SdpError::at_line(
            line_number,
            SdpErrorKind::InvalidMediaLine,
        ));
    }
    builder.media.push(MediaDescription {
        kind,
        port,
        protocol: protocol.to_owned(),
        formats,
        connection: None,
        attributes: Vec::new(),
    });
    Ok(())
}

fn finish(builder: SessionBuilder) -> Result<SessionDescription, SdpError> {
    let version = builder
        .version
        .ok_or_else(|| SdpError::new(SdpErrorKind::MissingSessionField("v")))?;
    if version != "0" {
        return Err(SdpError::new(SdpErrorKind::UnsupportedVersion(version)));
    }
    Ok(SessionDescription {
        version,
        origin: builder
            .origin
            .ok_or_else(|| SdpError::new(SdpErrorKind::MissingSessionField("o")))?,
        session_name: builder
            .session_name
            .ok_or_else(|| SdpError::new(SdpErrorKind::MissingSessionField("s")))?,
        timing: builder
            .timing
            .ok_or_else(|| SdpError::new(SdpErrorKind::MissingSessionField("t")))?,
        connection: builder.connection,
        attributes: builder.attributes,
        media: builder.media,
    })
}
