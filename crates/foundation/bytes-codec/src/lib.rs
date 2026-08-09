//! Checked network-byte-order primitives used by Fluvora protocol parsers.

use core::fmt;

/// An error returned while reading an untrusted byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    offset: usize,
    needed: usize,
    remaining: usize,
}

/// An error returned while building a bounded network message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeError {
    current: usize,
    additional: usize,
    limit: usize,
}

impl EncodeError {
    /// Returns the encoded length before the failed operation.
    #[must_use]
    pub const fn current(self) -> usize {
        self.current
    }

    /// Returns the number of bytes requested by the failed operation.
    #[must_use]
    pub const fn additional(self) -> usize {
        self.additional
    }

    /// Returns the configured message-size limit.
    #[must_use]
    pub const fn limit(self) -> usize {
        self.limit
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "encoded message would exceed limit {}: current {}, additional {}",
            self.limit, self.current, self.additional
        )
    }
}

impl std::error::Error for EncodeError {}

impl DecodeError {
    /// Returns the byte offset at which decoding failed.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Returns the number of bytes the operation required.
    #[must_use]
    pub const fn needed(self) -> usize {
        self.needed
    }

    /// Returns the number of bytes that remained in the input.
    #[must_use]
    pub const fn remaining(self) -> usize {
        self.remaining
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "insufficient input at offset {}: needed {} bytes, remaining {}",
            self.offset, self.needed, self.remaining
        )
    }
}

impl std::error::Error for DecodeError {}

/// A checked cursor over an immutable byte slice.
#[derive(Debug, Clone, Copy)]
pub struct ReadCursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> ReadCursor<'a> {
    /// Creates a cursor positioned at the start of `input`.
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    /// Returns the current byte offset.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.offset
    }

    /// Returns the number of unread bytes.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.input.len() - self.offset
    }

    /// Returns whether no unread bytes remain.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Returns the complete input without changing the cursor.
    #[must_use]
    pub const fn input(&self) -> &'a [u8] {
        self.input
    }

    /// Returns the unread suffix without changing the cursor.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        self.input.get(self.offset..).unwrap_or_default()
    }

    /// Reads one byte.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] without advancing the cursor when no byte remains.
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let offset = self.position();
        let bytes = self.take(1)?;
        bytes.first().copied().ok_or(DecodeError {
            offset,
            needed: 1,
            remaining: 0,
        })
    }

    /// Reads a big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] without advancing the cursor when fewer than two bytes remain.
    pub fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let offset = self.position();
        let bytes = self.take(2)?;
        let bytes: [u8; 2] = bytes.try_into().map_err(|_| DecodeError {
            offset,
            needed: 2,
            remaining: bytes.len(),
        })?;
        Ok(u16::from_be_bytes(bytes))
    }

    /// Reads a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] without advancing the cursor when fewer than four bytes remain.
    pub fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let offset = self.position();
        let bytes = self.take(4)?;
        let bytes: [u8; 4] = bytes.try_into().map_err(|_| DecodeError {
            offset,
            needed: 4,
            remaining: bytes.len(),
        })?;
        Ok(u32::from_be_bytes(bytes))
    }

    /// Reads a big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] without advancing the cursor when fewer than eight bytes remain.
    pub fn read_u64(&mut self) -> Result<u64, DecodeError> {
        let offset = self.position();
        let bytes = self.take(8)?;
        let bytes: [u8; 8] = bytes.try_into().map_err(|_| DecodeError {
            offset,
            needed: 8,
            remaining: bytes.len(),
        })?;
        Ok(u64::from_be_bytes(bytes))
    }

    /// Returns the next `length` bytes and advances the cursor.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] without advancing the cursor when `length` exceeds the number of
    /// remaining bytes.
    pub fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let remaining = self.remaining();
        if length > remaining {
            return Err(DecodeError {
                offset: self.offset,
                needed: length,
                remaining,
            });
        }

        let start = self.offset;
        let end = start + length;
        self.offset = end;
        Ok(&self.input[start..end])
    }
}

/// A size-bounded writer for network-byte-order messages.
#[derive(Debug, Clone)]
pub struct WriteBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl WriteBuffer {
    /// Creates an empty writer with a hard byte limit.
    #[must_use]
    pub const fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    /// Returns the current encoded length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the writer is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the configured hard limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the writer and returns the encoded bytes.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    /// Appends one byte.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if the limit would be exceeded.
    pub fn write_u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.extend_from_slice(&[value])
    }

    /// Appends a big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if the limit would be exceeded.
    pub fn write_u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.extend_from_slice(&value.to_be_bytes())
    }

    /// Appends a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if the limit would be exceeded.
    pub fn write_u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.extend_from_slice(&value.to_be_bytes())
    }

    /// Appends a big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if the limit would be exceeded.
    pub fn write_u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.extend_from_slice(&value.to_be_bytes())
    }

    /// Appends bytes.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if the limit would be exceeded.
    pub fn extend_from_slice(&mut self, value: &[u8]) -> Result<(), EncodeError> {
        let Some(new_len) = self.bytes.len().checked_add(value.len()) else {
            return Err(self.error(value.len()));
        };
        if new_len > self.limit {
            return Err(self.error(value.len()));
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    /// Replaces an existing big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if `offset..offset + 2` is outside the
    /// encoded bytes.
    pub fn set_u16(&mut self, offset: usize, value: u16) -> Result<(), EncodeError> {
        self.set_slice(offset, &value.to_be_bytes())
    }

    /// Replaces an existing big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] without modifying the writer if `offset..offset + 4` is outside the
    /// encoded bytes.
    pub fn set_u32(&mut self, offset: usize, value: u32) -> Result<(), EncodeError> {
        self.set_slice(offset, &value.to_be_bytes())
    }

    fn set_slice(&mut self, offset: usize, value: &[u8]) -> Result<(), EncodeError> {
        let Some(end) = offset.checked_add(value.len()) else {
            return Err(self.error(value.len()));
        };
        let Some(target) = self.bytes.get_mut(offset..end) else {
            return Err(self.error(value.len()));
        };
        target.copy_from_slice(value);
        Ok(())
    }

    const fn error(&self, additional: usize) -> EncodeError {
        EncodeError {
            current: self.bytes.len(),
            additional,
            limit: self.limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeError, EncodeError, ReadCursor, WriteBuffer};

    #[test]
    fn reads_network_byte_order_values() {
        let mut cursor = ReadCursor::new(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde]);

        assert_eq!(cursor.read_u8(), Ok(0x12));
        assert_eq!(cursor.read_u16(), Ok(0x3456));
        assert_eq!(cursor.read_u32(), Ok(0x789a_bcde));
        assert!(cursor.is_empty());
    }

    #[test]
    fn failed_read_does_not_advance_cursor() {
        let mut cursor = ReadCursor::new(&[1, 2, 3]);

        assert_eq!(
            cursor.read_u32(),
            Err(DecodeError {
                offset: 0,
                needed: 4,
                remaining: 3,
            })
        );
        assert_eq!(cursor.position(), 0);
        assert_eq!(cursor.take(3), Ok([1, 2, 3].as_slice()));
    }

    #[test]
    fn taking_zero_bytes_is_valid() {
        let mut cursor = ReadCursor::new(&[]);

        assert_eq!(cursor.take(0), Ok([].as_slice()));
        assert!(cursor.is_empty());
    }

    #[test]
    fn reads_u64_and_exposes_rest() {
        let mut cursor = ReadCursor::new(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

        assert_eq!(cursor.read_u64(), Ok(0x0001_0203_0405_0607));
        assert_eq!(cursor.rest(), &[8, 9]);
        assert_eq!(cursor.input().len(), 10);
    }

    #[test]
    fn writer_encodes_and_patches_network_values() {
        let mut writer = WriteBuffer::with_limit(12);

        assert_eq!(writer.write_u16(0), Ok(()));
        assert_eq!(writer.write_u32(0x1122_3344), Ok(()));
        assert_eq!(
            writer.write_u64(0x0102_0304_0506_0708),
            Err(EncodeError {
                current: 6,
                additional: 8,
                limit: 12,
            })
        );
        assert_eq!(writer.set_u16(0, 0xabcd), Ok(()));
        assert_eq!(writer.as_slice(), &[0xab, 0xcd, 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn failed_patch_does_not_modify_writer() {
        let mut writer = WriteBuffer::with_limit(4);
        writer.write_u32(7).expect("fits configured test limit");

        assert!(writer.set_u16(4, 3).is_err());
        assert_eq!(writer.as_slice(), &[0, 0, 0, 7]);
    }
}
