//! Lightweight encoded-payload inspection for SFU routing decisions.

use std::fmt;

/// Media codec whose payload can be routed without decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Opus audio.
    Opus,
    /// VP8 video.
    Vp8,
    /// VP9 video.
    Vp9,
    /// H.264/AVC video in RFC 6184 packetization mode.
    H264,
    /// AV1 video in RTP.
    Av1,
    /// Codec not inspected by the core.
    Unknown,
}

/// Routing metadata derived from one encoded RTP payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadInfo {
    /// Packet begins an encoded frame.
    pub start_of_frame: bool,
    /// Packet ends an encoded frame.
    pub end_of_frame: bool,
    /// Packet belongs to a random-access frame.
    pub keyframe: bool,
    /// Codec temporal layer, when carried in the payload descriptor.
    pub temporal_id: Option<u8>,
    /// Codec spatial layer, when carried in the payload descriptor.
    pub spatial_id: Option<u8>,
}

/// Parses only the payload metadata needed for forwarding and layer switching.
///
/// # Errors
///
/// Returns [`PayloadError`] when a known codec descriptor or aggregation structure is truncated.
pub fn inspect_payload(
    codec: Codec,
    payload: &[u8],
    marker: bool,
) -> Result<PayloadInfo, PayloadError> {
    match codec {
        Codec::Opus => Ok(PayloadInfo {
            start_of_frame: true,
            end_of_frame: true,
            keyframe: true,
            temporal_id: None,
            spatial_id: None,
        }),
        Codec::Vp8 => inspect_vp8(payload, marker),
        Codec::Vp9 => inspect_vp9(payload, marker),
        Codec::H264 => inspect_h264(payload, marker),
        Codec::Av1 => inspect_av1(payload, marker),
        Codec::Unknown => Ok(PayloadInfo {
            start_of_frame: false,
            end_of_frame: marker,
            keyframe: false,
            temporal_id: None,
            spatial_id: None,
        }),
    }
}

fn inspect_vp8(payload: &[u8], marker: bool) -> Result<PayloadInfo, PayloadError> {
    let descriptor = *payload.first().ok_or(PayloadError::EmptyPayload)?;
    let extended = descriptor & 0x80 != 0;
    let start = descriptor & 0x10 != 0;
    let partition_id = descriptor & 0x0f;
    let mut position = 1;
    let mut temporal_id = None;
    if extended {
        let extension = read_byte(payload, position)?;
        position += 1;
        if extension & 0x80 != 0 {
            let picture_id = read_byte(payload, position)?;
            position += if picture_id & 0x80 != 0 { 2 } else { 1 };
            require_position(payload, position)?;
        }
        if extension & 0x40 != 0 {
            position += 1;
            require_position(payload, position)?;
        }
        if extension & 0x30 != 0 {
            let layer = read_byte(payload, position)?;
            position += 1;
            if extension & 0x20 != 0 {
                temporal_id = Some(layer >> 6);
            }
        }
    }
    let first_partition_payload = start && partition_id == 0;
    let keyframe = first_partition_payload && read_byte(payload, position)? & 0x01 == 0;
    Ok(PayloadInfo {
        start_of_frame: first_partition_payload,
        end_of_frame: marker,
        keyframe,
        temporal_id,
        spatial_id: None,
    })
}

fn inspect_vp9(payload: &[u8], marker: bool) -> Result<PayloadInfo, PayloadError> {
    let descriptor = *payload.first().ok_or(PayloadError::EmptyPayload)?;
    let inter_picture = descriptor & 0x40 != 0;
    let flexible = descriptor & 0x10 != 0;
    let begins_frame = descriptor & 0x08 != 0;
    let ends_frame = descriptor & 0x04 != 0;
    let mut position = 1;
    if descriptor & 0x80 != 0 {
        let picture_id = read_byte(payload, position)?;
        position += if picture_id & 0x80 != 0 { 2 } else { 1 };
        require_position(payload, position)?;
    }
    let (temporal_id, spatial_id) = if descriptor & 0x20 != 0 {
        let layer = read_byte(payload, position)?;
        position += 1;
        if !flexible {
            position += 1;
            require_position(payload, position)?;
        }
        (Some(layer >> 5), Some((layer >> 1) & 0x07))
    } else {
        (None, None)
    };
    if flexible && inter_picture {
        loop {
            let reference = read_byte(payload, position)?;
            position += 1;
            if reference & 0x01 == 0 {
                break;
            }
        }
    }
    if descriptor & 0x02 != 0 {
        parse_vp9_scalability_structure(payload, &mut position)?;
    }
    require_position(payload, position)?;
    Ok(PayloadInfo {
        start_of_frame: begins_frame,
        end_of_frame: ends_frame || marker,
        keyframe: begins_frame && !inter_picture,
        temporal_id,
        spatial_id,
    })
}

fn parse_vp9_scalability_structure(
    payload: &[u8],
    position: &mut usize,
) -> Result<(), PayloadError> {
    let header = read_byte(payload, *position)?;
    *position += 1;
    let spatial_layers = usize::from((header >> 5) + 1);
    if header & 0x10 != 0 {
        *position = position
            .checked_add(spatial_layers * 4)
            .ok_or(PayloadError::TruncatedDescriptor)?;
        require_position(payload, *position)?;
    }
    if header & 0x08 != 0 {
        let groups = usize::from(read_byte(payload, *position)?);
        *position += 1;
        for _ in 0..groups {
            let group = read_byte(payload, *position)?;
            *position += 1 + usize::from(group & 0x03);
            require_position(payload, *position)?;
        }
    }
    Ok(())
}

fn inspect_h264(payload: &[u8], marker: bool) -> Result<PayloadInfo, PayloadError> {
    let nal_header = *payload.first().ok_or(PayloadError::EmptyPayload)?;
    let nal_type = nal_header & 0x1f;
    match nal_type {
        1..=23 => Ok(PayloadInfo {
            start_of_frame: true,
            end_of_frame: marker,
            keyframe: nal_type == 5,
            temporal_id: None,
            spatial_id: None,
        }),
        24 => inspect_h264_stap_a(payload, marker),
        28 => {
            let fu_header = read_byte(payload, 1)?;
            Ok(PayloadInfo {
                start_of_frame: fu_header & 0x80 != 0,
                end_of_frame: fu_header & 0x40 != 0 || marker,
                keyframe: fu_header & 0x1f == 5,
                temporal_id: None,
                spatial_id: None,
            })
        }
        unsupported => Err(PayloadError::UnsupportedH264NalType(unsupported)),
    }
}

fn inspect_h264_stap_a(payload: &[u8], marker: bool) -> Result<PayloadInfo, PayloadError> {
    let mut position = 1;
    let mut keyframe = false;
    let mut units = 0;
    while position < payload.len() {
        let length = usize::from(read_u16(payload, position)?);
        position += 2;
        if length == 0 {
            return Err(PayloadError::InvalidAggregationUnit);
        }
        let end = position
            .checked_add(length)
            .ok_or(PayloadError::InvalidAggregationUnit)?;
        let unit = payload
            .get(position..end)
            .ok_or(PayloadError::InvalidAggregationUnit)?;
        keyframe |= unit[0] & 0x1f == 5;
        units += 1;
        position = end;
    }
    if units == 0 {
        return Err(PayloadError::InvalidAggregationUnit);
    }
    Ok(PayloadInfo {
        start_of_frame: true,
        end_of_frame: marker,
        keyframe,
        temporal_id: None,
        spatial_id: None,
    })
}

fn inspect_av1(payload: &[u8], marker: bool) -> Result<PayloadInfo, PayloadError> {
    let aggregation = *payload.first().ok_or(PayloadError::EmptyPayload)?;
    let continues_previous = aggregation & 0x80 != 0;
    let continues_next = aggregation & 0x40 != 0;
    let new_coded_sequence = aggregation & 0x08 != 0;
    if payload.len() == 1 {
        return Err(PayloadError::TruncatedDescriptor);
    }
    Ok(PayloadInfo {
        start_of_frame: !continues_previous,
        end_of_frame: (!continues_next) || marker,
        keyframe: !continues_previous && new_coded_sequence,
        temporal_id: None,
        spatial_id: None,
    })
}

fn read_byte(payload: &[u8], position: usize) -> Result<u8, PayloadError> {
    payload
        .get(position)
        .copied()
        .ok_or(PayloadError::TruncatedDescriptor)
}

fn read_u16(payload: &[u8], position: usize) -> Result<u16, PayloadError> {
    let bytes: [u8; 2] = payload
        .get(position..position.saturating_add(2))
        .ok_or(PayloadError::TruncatedDescriptor)?
        .try_into()
        .map_err(|_| PayloadError::TruncatedDescriptor)?;
    Ok(u16::from_be_bytes(bytes))
}

fn require_position(payload: &[u8], position: usize) -> Result<(), PayloadError> {
    if position < payload.len() {
        Ok(())
    } else {
        Err(PayloadError::TruncatedDescriptor)
    }
}

/// Encoded payload inspection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadError {
    /// RTP payload was empty.
    EmptyPayload,
    /// A codec payload descriptor ended before all flagged fields.
    TruncatedDescriptor,
    /// H.264 packetization type is not supported by the forwarding core.
    UnsupportedH264NalType(u8),
    /// An H.264 STAP-A unit was empty or crossed the payload boundary.
    InvalidAggregationUnit,
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPayload => formatter.write_str("encoded RTP payload is empty"),
            Self::TruncatedDescriptor => formatter.write_str("codec payload descriptor truncated"),
            Self::UnsupportedH264NalType(nal_type) => {
                write!(formatter, "unsupported H.264 NAL packet type {nal_type}")
            }
            Self::InvalidAggregationUnit => formatter.write_str("invalid H.264 aggregation unit"),
        }
    }
}

impl std::error::Error for PayloadError {}

#[cfg(test)]
mod tests {
    use super::{Codec, PayloadError, inspect_payload};

    #[test]
    fn inspects_vp8_keyframe_and_temporal_layer() {
        let info = inspect_payload(Codec::Vp8, &[0x90, 0x20, 0x80, 0x00], true).expect("valid VP8");
        assert!(info.start_of_frame);
        assert!(info.end_of_frame);
        assert!(info.keyframe);
        assert_eq!(info.temporal_id, Some(2));
    }

    #[test]
    fn inspects_vp9_layer_and_inter_frame() {
        let info =
            inspect_payload(Codec::Vp9, &[0x7c, 0b0110_0100, 0x80, 0], false).expect("valid VP9");
        assert!(info.start_of_frame);
        assert!(info.end_of_frame);
        assert!(!info.keyframe);
        assert_eq!(info.temporal_id, Some(3));
        assert_eq!(info.spatial_id, Some(2));
    }

    #[test]
    fn finds_idr_inside_h264_stap_a() {
        let payload = [24, 0, 2, 0x67, 1, 0, 2, 0x65, 2];
        let info = inspect_payload(Codec::H264, &payload, true).expect("valid STAP-A");
        assert!(info.keyframe);
        assert!(info.end_of_frame);
    }

    #[test]
    fn identifies_h264_fu_a_boundaries() {
        let start = inspect_payload(Codec::H264, &[28, 0x85, 1], false).expect("valid FU-A");
        assert!(start.start_of_frame);
        assert!(start.keyframe);
        let end = inspect_payload(Codec::H264, &[28, 0x45, 2], false).expect("valid FU-A");
        assert!(end.end_of_frame);
    }

    #[test]
    fn rejects_truncated_descriptors() {
        assert_eq!(
            inspect_payload(Codec::Vp8, &[0x80], false),
            Err(PayloadError::TruncatedDescriptor)
        );
        assert_eq!(
            inspect_payload(Codec::H264, &[28], false),
            Err(PayloadError::TruncatedDescriptor)
        );
    }
}
