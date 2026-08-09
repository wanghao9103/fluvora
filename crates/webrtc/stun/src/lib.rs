//! A strict, Sans-I/O STUN codec for Fluvora.
//!
//! The implementation follows RFC 8489 and includes the ICE attributes used by RFC 8445.

use core::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crc32fast::hash as crc32;
use fluvora_bytes_codec::{DecodeError, EncodeError, ReadCursor, WriteBuffer};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use subtle::ConstantTimeEq;

const HEADER_LEN: usize = 20;
const MAGIC_COOKIE: u32 = 0x2112_a442;
const FINGERPRINT_XOR: u32 = 0x5354_554e;
const MAX_MESSAGE_LEN: usize = HEADER_LEN + 65_535;

type HmacSha1 = Hmac<Sha1>;
type HmacSha256 = Hmac<Sha256>;

/// A STUN method number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Method(u16);

impl Method {
    /// The STUN Binding method.
    pub const BINDING: Self = Self(0x001);
    /// TURN Allocate.
    pub const ALLOCATE: Self = Self(0x003);
    /// TURN Refresh.
    pub const REFRESH: Self = Self(0x004);
    /// TURN Send indication.
    pub const SEND: Self = Self(0x006);
    /// TURN Data indication.
    pub const DATA: Self = Self(0x007);
    /// TURN `CreatePermission`.
    pub const CREATE_PERMISSION: Self = Self(0x008);
    /// TURN `ChannelBind`.
    pub const CHANNEL_BIND: Self = Self(0x009);

    /// Creates a 12-bit STUN method.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidMethod`] when `value` does not fit in 12 bits.
    pub const fn new(value: u16) -> Result<Self, StunError> {
        if value <= 0x0fff {
            Ok(Self(value))
        } else {
            Err(StunError::InvalidMethod(value))
        }
    }

    /// Returns the numeric 12-bit method.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// A STUN message class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageClass {
    /// A request that expects a success or error response.
    Request = 0,
    /// A one-way indication.
    Indication = 1,
    /// A successful response.
    SuccessResponse = 2,
    /// An error response.
    ErrorResponse = 3,
}

impl MessageClass {
    const fn from_raw(value: u16) -> Self {
        match value {
            0 => Self::Request,
            1 => Self::Indication,
            2 => Self::SuccessResponse,
            _ => Self::ErrorResponse,
        }
    }
}

/// A decoded STUN method and class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageType {
    method: Method,
    class: MessageClass,
}

impl MessageType {
    /// Creates a STUN message type.
    #[must_use]
    pub const fn new(method: Method, class: MessageClass) -> Self {
        Self { method, class }
    }

    /// Returns the message method.
    #[must_use]
    pub const fn method(self) -> Method {
        self.method
    }

    /// Returns the message class.
    #[must_use]
    pub const fn class(self) -> MessageClass {
        self.class
    }

    /// Encodes the RFC 8489 interleaved method/class bit field.
    #[must_use]
    pub const fn encode(self) -> u16 {
        let method = self.method.value();
        let class = self.class as u16;
        (method & 0x000f)
            | ((method & 0x0070) << 1)
            | ((method & 0x0f80) << 2)
            | ((class & 0x01) << 4)
            | ((class & 0x02) << 7)
    }

    /// Decodes an RFC 8489 message type.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidMessageType`] when either of the two most significant bits is
    /// set.
    pub const fn decode(value: u16) -> Result<Self, StunError> {
        if value & 0xc000 != 0 {
            return Err(StunError::InvalidMessageType(value));
        }
        let method = (value & 0x000f) | ((value & 0x00e0) >> 1) | ((value & 0x3e00) >> 2);
        let class = ((value >> 4) & 0x01) | ((value >> 7) & 0x02);
        Ok(Self::new(Method(method), MessageClass::from_raw(class)))
    }
}

/// A 96-bit STUN transaction identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionId([u8; 12]);

impl TransactionId {
    /// Creates a transaction identifier from exactly 12 random bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 12]) -> Self {
        Self(bytes)
    }

    /// Returns the transaction ID bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 12] {
        &self.0
    }
}

impl fmt::Debug for TransactionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TransactionId(")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

/// A STUN attribute type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttributeType(u16);

impl AttributeType {
    /// MAPPED-ADDRESS.
    pub const MAPPED_ADDRESS: Self = Self(0x0001);
    /// USERNAME.
    pub const USERNAME: Self = Self(0x0006);
    /// MESSAGE-INTEGRITY (HMAC-SHA1).
    pub const MESSAGE_INTEGRITY: Self = Self(0x0008);
    /// ERROR-CODE.
    pub const ERROR_CODE: Self = Self(0x0009);
    /// UNKNOWN-ATTRIBUTES.
    pub const UNKNOWN_ATTRIBUTES: Self = Self(0x000a);
    /// TURN CHANNEL-NUMBER.
    pub const CHANNEL_NUMBER: Self = Self(0x000c);
    /// TURN LIFETIME.
    pub const LIFETIME: Self = Self(0x000d);
    /// TURN XOR-PEER-ADDRESS.
    pub const XOR_PEER_ADDRESS: Self = Self(0x0012);
    /// TURN DATA.
    pub const DATA: Self = Self(0x0013);
    /// REALM.
    pub const REALM: Self = Self(0x0014);
    /// NONCE.
    pub const NONCE: Self = Self(0x0015);
    /// TURN XOR-RELAYED-ADDRESS.
    pub const XOR_RELAYED_ADDRESS: Self = Self(0x0016);
    /// TURN REQUESTED-ADDRESS-FAMILY.
    pub const REQUESTED_ADDRESS_FAMILY: Self = Self(0x0017);
    /// TURN REQUESTED-TRANSPORT.
    pub const REQUESTED_TRANSPORT: Self = Self(0x0019);
    /// TURN DONT-FRAGMENT.
    pub const DONT_FRAGMENT: Self = Self(0x001a);
    /// MESSAGE-INTEGRITY-SHA256.
    pub const MESSAGE_INTEGRITY_SHA256: Self = Self(0x001c);
    /// XOR-MAPPED-ADDRESS.
    pub const XOR_MAPPED_ADDRESS: Self = Self(0x0020);
    /// TURN RESERVATION-TOKEN.
    pub const RESERVATION_TOKEN: Self = Self(0x0022);
    /// ICE PRIORITY.
    pub const PRIORITY: Self = Self(0x0024);
    /// ICE USE-CANDIDATE.
    pub const USE_CANDIDATE: Self = Self(0x0025);
    /// SOFTWARE.
    pub const SOFTWARE: Self = Self(0x8022);
    /// ALTERNATE-SERVER.
    pub const ALTERNATE_SERVER: Self = Self(0x8023);
    /// FINGERPRINT.
    pub const FINGERPRINT: Self = Self(0x8028);
    /// ICE-CONTROLLED.
    pub const ICE_CONTROLLED: Self = Self(0x8029);
    /// ICE-CONTROLLING.
    pub const ICE_CONTROLLING: Self = Self(0x802a);

    /// Creates an attribute type from its wire value.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }

    /// Returns whether an unknown instance of this type must be understood.
    #[must_use]
    pub const fn is_comprehension_required(self) -> bool {
        self.0 < 0x8000
    }

    const fn is_known(self) -> bool {
        matches!(
            self,
            Self::MAPPED_ADDRESS
                | Self::USERNAME
                | Self::MESSAGE_INTEGRITY
                | Self::ERROR_CODE
                | Self::UNKNOWN_ATTRIBUTES
                | Self::CHANNEL_NUMBER
                | Self::LIFETIME
                | Self::XOR_PEER_ADDRESS
                | Self::DATA
                | Self::REALM
                | Self::NONCE
                | Self::XOR_RELAYED_ADDRESS
                | Self::REQUESTED_ADDRESS_FAMILY
                | Self::REQUESTED_TRANSPORT
                | Self::DONT_FRAGMENT
                | Self::MESSAGE_INTEGRITY_SHA256
                | Self::XOR_MAPPED_ADDRESS
                | Self::RESERVATION_TOKEN
                | Self::PRIORITY
                | Self::USE_CANDIDATE
                | Self::SOFTWARE
                | Self::ALTERNATE_SERVER
                | Self::FINGERPRINT
                | Self::ICE_CONTROLLED
                | Self::ICE_CONTROLLING
        )
    }
}

/// A borrowed STUN attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawAttribute<'a> {
    attribute_type: AttributeType,
    value: &'a [u8],
    start: usize,
}

impl<'a> RawAttribute<'a> {
    /// Returns the attribute type.
    #[must_use]
    pub const fn attribute_type(self) -> AttributeType {
        self.attribute_type
    }

    /// Returns the unpadded attribute value.
    #[must_use]
    pub const fn value(self) -> &'a [u8] {
        self.value
    }
}

/// A decoded STUN ERROR-CODE attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode<'a> {
    code: u16,
    reason: &'a str,
}

impl<'a> ErrorCode<'a> {
    /// Returns the three-digit STUN error code.
    #[must_use]
    pub const fn code(self) -> u16 {
        self.code
    }

    /// Returns the UTF-8 reason phrase.
    #[must_use]
    pub const fn reason(self) -> &'a str {
        self.reason
    }
}

/// A validated borrowed STUN message.
#[derive(Debug, Clone)]
pub struct Message<'a> {
    raw: &'a [u8],
    kind: MessageType,
    transaction_id: TransactionId,
    attributes: Vec<RawAttribute<'a>>,
}

impl<'a> Message<'a> {
    /// Parses one complete STUN message.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] for malformed headers, lengths, attribute ordering, or attribute
    /// shapes. No malformed input can cause a panic.
    pub fn parse(input: &'a [u8]) -> Result<Self, StunError> {
        let (kind, transaction_id, body) = parse_header(input)?;
        let attributes = parse_attributes(body)?;

        Ok(Self {
            raw: input,
            kind,
            transaction_id,
            attributes,
        })
    }

    /// Returns the message type.
    #[must_use]
    pub const fn message_type(&self) -> MessageType {
        self.kind
    }

    /// Returns the transaction identifier.
    #[must_use]
    pub const fn transaction_id(&self) -> TransactionId {
        self.transaction_id
    }

    /// Returns all attributes in wire order.
    #[must_use]
    pub fn attributes(&self) -> &[RawAttribute<'a>] {
        &self.attributes
    }

    /// Returns the first attribute with the requested type.
    #[must_use]
    pub fn attribute(&self, attribute_type: AttributeType) -> Option<RawAttribute<'a>> {
        self.attributes
            .iter()
            .copied()
            .find(|attribute| attribute.attribute_type == attribute_type)
    }

    /// Returns unknown comprehension-required attribute type values.
    #[must_use]
    pub fn unknown_required_attributes(&self) -> Vec<AttributeType> {
        self.attributes
            .iter()
            .map(|attribute| attribute.attribute_type)
            .filter(|attribute_type| {
                attribute_type.is_comprehension_required() && !attribute_type.is_known()
            })
            .collect()
    }

    /// Decodes ERROR-CODE.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the value is malformed or the reason is not UTF-8.
    pub fn error_code(&self) -> Result<Option<ErrorCode<'a>>, StunError> {
        let Some(attribute) = self.attribute(AttributeType::ERROR_CODE) else {
            return Ok(None);
        };
        if attribute.value.len() < 4 {
            return Err(StunError::InvalidAttributeLength {
                attribute_type: AttributeType::ERROR_CODE,
                expected: AttributeLength::Range {
                    minimum: 4,
                    maximum: 763,
                    multiple: 1,
                },
                actual: attribute.value.len(),
            });
        }
        let reserved = attribute
            .value
            .get(..2)
            .ok_or(StunError::InvalidErrorCode(0))?;
        if reserved != [0, 0] {
            return Err(StunError::InvalidErrorCode(0));
        }
        let class = attribute.value[2] & 0x07;
        let number = attribute.value[3];
        let code = u16::from(class) * 100 + u16::from(number);
        if !(300..=699).contains(&code) || number > 99 {
            return Err(StunError::InvalidErrorCode(code));
        }
        let reason_bytes = attribute
            .value
            .get(4..)
            .ok_or(StunError::InvalidErrorCode(code))?;
        let reason = std::str::from_utf8(reason_bytes)
            .map_err(|_| StunError::InvalidUtf8(AttributeType::ERROR_CODE))?;
        Ok(Some(ErrorCode { code, reason }))
    }

    /// Decodes the list in UNKNOWN-ATTRIBUTES.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the attribute has an odd byte length.
    pub fn unknown_attributes(&self) -> Result<Vec<AttributeType>, StunError> {
        let Some(attribute) = self.attribute(AttributeType::UNKNOWN_ATTRIBUTES) else {
            return Ok(Vec::new());
        };
        if attribute.value.len() % 2 != 0 {
            return Err(StunError::InvalidAttributeLength {
                attribute_type: AttributeType::UNKNOWN_ATTRIBUTES,
                expected: AttributeLength::Range {
                    minimum: 0,
                    maximum: u16::MAX as usize,
                    multiple: 2,
                },
                actual: attribute.value.len(),
            });
        }
        Ok(attribute
            .value
            .chunks_exact(2)
            .map(|bytes| AttributeType::new(u16::from_be_bytes([bytes[0], bytes[1]])))
            .collect())
    }

    /// Decodes the UTF-8 USERNAME value.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidUtf8`] when USERNAME is not valid UTF-8.
    pub fn username(&self) -> Result<Option<&'a str>, StunError> {
        self.utf8_attribute(AttributeType::USERNAME)
    }

    /// Decodes the UTF-8 REALM value.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidUtf8`] when REALM is not valid UTF-8.
    pub fn realm(&self) -> Result<Option<&'a str>, StunError> {
        self.utf8_attribute(AttributeType::REALM)
    }

    /// Decodes the UTF-8 NONCE value.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidUtf8`] when NONCE is not valid UTF-8.
    pub fn nonce(&self) -> Result<Option<&'a str>, StunError> {
        self.utf8_attribute(AttributeType::NONCE)
    }

    /// Decodes the UTF-8 SOFTWARE value.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidUtf8`] when SOFTWARE is not valid UTF-8.
    pub fn software(&self) -> Result<Option<&'a str>, StunError> {
        self.utf8_attribute(AttributeType::SOFTWARE)
    }

    /// Decodes ICE PRIORITY.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the attribute has an invalid shape.
    pub fn priority(&self) -> Result<Option<u32>, StunError> {
        self.u32_attribute(AttributeType::PRIORITY)
    }

    /// Returns whether USE-CANDIDATE is present.
    #[must_use]
    pub fn use_candidate(&self) -> bool {
        self.attribute(AttributeType::USE_CANDIDATE).is_some()
    }

    /// Decodes ICE-CONTROLLING.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the attribute has an invalid shape.
    pub fn ice_controlling(&self) -> Result<Option<u64>, StunError> {
        self.u64_attribute(AttributeType::ICE_CONTROLLING)
    }

    /// Decodes ICE-CONTROLLED.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the attribute has an invalid shape.
    pub fn ice_controlled(&self) -> Result<Option<u64>, StunError> {
        self.u64_attribute(AttributeType::ICE_CONTROLLED)
    }

    /// Decodes XOR-MAPPED-ADDRESS.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the address family or encoded length is invalid.
    pub fn xor_mapped_address(&self) -> Result<Option<SocketAddr>, StunError> {
        self.xor_address(AttributeType::XOR_MAPPED_ADDRESS)
    }

    /// Decodes the first XOR address attribute of the requested type.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when the address family or encoded length is invalid.
    pub fn xor_address(
        &self,
        attribute_type: AttributeType,
    ) -> Result<Option<SocketAddr>, StunError> {
        let Some(attribute) = self.attribute(attribute_type) else {
            return Ok(None);
        };
        decode_xor_address(attribute.value, self.transaction_id).map(Some)
    }

    /// Decodes every XOR address attribute of the requested type.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] when any address family or encoded length is invalid.
    pub fn xor_addresses(
        &self,
        attribute_type: AttributeType,
    ) -> Result<Vec<SocketAddr>, StunError> {
        self.attributes
            .iter()
            .filter(|attribute| attribute.attribute_type == attribute_type)
            .map(|attribute| decode_xor_address(attribute.value, self.transaction_id))
            .collect()
    }

    /// Verifies MESSAGE-INTEGRITY using an ICE short-term password.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::MissingAttribute`] when MESSAGE-INTEGRITY is absent and
    /// [`StunError::IntegrityMismatch`] when authentication fails.
    pub fn verify_message_integrity_sha1(&self, key: &[u8]) -> Result<(), StunError> {
        let attribute =
            self.attribute(AttributeType::MESSAGE_INTEGRITY)
                .ok_or(StunError::MissingAttribute(
                    AttributeType::MESSAGE_INTEGRITY,
                ))?;
        let input = self.integrity_input(attribute)?;
        let mut mac = HmacSha1::new_from_slice(key).map_err(|_| StunError::InvalidIntegrityKey)?;
        mac.update(&input);
        mac.verify_slice(attribute.value)
            .map_err(|_| StunError::IntegrityMismatch)
    }

    /// Verifies MESSAGE-INTEGRITY-SHA256, including allowed truncated values.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::MissingAttribute`] when MESSAGE-INTEGRITY-SHA256 is absent and
    /// [`StunError::IntegrityMismatch`] when authentication fails.
    pub fn verify_message_integrity_sha256(&self, key: &[u8]) -> Result<(), StunError> {
        let attribute = self
            .attribute(AttributeType::MESSAGE_INTEGRITY_SHA256)
            .ok_or(StunError::MissingAttribute(
                AttributeType::MESSAGE_INTEGRITY_SHA256,
            ))?;
        let input = self.integrity_input(attribute)?;
        let mut mac =
            HmacSha256::new_from_slice(key).map_err(|_| StunError::InvalidIntegrityKey)?;
        mac.update(&input);
        let output = mac.finalize().into_bytes();
        if attribute
            .value
            .ct_eq(&output[..attribute.value.len()])
            .into()
        {
            Ok(())
        } else {
            Err(StunError::IntegrityMismatch)
        }
    }

    /// Verifies the final FINGERPRINT attribute.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::MissingAttribute`] when FINGERPRINT is absent and
    /// [`StunError::FingerprintMismatch`] when the CRC does not match.
    pub fn verify_fingerprint(&self) -> Result<(), StunError> {
        let attribute = self
            .attribute(AttributeType::FINGERPRINT)
            .ok_or(StunError::MissingAttribute(AttributeType::FINGERPRINT))?;
        let expected = read_exact_u32(attribute.value, attribute.attribute_type)?;
        let actual = crc32(
            self.raw
                .get(..attribute.start)
                .ok_or(StunError::FingerprintMismatch)?,
        ) ^ FINGERPRINT_XOR;
        if actual == expected {
            Ok(())
        } else {
            Err(StunError::FingerprintMismatch)
        }
    }

    fn utf8_attribute(&self, attribute_type: AttributeType) -> Result<Option<&'a str>, StunError> {
        self.attribute(attribute_type)
            .map(|attribute| {
                std::str::from_utf8(attribute.value)
                    .map_err(|_| StunError::InvalidUtf8(attribute_type))
            })
            .transpose()
    }

    fn u32_attribute(&self, attribute_type: AttributeType) -> Result<Option<u32>, StunError> {
        self.attribute(attribute_type)
            .map(|attribute| read_exact_u32(attribute.value, attribute_type))
            .transpose()
    }

    fn u64_attribute(&self, attribute_type: AttributeType) -> Result<Option<u64>, StunError> {
        self.attribute(attribute_type)
            .map(|attribute| {
                let mut cursor = ReadCursor::new(attribute.value);
                let value = cursor.read_u64()?;
                if !cursor.is_empty() {
                    return Err(StunError::InvalidAttributeLength {
                        attribute_type,
                        expected: AttributeLength::Exact(8),
                        actual: attribute.value.len(),
                    });
                }
                Ok(value)
            })
            .transpose()
    }

    fn integrity_input(&self, attribute: RawAttribute<'a>) -> Result<Vec<u8>, StunError> {
        let end = attribute
            .start
            .checked_add(4)
            .and_then(|value| value.checked_add(attribute.value.len()))
            .ok_or(StunError::MessageTooLarge)?;
        let adjusted_len = end
            .checked_sub(HEADER_LEN)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or(StunError::MessageTooLarge)?;
        let prefix = self
            .raw
            .get(..attribute.start)
            .ok_or(StunError::IntegrityMismatch)?;
        let mut input = prefix.to_vec();
        let length_bytes = input.get_mut(2..4).ok_or(StunError::IntegrityMismatch)?;
        length_bytes.copy_from_slice(&adjusted_len.to_be_bytes());
        Ok(input)
    }
}

#[derive(Debug, Clone)]
enum Integrity {
    Sha1(Vec<u8>),
    Sha256(Vec<u8>),
}

/// Builds a STUN message while enforcing integrity and fingerprint ordering.
#[derive(Debug, Clone)]
pub struct MessageBuilder {
    message_type: MessageType,
    transaction_id: TransactionId,
    attributes: Vec<(AttributeType, Vec<u8>)>,
    integrity: Option<Integrity>,
    fingerprint: bool,
}

impl MessageBuilder {
    /// Creates an empty STUN message.
    #[must_use]
    pub const fn new(message_type: MessageType, transaction_id: TransactionId) -> Self {
        Self {
            message_type,
            transaction_id,
            attributes: Vec::new(),
            integrity: None,
            fingerprint: false,
        }
    }

    /// Adds an arbitrary attribute before message integrity.
    #[must_use]
    pub fn raw_attribute(mut self, attribute_type: AttributeType, value: Vec<u8>) -> Self {
        self.attributes.push((attribute_type, value));
        self
    }

    /// Adds USERNAME.
    #[must_use]
    pub fn username(self, value: impl Into<String>) -> Self {
        self.raw_attribute(AttributeType::USERNAME, value.into().into_bytes())
    }

    /// Adds SOFTWARE.
    #[must_use]
    pub fn software(self, value: impl Into<String>) -> Self {
        self.raw_attribute(AttributeType::SOFTWARE, value.into().into_bytes())
    }

    /// Adds ICE PRIORITY.
    #[must_use]
    pub fn priority(self, value: u32) -> Self {
        self.raw_attribute(AttributeType::PRIORITY, value.to_be_bytes().to_vec())
    }

    /// Adds USE-CANDIDATE.
    #[must_use]
    pub fn use_candidate(self) -> Self {
        self.raw_attribute(AttributeType::USE_CANDIDATE, Vec::new())
    }

    /// Adds ICE-CONTROLLING.
    #[must_use]
    pub fn ice_controlling(self, tie_breaker: u64) -> Self {
        self.raw_attribute(
            AttributeType::ICE_CONTROLLING,
            tie_breaker.to_be_bytes().to_vec(),
        )
    }

    /// Adds ICE-CONTROLLED.
    #[must_use]
    pub fn ice_controlled(self, tie_breaker: u64) -> Self {
        self.raw_attribute(
            AttributeType::ICE_CONTROLLED,
            tie_breaker.to_be_bytes().to_vec(),
        )
    }

    /// Adds XOR-MAPPED-ADDRESS.
    #[must_use]
    pub fn xor_mapped_address(self, address: SocketAddr) -> Self {
        self.xor_address(AttributeType::XOR_MAPPED_ADDRESS, address)
    }

    /// Adds an arbitrary XOR address attribute, including TURN peer/relay addresses.
    #[must_use]
    pub fn xor_address(self, attribute_type: AttributeType, address: SocketAddr) -> Self {
        let transaction_id = self.transaction_id;
        self.raw_attribute(attribute_type, encode_xor_address(address, transaction_id))
    }

    /// Adds ERROR-CODE.
    ///
    /// # Errors
    ///
    /// Returns [`StunError::InvalidErrorCode`] unless `code` is in the STUN error range, or
    /// [`StunError::ReasonPhraseTooLong`] when the UTF-8 reason exceeds the wire limit.
    pub fn error_code(self, code: u16, reason: impl Into<String>) -> Result<Self, StunError> {
        let class = code / 100;
        let number = code % 100;
        if !(3..=6).contains(&class) {
            return Err(StunError::InvalidErrorCode(code));
        }
        let reason = reason.into();
        if reason.len() > 759 {
            return Err(StunError::ReasonPhraseTooLong(reason.len()));
        }
        let mut value = Vec::with_capacity(4 + reason.len());
        value.extend_from_slice(&[0, 0, u8::try_from(class).unwrap_or_default()]);
        value.push(u8::try_from(number).unwrap_or_default());
        value.extend_from_slice(reason.as_bytes());
        Ok(self.raw_attribute(AttributeType::ERROR_CODE, value))
    }

    /// Adds a sorted, deduplicated UNKNOWN-ATTRIBUTES list.
    #[must_use]
    pub fn unknown_attributes(self, attribute_types: &[AttributeType]) -> Self {
        let mut values: Vec<u16> = attribute_types
            .iter()
            .map(|attribute_type| attribute_type.value())
            .collect();
        values.sort_unstable();
        values.dedup();
        let value = values
            .into_iter()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>();
        self.raw_attribute(AttributeType::UNKNOWN_ATTRIBUTES, value)
    }

    /// Adds MESSAGE-INTEGRITY using HMAC-SHA1.
    #[must_use]
    pub fn message_integrity_sha1(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.integrity = Some(Integrity::Sha1(key.into()));
        self
    }

    /// Adds MESSAGE-INTEGRITY-SHA256 using the full 32-byte output.
    #[must_use]
    pub fn message_integrity_sha256(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.integrity = Some(Integrity::Sha256(key.into()));
        self
    }

    /// Adds a final FINGERPRINT.
    #[must_use]
    pub const fn fingerprint(mut self) -> Self {
        self.fingerprint = true;
        self
    }

    /// Encodes the complete STUN message.
    ///
    /// # Errors
    ///
    /// Returns [`StunError`] if an attribute or the entire message exceeds RFC 8489 limits.
    pub fn build(self) -> Result<Vec<u8>, StunError> {
        let mut output = WriteBuffer::with_limit(MAX_MESSAGE_LEN);
        output.write_u16(self.message_type.encode())?;
        output.write_u16(0)?;
        output.write_u32(MAGIC_COOKIE)?;
        output.extend_from_slice(self.transaction_id.as_bytes())?;

        for (attribute_type, value) in self.attributes {
            encode_attribute(&mut output, attribute_type, &value)?;
        }

        if let Some(integrity) = self.integrity {
            match integrity {
                Integrity::Sha1(key) => {
                    set_length_through_attribute(&mut output, 20)?;
                    let mut mac = HmacSha1::new_from_slice(&key)
                        .map_err(|_| StunError::InvalidIntegrityKey)?;
                    mac.update(output.as_slice());
                    let value = mac.finalize().into_bytes();
                    encode_attribute(
                        &mut output,
                        AttributeType::MESSAGE_INTEGRITY,
                        value.as_slice(),
                    )?;
                }
                Integrity::Sha256(key) => {
                    set_length_through_attribute(&mut output, 32)?;
                    let mut mac = HmacSha256::new_from_slice(&key)
                        .map_err(|_| StunError::InvalidIntegrityKey)?;
                    mac.update(output.as_slice());
                    let value = mac.finalize().into_bytes();
                    encode_attribute(
                        &mut output,
                        AttributeType::MESSAGE_INTEGRITY_SHA256,
                        value.as_slice(),
                    )?;
                }
            }
        }

        if self.fingerprint {
            set_length_through_attribute(&mut output, 4)?;
            let value = crc32(output.as_slice()) ^ FINGERPRINT_XOR;
            encode_attribute(
                &mut output,
                AttributeType::FINGERPRINT,
                &value.to_be_bytes(),
            )?;
        } else {
            set_current_length(&mut output)?;
        }

        Ok(output.into_vec())
    }
}

/// Errors produced by STUN encoding, parsing, and authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StunError {
    /// Fewer than 20 header bytes were supplied.
    MessageTooShort(usize),
    /// The top two bits of the message type were not zero.
    InvalidMessageType(u16),
    /// A method did not fit in 12 bits.
    InvalidMethod(u16),
    /// The declared and actual complete message lengths differ.
    InvalidMessageLength {
        /// Complete length derived from the header.
        declared: usize,
        /// Supplied byte-slice length.
        actual: usize,
    },
    /// The STUN body length was not divisible by four.
    LengthNotAligned(usize),
    /// The RFC 8489 magic cookie did not match.
    InvalidMagicCookie(u32),
    /// The message exceeded the 16-bit STUN length.
    MessageTooLarge,
    /// A typed attribute had an invalid length.
    InvalidAttributeLength {
        /// Attribute type.
        attribute_type: AttributeType,
        /// Required length.
        expected: AttributeLength,
        /// Received length.
        actual: usize,
    },
    /// An attribute that must be unique occurred more than once.
    DuplicateAttribute(AttributeType),
    /// An unprotected attribute followed message integrity.
    AttributeAfterIntegrity(AttributeType),
    /// FINGERPRINT was not the final attribute.
    FingerprintNotLast,
    /// A text attribute was not valid UTF-8.
    InvalidUtf8(AttributeType),
    /// An address attribute used an unsupported family.
    UnsupportedAddressFamily(u8),
    /// ERROR-CODE was outside the RFC-defined range or malformed.
    InvalidErrorCode(u16),
    /// An ERROR-CODE reason phrase exceeded the maximum value length.
    ReasonPhraseTooLong(usize),
    /// A required attribute was absent.
    MissingAttribute(AttributeType),
    /// The HMAC key was rejected.
    InvalidIntegrityKey,
    /// MESSAGE-INTEGRITY verification failed.
    IntegrityMismatch,
    /// FINGERPRINT verification failed.
    FingerprintMismatch,
    /// A checked byte read failed.
    Decode(DecodeError),
    /// A bounded byte write failed.
    Encode(EncodeError),
}

impl fmt::Display for StunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageTooShort(length) => {
                write!(formatter, "STUN message is shorter than 20 bytes: {length}")
            }
            Self::InvalidMessageType(value) => {
                write!(formatter, "invalid STUN message type {value:#06x}")
            }
            Self::InvalidMethod(value) => write!(formatter, "invalid STUN method {value:#06x}"),
            Self::InvalidMessageLength { declared, actual } => {
                write!(
                    formatter,
                    "STUN length mismatch: declared {declared}, actual {actual}"
                )
            }
            Self::LengthNotAligned(length) => {
                write!(
                    formatter,
                    "STUN body length is not 32-bit aligned: {length}"
                )
            }
            Self::InvalidMagicCookie(value) => {
                write!(formatter, "invalid STUN magic cookie {value:#010x}")
            }
            Self::MessageTooLarge => formatter.write_str("STUN message exceeds the wire limit"),
            Self::InvalidAttributeLength {
                attribute_type,
                expected,
                actual,
            } => write!(
                formatter,
                "invalid length for STUN attribute {:#06x}: expected {expected}, actual {actual}",
                attribute_type.value()
            ),
            Self::DuplicateAttribute(attribute_type) => {
                write!(
                    formatter,
                    "duplicate STUN attribute {:#06x}",
                    attribute_type.value()
                )
            }
            Self::AttributeAfterIntegrity(attribute_type) => write!(
                formatter,
                "unprotected STUN attribute after integrity: {:#06x}",
                attribute_type.value()
            ),
            Self::FingerprintNotLast => formatter.write_str("STUN FINGERPRINT is not last"),
            Self::InvalidUtf8(attribute_type) => write!(
                formatter,
                "STUN attribute {:#06x} is not valid UTF-8",
                attribute_type.value()
            ),
            Self::UnsupportedAddressFamily(family) => {
                write!(formatter, "unsupported STUN address family {family:#04x}")
            }
            Self::InvalidErrorCode(code) => write!(formatter, "invalid STUN error code {code}"),
            Self::ReasonPhraseTooLong(length) => {
                write!(formatter, "STUN reason phrase is too long: {length} bytes")
            }
            Self::MissingAttribute(attribute_type) => {
                write!(
                    formatter,
                    "missing STUN attribute {:#06x}",
                    attribute_type.value()
                )
            }
            Self::InvalidIntegrityKey => formatter.write_str("invalid STUN integrity key"),
            Self::IntegrityMismatch => formatter.write_str("STUN message integrity mismatch"),
            Self::FingerprintMismatch => formatter.write_str("STUN fingerprint mismatch"),
            Self::Decode(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StunError {}

impl From<DecodeError> for StunError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<EncodeError> for StunError {
    fn from(value: EncodeError) -> Self {
        Self::Encode(value)
    }
}

/// Expected shape of an attribute value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeLength {
    /// Exactly this many bytes.
    Exact(usize),
    /// An inclusive range and required multiple.
    Range {
        /// Minimum bytes.
        minimum: usize,
        /// Maximum bytes.
        maximum: usize,
        /// Required byte multiple.
        multiple: usize,
    },
}

impl fmt::Display for AttributeLength {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(length) => write!(formatter, "{length} bytes"),
            Self::Range {
                minimum,
                maximum,
                multiple,
            } => write!(
                formatter,
                "{minimum}..={maximum} bytes in multiples of {multiple}"
            ),
        }
    }
}

fn parse_header(input: &[u8]) -> Result<(MessageType, TransactionId, &[u8]), StunError> {
    if input.len() < HEADER_LEN {
        return Err(StunError::MessageTooShort(input.len()));
    }

    let mut header = ReadCursor::new(input);
    let message_type = MessageType::decode(header.read_u16()?)?;
    let body_len = usize::from(header.read_u16()?);
    if body_len % 4 != 0 {
        return Err(StunError::LengthNotAligned(body_len));
    }

    let cookie = header.read_u32()?;
    if cookie != MAGIC_COOKIE {
        return Err(StunError::InvalidMagicCookie(cookie));
    }

    let transaction_id = {
        let bytes = header.take(12)?;
        let bytes: [u8; 12] = bytes
            .try_into()
            .map_err(|_| StunError::MessageTooShort(input.len()))?;
        TransactionId::new(bytes)
    };

    let declared_len = HEADER_LEN
        .checked_add(body_len)
        .ok_or(StunError::InvalidMessageLength {
            declared: body_len,
            actual: input.len(),
        })?;
    if declared_len != input.len() {
        return Err(StunError::InvalidMessageLength {
            declared: declared_len,
            actual: input.len(),
        });
    }

    Ok((message_type, transaction_id, header.rest()))
}

fn parse_attributes(body_bytes: &[u8]) -> Result<Vec<RawAttribute<'_>>, StunError> {
    let mut body = ReadCursor::new(body_bytes);
    let mut attributes = Vec::new();
    let mut seen_sha1 = false;
    let mut seen_sha256 = false;
    let mut seen_fingerprint = false;

    while !body.is_empty() {
        let start = HEADER_LEN + body.position();
        let attribute_type = AttributeType::new(body.read_u16()?);
        let value_len = usize::from(body.read_u16()?);
        let value = body.take(value_len)?;
        let padding = (4 - (value_len % 4)) % 4;
        body.take(padding)?;

        if seen_fingerprint {
            return Err(StunError::FingerprintNotLast);
        }
        if (seen_sha1 || seen_sha256)
            && !matches!(
                attribute_type,
                AttributeType::MESSAGE_INTEGRITY_SHA256 | AttributeType::FINGERPRINT
            )
        {
            return Err(StunError::AttributeAfterIntegrity(attribute_type));
        }

        match attribute_type {
            AttributeType::MESSAGE_INTEGRITY => {
                if seen_sha1 {
                    return Err(StunError::DuplicateAttribute(attribute_type));
                }
                require_len(attribute_type, value, 20)?;
                seen_sha1 = true;
            }
            AttributeType::MESSAGE_INTEGRITY_SHA256 => {
                if seen_sha256 {
                    return Err(StunError::DuplicateAttribute(attribute_type));
                }
                if !(16..=32).contains(&value.len()) || value.len() % 4 != 0 {
                    return Err(StunError::InvalidAttributeLength {
                        attribute_type,
                        expected: AttributeLength::Range {
                            minimum: 16,
                            maximum: 32,
                            multiple: 4,
                        },
                        actual: value.len(),
                    });
                }
                seen_sha256 = true;
            }
            AttributeType::FINGERPRINT => {
                require_len(attribute_type, value, 4)?;
                seen_fingerprint = true;
            }
            AttributeType::PRIORITY
            | AttributeType::CHANNEL_NUMBER
            | AttributeType::LIFETIME
            | AttributeType::REQUESTED_ADDRESS_FAMILY
            | AttributeType::REQUESTED_TRANSPORT => require_len(attribute_type, value, 4)?,
            AttributeType::DONT_FRAGMENT | AttributeType::USE_CANDIDATE => {
                require_len(attribute_type, value, 0)?;
            }
            AttributeType::RESERVATION_TOKEN => require_len(attribute_type, value, 8)?,
            AttributeType::XOR_MAPPED_ADDRESS
            | AttributeType::XOR_PEER_ADDRESS
            | AttributeType::XOR_RELAYED_ADDRESS => {
                if !matches!(value.len(), 8 | 20) {
                    return Err(StunError::InvalidAttributeLength {
                        attribute_type,
                        expected: AttributeLength::Range {
                            minimum: 8,
                            maximum: 20,
                            multiple: 4,
                        },
                        actual: value.len(),
                    });
                }
            }
            AttributeType::ICE_CONTROLLED | AttributeType::ICE_CONTROLLING => {
                require_len(attribute_type, value, 8)?;
            }
            _ => {}
        }

        attributes.push(RawAttribute {
            attribute_type,
            value,
            start,
        });
    }

    Ok(attributes)
}

fn require_len(
    attribute_type: AttributeType,
    value: &[u8],
    expected: usize,
) -> Result<(), StunError> {
    if value.len() == expected {
        Ok(())
    } else {
        Err(StunError::InvalidAttributeLength {
            attribute_type,
            expected: AttributeLength::Exact(expected),
            actual: value.len(),
        })
    }
}

fn read_exact_u32(value: &[u8], attribute_type: AttributeType) -> Result<u32, StunError> {
    require_len(attribute_type, value, 4)?;
    let mut cursor = ReadCursor::new(value);
    Ok(cursor.read_u32()?)
}

fn encode_attribute(
    output: &mut WriteBuffer,
    attribute_type: AttributeType,
    value: &[u8],
) -> Result<(), StunError> {
    let value_len = u16::try_from(value.len()).map_err(|_| StunError::MessageTooLarge)?;
    output.write_u16(attribute_type.value())?;
    output.write_u16(value_len)?;
    output.extend_from_slice(value)?;
    let padding = (4 - (value.len() % 4)) % 4;
    output.extend_from_slice(&[0; 3][..padding])?;
    Ok(())
}

fn set_length_through_attribute(
    output: &mut WriteBuffer,
    value_len: usize,
) -> Result<(), StunError> {
    let additional = 4usize
        .checked_add(value_len)
        .and_then(|length| length.checked_add((4 - (value_len % 4)) % 4))
        .ok_or(StunError::MessageTooLarge)?;
    let complete = output
        .len()
        .checked_add(additional)
        .ok_or(StunError::MessageTooLarge)?;
    set_wire_length(output, complete)
}

fn set_current_length(output: &mut WriteBuffer) -> Result<(), StunError> {
    set_wire_length(output, output.len())
}

fn set_wire_length(output: &mut WriteBuffer, complete_len: usize) -> Result<(), StunError> {
    let body_len = complete_len
        .checked_sub(HEADER_LEN)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or(StunError::MessageTooLarge)?;
    output.set_u16(2, body_len)?;
    Ok(())
}

fn encode_xor_address(address: SocketAddr, transaction_id: TransactionId) -> Vec<u8> {
    let mut value = Vec::with_capacity(match address {
        SocketAddr::V4(_) => 8,
        SocketAddr::V6(_) => 20,
    });
    value.push(0);
    let xor_port = address.port() ^ u16::try_from(MAGIC_COOKIE >> 16).unwrap_or_default();
    match address.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01);
            value.extend_from_slice(&xor_port.to_be_bytes());
            let encoded = u32::from(ip) ^ MAGIC_COOKIE;
            value.extend_from_slice(&encoded.to_be_bytes());
        }
        IpAddr::V6(ip) => {
            value.push(0x02);
            value.extend_from_slice(&xor_port.to_be_bytes());
            let mut mask = [0_u8; 16];
            mask[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
            mask[4..].copy_from_slice(transaction_id.as_bytes());
            for (byte, mask_byte) in ip.octets().into_iter().zip(mask) {
                value.push(byte ^ mask_byte);
            }
        }
    }
    value
}

fn decode_xor_address(
    value: &[u8],
    transaction_id: TransactionId,
) -> Result<SocketAddr, StunError> {
    let mut cursor = ReadCursor::new(value);
    cursor.read_u8()?;
    let family = cursor.read_u8()?;
    let port = cursor.read_u16()? ^ u16::try_from(MAGIC_COOKIE >> 16).unwrap_or_default();
    match family {
        0x01 => {
            require_len(AttributeType::XOR_MAPPED_ADDRESS, value, 8)?;
            let address = cursor.read_u32()? ^ MAGIC_COOKIE;
            Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(address)), port))
        }
        0x02 => {
            require_len(AttributeType::XOR_MAPPED_ADDRESS, value, 20)?;
            let encoded = cursor.take(16)?;
            let mut mask = [0_u8; 16];
            mask[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
            mask[4..].copy_from_slice(transaction_id.as_bytes());
            let mut decoded = [0_u8; 16];
            for ((output, encoded_byte), mask_byte) in
                decoded.iter_mut().zip(encoded.iter().copied()).zip(mask)
            {
                *output = encoded_byte ^ mask_byte;
            }
            Ok(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(decoded)), port))
        }
        other => Err(StunError::UnsupportedAddressFamily(other)),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    use super::{
        AttributeType, Message, MessageBuilder, MessageClass, MessageType, Method, StunError,
        TransactionId,
    };

    const TRANSACTION_ID: TransactionId = TransactionId::new([
        0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae,
    ]);

    #[test]
    fn encodes_known_binding_message_types() {
        let cases = [
            (MessageClass::Request, 0x0001),
            (MessageClass::Indication, 0x0011),
            (MessageClass::SuccessResponse, 0x0101),
            (MessageClass::ErrorResponse, 0x0111),
        ];

        for (class, expected) in cases {
            let message_type = MessageType::new(Method::BINDING, class);
            assert_eq!(message_type.encode(), expected);
            assert_eq!(MessageType::decode(expected), Ok(message_type));
        }
    }

    #[test]
    fn builds_and_parses_ice_binding_request() {
        let bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TRANSACTION_ID,
        )
        .username("server:client")
        .priority(1_847_000_000)
        .ice_controlling(0x932f_f9b1_5126_3b36)
        .use_candidate()
        .message_integrity_sha1(b"secret".to_vec())
        .fingerprint()
        .build()
        .expect("valid test message");

        let message = Message::parse(&bytes).expect("builder output must parse");
        assert_eq!(message.username(), Ok(Some("server:client")));
        assert_eq!(message.priority(), Ok(Some(1_847_000_000)));
        assert_eq!(message.ice_controlling(), Ok(Some(0x932f_f9b1_5126_3b36)));
        assert!(message.use_candidate());
        assert_eq!(message.verify_message_integrity_sha1(b"secret"), Ok(()));
        assert_eq!(message.verify_fingerprint(), Ok(()));
    }

    #[test]
    fn round_trips_ipv4_and_ipv6_xor_addresses() {
        let addresses = [
            SocketAddr::from((Ipv4Addr::new(203, 0, 113, 7), 50000)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 3478)),
        ];

        for address in addresses {
            let bytes = MessageBuilder::new(
                MessageType::new(Method::BINDING, MessageClass::SuccessResponse),
                TRANSACTION_ID,
            )
            .xor_mapped_address(address)
            .fingerprint()
            .build()
            .expect("valid address response");
            let message = Message::parse(&bytes).expect("builder output must parse");
            assert_eq!(message.xor_mapped_address(), Ok(Some(address)));
        }
    }

    #[test]
    fn verifies_rfc_5769_short_term_request_vector() {
        let bytes = [
            0x00, 0x01, 0x00, 0x58, 0x21, 0x12, 0xa4, 0x42, 0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34,
            0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae, 0x80, 0x22, 0x00, 0x10, 0x53, 0x54, 0x55, 0x4e,
            0x20, 0x74, 0x65, 0x73, 0x74, 0x20, 0x63, 0x6c, 0x69, 0x65, 0x6e, 0x74, 0x00, 0x24,
            0x00, 0x04, 0x6e, 0x00, 0x01, 0xff, 0x80, 0x29, 0x00, 0x08, 0x93, 0x2f, 0xf9, 0xb1,
            0x51, 0x26, 0x3b, 0x36, 0x00, 0x06, 0x00, 0x09, 0x65, 0x76, 0x74, 0x6a, 0x3a, 0x68,
            0x36, 0x76, 0x59, 0x20, 0x20, 0x20, 0x00, 0x08, 0x00, 0x14, 0x9a, 0xea, 0xa7, 0x0c,
            0xbf, 0xd8, 0xcb, 0x56, 0x78, 0x1e, 0xf2, 0xb5, 0xb2, 0xd3, 0xf2, 0x49, 0xc1, 0xb5,
            0x71, 0xa2, 0x80, 0x28, 0x00, 0x04, 0xe5, 0x7a, 0x3b, 0xcf,
        ];

        let message = Message::parse(&bytes).expect("RFC vector must parse");
        assert_eq!(message.username(), Ok(Some("evtj:h6vY")));
        assert_eq!(message.software(), Ok(Some("STUN test client")));
        assert_eq!(
            message.verify_message_integrity_sha1(b"VOkJxbRl1RmTxUk/WvJxBt"),
            Ok(())
        );
        assert_eq!(message.verify_fingerprint(), Ok(()));
    }

    #[test]
    fn rejects_tampered_integrity_and_fingerprint() {
        let mut bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TRANSACTION_ID,
        )
        .username("a:b")
        .message_integrity_sha1(b"secret".to_vec())
        .fingerprint()
        .build()
        .expect("valid test message");

        bytes[24] ^= 1;
        let message = Message::parse(&bytes).expect("tampering kept message shape valid");
        assert_eq!(
            message.verify_message_integrity_sha1(b"secret"),
            Err(StunError::IntegrityMismatch)
        );
        assert_eq!(
            message.verify_fingerprint(),
            Err(StunError::FingerprintMismatch)
        );
    }

    #[test]
    fn builds_and_verifies_sha256_integrity() {
        let bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TRANSACTION_ID,
        )
        .username("server:client")
        .message_integrity_sha256(b"stronger-secret".to_vec())
        .fingerprint()
        .build()
        .expect("valid SHA-256 test message");

        let message = Message::parse(&bytes).expect("builder output must parse");
        assert_eq!(
            message.verify_message_integrity_sha256(b"stronger-secret"),
            Ok(())
        );
        assert_eq!(
            message.verify_message_integrity_sha256(b"wrong-secret"),
            Err(StunError::IntegrityMismatch)
        );
        assert_eq!(message.verify_fingerprint(), Ok(()));
    }

    #[test]
    fn reports_unknown_comprehension_required_attributes() {
        let bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TRANSACTION_ID,
        )
        .raw_attribute(AttributeType::new(0x1234), vec![1, 2, 3])
        .raw_attribute(AttributeType::new(0x9234), vec![4])
        .build()
        .expect("valid test message");
        let message = Message::parse(&bytes).expect("valid test message");

        assert_eq!(
            message.unknown_required_attributes(),
            vec![AttributeType::new(0x1234)]
        );
    }

    #[test]
    fn round_trips_error_code_and_unknown_attributes() {
        let unknown = [AttributeType::new(0x1234), AttributeType::new(0x0042)];
        let bytes = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::ErrorResponse),
            TRANSACTION_ID,
        )
        .error_code(420, "Unknown Attribute")
        .expect("valid error code")
        .unknown_attributes(&unknown)
        .fingerprint()
        .build()
        .expect("valid error response");
        let message = Message::parse(&bytes).expect("builder output must parse");

        let error = message
            .error_code()
            .expect("valid error attribute")
            .expect("error attribute present");
        assert_eq!(error.code(), 420);
        assert_eq!(error.reason(), "Unknown Attribute");
        assert_eq!(
            message.unknown_attributes(),
            Ok(vec![AttributeType::new(0x0042), AttributeType::new(0x1234)])
        );
    }

    #[test]
    fn rejects_non_aligned_and_trailing_messages() {
        let non_aligned = [
            0x00, 0x01, 0x00, 0x01, 0x21, 0x12, 0xa4, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            Message::parse(&non_aligned).expect_err("length is not aligned"),
            StunError::LengthNotAligned(1)
        );

        let mut valid = MessageBuilder::new(
            MessageType::new(Method::BINDING, MessageClass::Request),
            TRANSACTION_ID,
        )
        .build()
        .expect("valid empty message");
        valid.push(0);
        assert_eq!(
            Message::parse(&valid).expect_err("trailing byte is not part of STUN message"),
            StunError::InvalidMessageLength {
                declared: 20,
                actual: 21
            }
        );
    }
}
