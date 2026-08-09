use crate::twcc;
use crate::{
    GenericNack, NackEntry, Packet, PictureLossIndication, RawPacket, ReceiverReport, ReportBlock,
    RtcpError, SdesChunk, SdesItem, SenderReport, SourceDescription,
};

const HEADER_LEN: usize = 4;
const REPORT_BLOCK_LEN: usize = 24;
const MAX_PACKET_LEN: usize = 262_144;

/// Parses every packet in an RTCP compound datagram.
///
/// # Errors
///
/// Returns [`RtcpError`] when any common header, padding field, or known packet body is malformed.
pub fn parse_compound(input: &[u8]) -> Result<Vec<Packet>, RtcpError> {
    let mut packets = Vec::new();
    let mut position = 0;
    while position < input.len() {
        let remaining = input.get(position..).ok_or(RtcpError::PacketTooShort(0))?;
        if remaining.len() < HEADER_LEN {
            return Err(RtcpError::PacketTooShort(remaining.len()));
        }
        let first = remaining[0];
        let version = first >> 6;
        if version != 2 {
            return Err(RtcpError::UnsupportedVersion(version));
        }
        let has_padding = first & 0x20 != 0;
        let count = first & 0x1f;
        let packet_type = remaining[1];
        let words_minus_one = usize::from(u16::from_be_bytes([remaining[2], remaining[3]]));
        let packet_len = words_minus_one
            .checked_add(1)
            .and_then(|words| words.checked_mul(4))
            .ok_or(RtcpError::PacketTooLarge)?;
        let packet_bytes = remaining
            .get(..packet_len)
            .ok_or(RtcpError::InvalidPacketLength {
                declared: packet_len,
                remaining: remaining.len(),
            })?;
        let body_with_padding = packet_bytes
            .get(HEADER_LEN..)
            .ok_or(RtcpError::TruncatedPacket)?;
        let body = strip_padding(has_padding, body_with_padding)?;
        packets.push(parse_packet(count, packet_type, body)?);
        position += packet_len;
    }
    if packets.is_empty() {
        return Err(RtcpError::PacketTooShort(0));
    }
    Ok(packets)
}

/// Encodes packets into one RTCP compound datagram.
///
/// # Errors
///
/// Returns [`RtcpError`] if a packet field exceeds its wire representation.
pub fn encode_compound(packets: &[Packet]) -> Result<Vec<u8>, RtcpError> {
    let mut output = Vec::new();
    for packet in packets {
        let (count, packet_type, mut body) = encode_packet(packet)?;
        while body.len() % 4 != 0 {
            body.push(0);
        }
        let complete_len = body
            .len()
            .checked_add(HEADER_LEN)
            .ok_or(RtcpError::PacketTooLarge)?;
        let words_minus_one = complete_len / 4 - 1;
        let wire_length = u16::try_from(words_minus_one).map_err(|_| RtcpError::PacketTooLarge)?;
        if count > 31 {
            return Err(RtcpError::CountTooLarge(usize::from(count)));
        }
        output.extend_from_slice(&[0x80 | count, packet_type]);
        output.extend_from_slice(&wire_length.to_be_bytes());
        output.extend_from_slice(&body);
        if output.len() > MAX_PACKET_LEN {
            return Err(RtcpError::PacketTooLarge);
        }
    }
    Ok(output)
}

fn parse_packet(count: u8, packet_type: u8, body: &[u8]) -> Result<Packet, RtcpError> {
    match (packet_type, count) {
        (200, report_count) => parse_sender_report(report_count, body).map(Packet::SenderReport),
        (201, report_count) => {
            parse_receiver_report(report_count, body).map(Packet::ReceiverReport)
        }
        (202, chunk_count) => {
            parse_source_description(chunk_count, body).map(Packet::SourceDescription)
        }
        (205, 1) => parse_nack(body).map(Packet::GenericNack),
        (205, 15) => twcc::parse(body).map(Packet::TransportWideFeedback),
        (206, 1) => parse_pli(body).map(Packet::PictureLossIndication),
        _ => Ok(Packet::Raw(RawPacket {
            count,
            packet_type,
            body: body.to_vec(),
        })),
    }
}

fn parse_sender_report(count: u8, body: &[u8]) -> Result<SenderReport, RtcpError> {
    let expected = 24 + usize::from(count) * REPORT_BLOCK_LEN;
    require_body_len(200, body, expected)?;
    Ok(SenderReport {
        sender_ssrc: read_u32(body, 0)?,
        ntp_timestamp: read_u64(body, 4)?,
        rtp_timestamp: read_u32(body, 12)?,
        sender_packet_count: read_u32(body, 16)?,
        sender_octet_count: read_u32(body, 20)?,
        reports: parse_report_blocks(body, 24, count)?,
    })
}

fn parse_receiver_report(count: u8, body: &[u8]) -> Result<ReceiverReport, RtcpError> {
    let expected = 4 + usize::from(count) * REPORT_BLOCK_LEN;
    require_body_len(201, body, expected)?;
    Ok(ReceiverReport {
        sender_ssrc: read_u32(body, 0)?,
        reports: parse_report_blocks(body, 4, count)?,
    })
}

fn parse_report_blocks(
    body: &[u8],
    mut position: usize,
    count: u8,
) -> Result<Vec<ReportBlock>, RtcpError> {
    let mut reports = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        reports.push(ReportBlock {
            ssrc: read_u32(body, position)?,
            fraction_lost: *body.get(position + 4).ok_or(RtcpError::TruncatedPacket)?,
            cumulative_lost: read_i24(body, position + 5)?,
            extended_highest_sequence: read_u32(body, position + 8)?,
            jitter: read_u32(body, position + 12)?,
            last_sender_report: read_u32(body, position + 16)?,
            delay_since_last_sender_report: read_u32(body, position + 20)?,
        });
        position += REPORT_BLOCK_LEN;
    }
    Ok(reports)
}

fn parse_source_description(count: u8, body: &[u8]) -> Result<SourceDescription, RtcpError> {
    let mut chunks = Vec::with_capacity(usize::from(count));
    let mut position = 0;
    for _ in 0..count {
        let chunk_start = position;
        let ssrc = read_u32(body, position)?;
        position += 4;
        let mut items = Vec::new();
        loop {
            let item_type = *body.get(position).ok_or(RtcpError::TruncatedPacket)?;
            position += 1;
            if item_type == 0 {
                break;
            }
            let length = usize::from(*body.get(position).ok_or(RtcpError::TruncatedPacket)?);
            position += 1;
            let end = position
                .checked_add(length)
                .ok_or(RtcpError::TruncatedPacket)?;
            let value = body
                .get(position..end)
                .ok_or(RtcpError::TruncatedPacket)?
                .to_vec();
            position = end;
            items.push(SdesItem { item_type, value });
        }
        while (position - chunk_start) % 4 != 0 {
            if *body.get(position).ok_or(RtcpError::TruncatedPacket)? != 0 {
                return Err(RtcpError::InvalidPadding(0));
            }
            position += 1;
        }
        chunks.push(SdesChunk { ssrc, items });
    }
    if position != body.len() {
        return Err(RtcpError::InvalidBodyLength {
            packet_type: 202,
            length: body.len(),
        });
    }
    Ok(SourceDescription { chunks })
}

fn parse_nack(body: &[u8]) -> Result<GenericNack, RtcpError> {
    if body.len() < 8 || !(body.len() - 8).is_multiple_of(4) {
        return Err(RtcpError::InvalidBodyLength {
            packet_type: 205,
            length: body.len(),
        });
    }
    let mut entries = Vec::with_capacity((body.len() - 8) / 4);
    let mut position = 8;
    while position < body.len() {
        entries.push(NackEntry {
            packet_id: read_u16(body, position)?,
            lost_packet_bitmask: read_u16(body, position + 2)?,
        });
        position += 4;
    }
    Ok(GenericNack {
        sender_ssrc: read_u32(body, 0)?,
        media_ssrc: read_u32(body, 4)?,
        entries,
    })
}

fn parse_pli(body: &[u8]) -> Result<PictureLossIndication, RtcpError> {
    require_body_len(206, body, 8)?;
    Ok(PictureLossIndication {
        sender_ssrc: read_u32(body, 0)?,
        media_ssrc: read_u32(body, 4)?,
    })
}

fn encode_packet(packet: &Packet) -> Result<(u8, u8, Vec<u8>), RtcpError> {
    match packet {
        Packet::SenderReport(report) => encode_sender_report(report),
        Packet::ReceiverReport(report) => encode_receiver_report(report),
        Packet::SourceDescription(description) => encode_sdes(description),
        Packet::GenericNack(nack) => Ok((1, 205, encode_nack(nack))),
        Packet::PictureLossIndication(pli) => Ok((1, 206, encode_pli(*pli))),
        Packet::TransportWideFeedback(feedback) => Ok((15, 205, twcc::encode(feedback)?)),
        Packet::Raw(raw) => Ok((raw.count, raw.packet_type, raw.body.clone())),
    }
}

fn encode_sender_report(report: &SenderReport) -> Result<(u8, u8, Vec<u8>), RtcpError> {
    let count = checked_count(report.reports.len())?;
    let mut body = Vec::new();
    body.extend_from_slice(&report.sender_ssrc.to_be_bytes());
    body.extend_from_slice(&report.ntp_timestamp.to_be_bytes());
    body.extend_from_slice(&report.rtp_timestamp.to_be_bytes());
    body.extend_from_slice(&report.sender_packet_count.to_be_bytes());
    body.extend_from_slice(&report.sender_octet_count.to_be_bytes());
    encode_report_blocks(&mut body, &report.reports)?;
    Ok((count, 200, body))
}

fn encode_receiver_report(report: &ReceiverReport) -> Result<(u8, u8, Vec<u8>), RtcpError> {
    let count = checked_count(report.reports.len())?;
    let mut body = Vec::new();
    body.extend_from_slice(&report.sender_ssrc.to_be_bytes());
    encode_report_blocks(&mut body, &report.reports)?;
    Ok((count, 201, body))
}

fn encode_report_blocks(output: &mut Vec<u8>, reports: &[ReportBlock]) -> Result<(), RtcpError> {
    for report in reports {
        if !(-8_388_608..=8_388_607).contains(&report.cumulative_lost) {
            return Err(RtcpError::InvalidCumulativeLoss(report.cumulative_lost));
        }
        output.extend_from_slice(&report.ssrc.to_be_bytes());
        output.push(report.fraction_lost);
        output.extend_from_slice(&report.cumulative_lost.to_be_bytes()[1..]);
        output.extend_from_slice(&report.extended_highest_sequence.to_be_bytes());
        output.extend_from_slice(&report.jitter.to_be_bytes());
        output.extend_from_slice(&report.last_sender_report.to_be_bytes());
        output.extend_from_slice(&report.delay_since_last_sender_report.to_be_bytes());
    }
    Ok(())
}

fn encode_sdes(description: &SourceDescription) -> Result<(u8, u8, Vec<u8>), RtcpError> {
    let count = checked_count(description.chunks.len())?;
    let mut body = Vec::new();
    for chunk in &description.chunks {
        let chunk_start = body.len();
        body.extend_from_slice(&chunk.ssrc.to_be_bytes());
        for item in &chunk.items {
            let length = u8::try_from(item.value.len())
                .map_err(|_| RtcpError::SdesItemTooLarge(item.value.len()))?;
            body.extend_from_slice(&[item.item_type, length]);
            body.extend_from_slice(&item.value);
        }
        body.push(0);
        while (body.len() - chunk_start) % 4 != 0 {
            body.push(0);
        }
    }
    Ok((count, 202, body))
}

fn encode_nack(nack: &GenericNack) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&nack.sender_ssrc.to_be_bytes());
    body.extend_from_slice(&nack.media_ssrc.to_be_bytes());
    for entry in &nack.entries {
        body.extend_from_slice(&entry.packet_id.to_be_bytes());
        body.extend_from_slice(&entry.lost_packet_bitmask.to_be_bytes());
    }
    body
}

fn encode_pli(pli: PictureLossIndication) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&pli.sender_ssrc.to_be_bytes());
    body.extend_from_slice(&pli.media_ssrc.to_be_bytes());
    body
}

fn strip_padding(has_padding: bool, body: &[u8]) -> Result<&[u8], RtcpError> {
    if !has_padding {
        return Ok(body);
    }
    let padding_len = usize::from(*body.last().ok_or(RtcpError::InvalidPadding(0))?);
    if padding_len == 0 || padding_len > body.len() {
        return Err(RtcpError::InvalidPadding(padding_len));
    }
    let content_len = body.len() - padding_len;
    let padding = body
        .get(content_len..body.len() - 1)
        .ok_or(RtcpError::InvalidPadding(padding_len))?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(RtcpError::InvalidPadding(padding_len));
    }
    body.get(..content_len)
        .ok_or(RtcpError::InvalidPadding(padding_len))
}

fn checked_count(count: usize) -> Result<u8, RtcpError> {
    if count > 31 {
        Err(RtcpError::CountTooLarge(count))
    } else {
        u8::try_from(count).map_err(|_| RtcpError::CountTooLarge(count))
    }
}

fn require_body_len(packet_type: u8, body: &[u8], expected: usize) -> Result<(), RtcpError> {
    if body.len() == expected {
        Ok(())
    } else {
        Err(RtcpError::InvalidBodyLength {
            packet_type,
            length: body.len(),
        })
    }
}

fn read_u16(input: &[u8], position: usize) -> Result<u16, RtcpError> {
    let bytes: [u8; 2] = read_array(input, position)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(input: &[u8], position: usize) -> Result<u32, RtcpError> {
    let bytes: [u8; 4] = read_array(input, position)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(input: &[u8], position: usize) -> Result<u64, RtcpError> {
    let bytes: [u8; 8] = read_array(input, position)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_i24(input: &[u8], position: usize) -> Result<i32, RtcpError> {
    let bytes: [u8; 3] = read_array(input, position)?;
    let sign = if bytes[0] & 0x80 == 0 { 0 } else { 0xff };
    Ok(i32::from_be_bytes([sign, bytes[0], bytes[1], bytes[2]]))
}

fn read_array<const LENGTH: usize>(
    input: &[u8],
    position: usize,
) -> Result<[u8; LENGTH], RtcpError> {
    input
        .get(position..position.saturating_add(LENGTH))
        .ok_or(RtcpError::TruncatedPacket)?
        .try_into()
        .map_err(|_| RtcpError::TruncatedPacket)
}
