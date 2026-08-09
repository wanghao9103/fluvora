use crate::{RtcpError, TransportWideFeedback, TwccStatus};

const FIXED_BODY_LEN: usize = 16;

pub(crate) fn parse(body: &[u8]) -> Result<TransportWideFeedback, RtcpError> {
    if body.len() < FIXED_BODY_LEN {
        return Err(RtcpError::InvalidBodyLength {
            packet_type: 205,
            length: body.len(),
        });
    }
    let sender_ssrc = read_u32(body, 0)?;
    let media_ssrc = read_u32(body, 4)?;
    let base_sequence_number = read_u16(body, 8)?;
    let status_count = usize::from(read_u16(body, 10)?);
    let reference_time = read_i24(body, 12)?;
    let feedback_packet_count = body[15];
    let mut position = FIXED_BODY_LEN;
    let mut symbols = Vec::with_capacity(status_count);
    while symbols.len() < status_count {
        let chunk = read_u16(body, position)?;
        position += 2;
        decode_chunk(chunk, status_count, &mut symbols)?;
    }

    let mut statuses = Vec::with_capacity(status_count);
    for symbol in symbols {
        match symbol {
            0 => statuses.push(TwccStatus::NotReceived),
            1 => {
                let delta = *body.get(position).ok_or(RtcpError::InvalidTwccDeltas)?;
                position += 1;
                statuses.push(TwccStatus::ReceivedSmallDelta(delta));
            }
            2 => {
                let delta = read_i16(body, position)?;
                position += 2;
                statuses.push(TwccStatus::ReceivedLargeDelta(delta));
            }
            reserved => return Err(RtcpError::ReservedTwccStatus(reserved)),
        }
    }
    if body
        .get(position..)
        .ok_or(RtcpError::InvalidTwccDeltas)?
        .iter()
        .any(|byte| *byte != 0)
        || body.len().saturating_sub(position) > 3
    {
        return Err(RtcpError::InvalidTwccDeltas);
    }
    Ok(TransportWideFeedback {
        sender_ssrc,
        media_ssrc,
        base_sequence_number,
        reference_time,
        feedback_packet_count,
        statuses,
    })
}

pub(crate) fn encode(feedback: &TransportWideFeedback) -> Result<Vec<u8>, RtcpError> {
    if feedback.statuses.len() > usize::from(u16::MAX) {
        return Err(RtcpError::InvalidTwccStatusCount);
    }
    if !(-8_388_608..=8_388_607).contains(&feedback.reference_time) {
        return Err(RtcpError::InvalidReferenceTime(feedback.reference_time));
    }
    let mut output = Vec::new();
    output.extend_from_slice(&feedback.sender_ssrc.to_be_bytes());
    output.extend_from_slice(&feedback.media_ssrc.to_be_bytes());
    output.extend_from_slice(&feedback.base_sequence_number.to_be_bytes());
    output.extend_from_slice(
        &u16::try_from(feedback.statuses.len())
            .map_err(|_| RtcpError::InvalidTwccStatusCount)?
            .to_be_bytes(),
    );
    write_i24(&mut output, feedback.reference_time);
    output.push(feedback.feedback_packet_count);

    for statuses in feedback.statuses.chunks(7) {
        let mut chunk = 0xc000_u16;
        for (index, status) in statuses.iter().enumerate() {
            let symbol: u16 = match status {
                TwccStatus::NotReceived => 0,
                TwccStatus::ReceivedSmallDelta(_) => 1,
                TwccStatus::ReceivedLargeDelta(_) => 2,
            };
            let shift = 12_u32.saturating_sub(u32::try_from(index * 2).unwrap_or_default());
            chunk |= symbol << shift;
        }
        output.extend_from_slice(&chunk.to_be_bytes());
    }
    for status in &feedback.statuses {
        match status {
            TwccStatus::NotReceived => {}
            TwccStatus::ReceivedSmallDelta(delta) => output.push(*delta),
            TwccStatus::ReceivedLargeDelta(delta) => {
                output.extend_from_slice(&delta.to_be_bytes());
            }
        }
    }
    while output.len() % 4 != 0 {
        output.push(0);
    }
    Ok(output)
}

fn decode_chunk(chunk: u16, status_count: usize, symbols: &mut Vec<u8>) -> Result<(), RtcpError> {
    if chunk & 0x8000 == 0 {
        let symbol = u8::try_from((chunk >> 13) & 0x03).unwrap_or_default();
        if symbol == 3 {
            return Err(RtcpError::ReservedTwccStatus(symbol));
        }
        let run_length = usize::from(chunk & 0x1fff);
        if run_length == 0 {
            return Err(RtcpError::InvalidTwccStatusCount);
        }
        let remaining = status_count.saturating_sub(symbols.len());
        symbols.extend(std::iter::repeat_n(symbol, run_length.min(remaining)));
        return Ok(());
    }

    let two_bit = chunk & 0x4000 != 0;
    let symbols_per_chunk = if two_bit { 7 } else { 14 };
    for index in 0..symbols_per_chunk {
        if symbols.len() == status_count {
            break;
        }
        let symbol = if two_bit {
            let shift = 12 - index * 2;
            u8::try_from((chunk >> shift) & 0x03).unwrap_or_default()
        } else {
            let shift = 13 - index;
            u8::try_from((chunk >> shift) & 0x01).unwrap_or_default()
        };
        if symbol == 3 {
            return Err(RtcpError::ReservedTwccStatus(symbol));
        }
        symbols.push(symbol);
    }
    Ok(())
}

fn read_u16(input: &[u8], position: usize) -> Result<u16, RtcpError> {
    let bytes: [u8; 2] = input
        .get(position..position.saturating_add(2))
        .ok_or(RtcpError::TruncatedPacket)?
        .try_into()
        .map_err(|_| RtcpError::TruncatedPacket)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_i16(input: &[u8], position: usize) -> Result<i16, RtcpError> {
    let bytes: [u8; 2] = input
        .get(position..position.saturating_add(2))
        .ok_or(RtcpError::InvalidTwccDeltas)?
        .try_into()
        .map_err(|_| RtcpError::InvalidTwccDeltas)?;
    Ok(i16::from_be_bytes(bytes))
}

fn read_u32(input: &[u8], position: usize) -> Result<u32, RtcpError> {
    let bytes: [u8; 4] = input
        .get(position..position.saturating_add(4))
        .ok_or(RtcpError::TruncatedPacket)?
        .try_into()
        .map_err(|_| RtcpError::TruncatedPacket)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_i24(input: &[u8], position: usize) -> Result<i32, RtcpError> {
    let bytes = input
        .get(position..position.saturating_add(3))
        .ok_or(RtcpError::TruncatedPacket)?;
    let sign = if bytes[0] & 0x80 == 0 { 0 } else { 0xff };
    Ok(i32::from_be_bytes([sign, bytes[0], bytes[1], bytes[2]]))
}

fn write_i24(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_be_bytes()[1..]);
}
