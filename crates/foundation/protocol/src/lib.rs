//! Versioned realtime data envelope shared by WebSocket, WebTransport, and data-channel SDKs.

use std::fmt;

use fluvora_bytes_codec::{DecodeError, EncodeError, ReadCursor, WriteBuffer};

const MAGIC: &[u8; 4] = b"FLUV";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 60;
const MAX_WIRE_LEN: usize = HEADER_LEN + u16::MAX as usize * 16;

/// Current signaling and realtime-data protocol version.
pub const SIGNALING_VERSION: u8 = VERSION;
/// Fixed byte length of an Envelope v1 header.
pub const ENVELOPE_HEADER_BYTES: usize = HEADER_LEN;

/// Realtime application message category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKind {
    /// Room presence or role update.
    Presence,
    /// Chat content.
    Chat,
    /// Server-verified gift event.
    Gift,
    /// Signaling/control data.
    Control,
    /// Application extension in the reserved `0x8000..=0xffff` range.
    Custom(u16),
    /// A future Fluvora core type retained for forward compatibility.
    Unknown(u16),
}

impl DataKind {
    const fn to_wire(self) -> u16 {
        match self {
            Self::Presence => 1,
            Self::Chat => 2,
            Self::Gift => 3,
            Self::Control => 4,
            Self::Custom(value) | Self::Unknown(value) => value,
        }
    }

    const fn from_wire(value: u16) -> Self {
        match value {
            1 => Self::Presence,
            2 => Self::Chat,
            3 => Self::Gift,
            4 => Self::Control,
            0x8000..=u16::MAX => Self::Custom(value),
            _ => Self::Unknown(value),
        }
    }
}

/// Envelope delivery hints; transports may provide stronger guarantees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct EnvelopeFlags(u8);

impl EnvelopeFlags {
    const RELIABLE: u8 = 0x01;
    const ORDERED: u8 = 0x02;
    const ACK_REQUIRED: u8 = 0x04;
    const ALLOWED: u8 = Self::RELIABLE | Self::ORDERED | Self::ACK_REQUIRED;

    /// Creates flags from delivery hints.
    #[must_use]
    pub fn new(reliable: bool, ordered: bool, acknowledgement_required: bool) -> Self {
        Self(
            (u8::from(reliable) * Self::RELIABLE)
                | (u8::from(ordered) * Self::ORDERED)
                | (u8::from(acknowledgement_required) * Self::ACK_REQUIRED),
        )
    }

    /// Returns whether lossless delivery is requested.
    #[must_use]
    pub const fn reliable(self) -> bool {
        self.0 & Self::RELIABLE != 0
    }

    /// Returns whether per-sender ordering is requested.
    #[must_use]
    pub const fn ordered(self) -> bool {
        self.0 & Self::ORDERED != 0
    }

    /// Returns whether the application expects an acknowledgement.
    #[must_use]
    pub const fn acknowledgement_required(self) -> bool {
        self.0 & Self::ACK_REQUIRED != 0
    }
}

/// Transport-neutral realtime data frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Delivery hints.
    pub flags: EnvelopeFlags,
    /// Core or application message category.
    pub kind: DataKind,
    /// 128-bit room identifier.
    pub room_id: u128,
    /// Authenticated sender identifier.
    pub sender_id: u128,
    /// Monotonic room event sequence.
    pub sequence: u64,
    /// Unix timestamp in milliseconds assigned by the server.
    pub timestamp_millis: u64,
    /// Kind-specific bytes.
    pub payload: Vec<u8>,
}

impl Envelope {
    /// Encodes a bounded version-1 frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when payload length exceeds the caller's limit or wire capacity.
    pub fn encode(&self, maximum_payload: usize) -> Result<Vec<u8>, ProtocolError> {
        if self.payload.len() > maximum_payload {
            return Err(ProtocolError::PayloadTooLarge {
                actual: self.payload.len(),
                maximum: maximum_payload,
            });
        }
        let payload_len =
            u32::try_from(self.payload.len()).map_err(|_| ProtocolError::PayloadTooLarge {
                actual: self.payload.len(),
                maximum: u32::MAX as usize,
            })?;
        let mut output = WriteBuffer::with_limit(MAX_WIRE_LEN);
        output.extend_from_slice(MAGIC)?;
        output.write_u8(VERSION)?;
        output.write_u8(self.flags.0)?;
        output.write_u16(self.kind.to_wire())?;
        write_u128(&mut output, self.room_id)?;
        write_u128(&mut output, self.sender_id)?;
        output.write_u64(self.sequence)?;
        output.write_u64(self.timestamp_millis)?;
        output.write_u32(payload_len)?;
        output.extend_from_slice(&self.payload)?;
        Ok(output.into_vec())
    }

    /// Decodes one exact bounded frame.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] for magic/version/flag errors, truncation, trailing bytes, or
    /// payload limits.
    pub fn decode(input: &[u8], maximum_payload: usize) -> Result<Self, ProtocolError> {
        if input.len() < HEADER_LEN {
            return Err(ProtocolError::FrameTooShort(input.len()));
        }
        let mut cursor = ReadCursor::new(input);
        if cursor.take(4)? != MAGIC {
            return Err(ProtocolError::InvalidMagic);
        }
        let version = cursor.read_u8()?;
        if version != VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let flags = cursor.read_u8()?;
        if flags & !EnvelopeFlags::ALLOWED != 0 {
            return Err(ProtocolError::UnknownFlags(flags));
        }
        let kind = DataKind::from_wire(cursor.read_u16()?);
        let room_id = read_u128(&mut cursor)?;
        let sender_id = read_u128(&mut cursor)?;
        let sequence = cursor.read_u64()?;
        let timestamp_millis = cursor.read_u64()?;
        let payload_len =
            usize::try_from(cursor.read_u32()?).map_err(|_| ProtocolError::FrameTooLarge)?;
        if payload_len > maximum_payload {
            return Err(ProtocolError::PayloadTooLarge {
                actual: payload_len,
                maximum: maximum_payload,
            });
        }
        let payload = cursor.take(payload_len)?.to_vec();
        if !cursor.is_empty() {
            return Err(ProtocolError::TrailingBytes(cursor.remaining()));
        }
        Ok(Self {
            flags: EnvelopeFlags(flags),
            kind,
            room_id,
            sender_id,
            sequence,
            timestamp_millis,
            payload,
        })
    }
}

fn write_u128(output: &mut WriteBuffer, value: u128) -> Result<(), EncodeError> {
    output.write_u64(u64::try_from(value >> 64).unwrap_or_default())?;
    output.write_u64(u64::try_from(value & u128::from(u64::MAX)).unwrap_or_default())?;
    Ok(())
}

fn read_u128(cursor: &mut ReadCursor<'_>) -> Result<u128, DecodeError> {
    let high = u128::from(cursor.read_u64()?);
    let low = u128::from(cursor.read_u64()?);
    Ok((high << 64) | low)
}

/// Realtime envelope codec failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// Input is shorter than the fixed header.
    FrameTooShort(usize),
    /// Magic bytes do not identify Fluvora data.
    InvalidMagic,
    /// Version is not implemented.
    UnsupportedVersion(u8),
    /// Reserved flag bits were set.
    UnknownFlags(u8),
    /// Payload exceeds its configured or wire limit.
    PayloadTooLarge {
        /// Declared or supplied payload bytes.
        actual: usize,
        /// Active upper bound.
        maximum: usize,
    },
    /// Frame size arithmetic exceeded the platform representation.
    FrameTooLarge,
    /// Exact frame had bytes after its payload.
    TrailingBytes(usize),
    /// Checked byte reader failed.
    Decode(DecodeError),
    /// Bounded byte writer failed.
    Encode(EncodeError),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooShort(length) => write!(formatter, "data frame too short: {length}"),
            Self::InvalidMagic => formatter.write_str("invalid Fluvora data magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported data protocol version {version}")
            }
            Self::UnknownFlags(flags) => write!(formatter, "unknown data flags {flags:#04x}"),
            Self::PayloadTooLarge { actual, maximum } => {
                write!(formatter, "payload {actual} exceeds limit {maximum}")
            }
            Self::FrameTooLarge => formatter.write_str("data frame is too large"),
            Self::TrailingBytes(length) => write!(formatter, "{length} trailing frame bytes"),
            Self::Decode(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<DecodeError> for ProtocolError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<EncodeError> for ProtocolError {
    fn from(value: EncodeError) -> Self {
        Self::Encode(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{DataKind, ENVELOPE_HEADER_BYTES, Envelope, EnvelopeFlags, ProtocolError};

    #[test]
    fn round_trips_versioned_envelope() {
        let envelope = Envelope {
            flags: EnvelopeFlags::new(true, true, true),
            kind: DataKind::Gift,
            room_id: u128::MAX - 1,
            sender_id: 42,
            sequence: 99,
            timestamp_millis: 1_700_000_000_000,
            payload: b"verified-receipt".to_vec(),
        };
        let bytes = envelope.encode(1_024).expect("valid envelope");
        assert_eq!(bytes.len(), ENVELOPE_HEADER_BYTES + envelope.payload.len());
        assert_eq!(
            Envelope::decode(&bytes, 1_024).expect("encoded envelope must decode"),
            envelope
        );
    }

    #[test]
    fn retains_custom_and_future_message_types() {
        for kind in [DataKind::Custom(0x9001), DataKind::Unknown(100)] {
            let envelope = Envelope {
                flags: EnvelopeFlags::default(),
                kind,
                room_id: 1,
                sender_id: 2,
                sequence: 3,
                timestamp_millis: 4,
                payload: Vec::new(),
            };
            let encoded = envelope.encode(0).expect("valid empty envelope");
            assert_eq!(
                Envelope::decode(&encoded, 0).expect("valid envelope").kind,
                kind
            );
        }
    }

    #[test]
    fn rejects_limits_and_trailing_bytes() {
        let envelope = Envelope {
            flags: EnvelopeFlags::default(),
            kind: DataKind::Chat,
            room_id: 1,
            sender_id: 2,
            sequence: 3,
            timestamp_millis: 4,
            payload: vec![1, 2, 3],
        };
        assert_eq!(
            envelope.encode(2),
            Err(ProtocolError::PayloadTooLarge {
                actual: 3,
                maximum: 2
            })
        );
        let mut encoded = envelope.encode(3).expect("valid frame");
        encoded.push(0);
        assert_eq!(
            Envelope::decode(&encoded, 3),
            Err(ProtocolError::TrailingBytes(1))
        );
    }
}
