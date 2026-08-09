//! Compact HMAC-authenticated access tokens for Fluvora service and SDK APIs.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fluvora_bytes_codec::{DecodeError, EncodeError, ReadCursor, WriteBuffer};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const MAGIC: &[u8; 4] = b"FLAT";
const VERSION: u8 = 1;
const CLAIMS_LEN: usize = 57;
const SIGNATURE_LEN: usize = 32;
const TOKEN_BYTES: usize = CLAIMS_LEN + SIGNATURE_LEN;

/// Bitset of capabilities granted to a token.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Scopes(u32);

impl Scopes {
    /// Create rooms.
    pub const ROOM_CREATE: Self = Self(1 << 0);
    /// Join rooms and exchange signaling.
    pub const ROOM_JOIN: Self = Self(1 << 1);
    /// Publish media.
    pub const MEDIA_PUBLISH: Self = Self(1 << 2);
    /// Moderate room roles and membership.
    pub const ROOM_MODERATE: Self = Self(1 << 3);
    /// Record gifts after trusted payment verification.
    pub const GIFT_VERIFY: Self = Self(1 << 4);
    /// Register media-node status.
    pub const NODE_STATUS_WRITE: Self = Self(1 << 5);
    /// Create, upload, transcode, and delete VOD assets.
    pub const VOD_MANAGE: Self = Self(1 << 6);
    /// Create and control live packaging outputs.
    pub const LIVE_MANAGE: Self = Self(1 << 7);
    /// Revoke access tokens.
    pub const TOKEN_REVOKE: Self = Self(1 << 8);

    /// Empty capability set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Returns the union.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether every requested bit is present.
    #[must_use]
    pub const fn contains(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }
}

/// Authenticated access-token claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claims {
    /// Authenticated user or service identifier.
    pub subject: u128,
    /// Optional room restriction; zero means no room restriction.
    pub room_id: u128,
    /// Expiration as Unix milliseconds.
    pub expires_at_millis: u64,
    /// Issuer-provided uniqueness value.
    pub nonce: u64,
    /// Granted capabilities.
    pub scopes: Scopes,
}

/// HMAC token issuer and verifier.
#[derive(Clone)]
pub struct TokenCodec {
    secret: Vec<u8>,
}

/// Bounded rotating token key ring.
///
/// The first key issues new tokens. Verification accepts every configured key, allowing an old
/// key to remain available only for the maximum token lifetime during a rotation.
#[derive(Clone)]
pub struct TokenKeyRing {
    codecs: Vec<TokenCodec>,
}

impl fmt::Debug for TokenKeyRing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenKeyRing")
            .field("keys", &self.codecs.len())
            .finish_non_exhaustive()
    }
}

impl TokenKeyRing {
    /// Creates a key ring containing one through eight secrets, active key first.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/oversized ring or a weak secret.
    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, TokenError> {
        let codecs = secrets
            .into_iter()
            .map(TokenCodec::new)
            .collect::<Result<Vec<_>, _>>()?;
        if codecs.is_empty() || codecs.len() > 8 {
            return Err(TokenError::InvalidKeyCount(codecs.len()));
        }
        Ok(Self { codecs })
    }

    /// Issues with the first (active) key.
    ///
    /// # Errors
    ///
    /// Returns an encoding or signing error.
    pub fn issue(&self, claims: Claims) -> Result<String, TokenError> {
        self.codecs
            .first()
            .ok_or(TokenError::InvalidKeyCount(0))?
            .issue(claims)
    }

    /// Verifies against every configured key.
    ///
    /// # Errors
    ///
    /// Returns expiration when a matching signature is expired, otherwise a verification error.
    pub fn verify(&self, token: &str, now_millis: u64) -> Result<Claims, TokenError> {
        let mut expired = false;
        for codec in &self.codecs {
            match codec.verify(token, now_millis) {
                Ok(claims) => return Ok(claims),
                Err(TokenError::Expired) => expired = true,
                Err(_) => {}
            }
        }
        if expired {
            Err(TokenError::Expired)
        } else {
            Err(TokenError::InvalidSignature)
        }
    }

    /// Returns the number of configured verification keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.codecs.len()
    }

    /// Returns whether no keys are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.codecs.is_empty()
    }
}

impl fmt::Debug for TokenCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenCodec([REDACTED])")
    }
}

impl TokenCodec {
    /// Creates a codec from at least 256 bits of secret key material.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::WeakSecret`] for a key shorter than 32 bytes.
    pub fn new(secret: impl Into<Vec<u8>>) -> Result<Self, TokenError> {
        let secret = secret.into();
        if secret.len() < 32 {
            return Err(TokenError::WeakSecret(secret.len()));
        }
        Ok(Self { secret })
    }

    /// Issues a URL-safe, padding-free token.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] if bounded encoding or HMAC initialization fails.
    pub fn issue(&self, claims: Claims) -> Result<String, TokenError> {
        let mut bytes = encode_claims(claims)?;
        let signature = self.sign(&bytes)?;
        bytes.extend_from_slice(&signature);
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Verifies signature, exact shape, version, and expiration.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] for malformed, forged, or expired tokens.
    pub fn verify(&self, token: &str, now_millis: u64) -> Result<Claims, TokenError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| TokenError::Malformed)?;
        if bytes.len() != TOKEN_BYTES {
            return Err(TokenError::Malformed);
        }
        let (encoded_claims, signature) = bytes.split_at(CLAIMS_LEN);
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| TokenError::InvalidSecret)?;
        mac.update(encoded_claims);
        mac.verify_slice(signature)
            .map_err(|_| TokenError::InvalidSignature)?;
        let claims = decode_claims(encoded_claims)?;
        if now_millis >= claims.expires_at_millis {
            return Err(TokenError::Expired);
        }
        Ok(claims)
    }

    fn sign(&self, bytes: &[u8]) -> Result<[u8; SIGNATURE_LEN], TokenError> {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| TokenError::InvalidSecret)?;
        mac.update(bytes);
        Ok(mac.finalize().into_bytes().into())
    }
}

fn encode_claims(claims: Claims) -> Result<Vec<u8>, TokenError> {
    let mut output = WriteBuffer::with_limit(CLAIMS_LEN);
    output.extend_from_slice(MAGIC)?;
    output.write_u8(VERSION)?;
    write_u128(&mut output, claims.subject)?;
    write_u128(&mut output, claims.room_id)?;
    output.write_u64(claims.expires_at_millis)?;
    output.write_u64(claims.nonce)?;
    output.write_u32(claims.scopes.0)?;
    Ok(output.into_vec())
}

fn decode_claims(input: &[u8]) -> Result<Claims, TokenError> {
    let mut cursor = ReadCursor::new(input);
    if cursor.take(4)? != MAGIC {
        return Err(TokenError::Malformed);
    }
    let version = cursor.read_u8()?;
    if version != VERSION {
        return Err(TokenError::UnsupportedVersion(version));
    }
    let claims = Claims {
        subject: read_u128(&mut cursor)?,
        room_id: read_u128(&mut cursor)?,
        expires_at_millis: cursor.read_u64()?,
        nonce: cursor.read_u64()?,
        scopes: Scopes(cursor.read_u32()?),
    };
    if !cursor.is_empty() {
        return Err(TokenError::Malformed);
    }
    Ok(claims)
}

fn write_u128(output: &mut WriteBuffer, value: u128) -> Result<(), EncodeError> {
    output.write_u64(u64::try_from(value >> 64).unwrap_or_default())?;
    output.write_u64(u64::try_from(value & u128::from(u64::MAX)).unwrap_or_default())?;
    Ok(())
}

fn read_u128(cursor: &mut ReadCursor<'_>) -> Result<u128, DecodeError> {
    Ok((u128::from(cursor.read_u64()?) << 64) | u128::from(cursor.read_u64()?))
}

/// Token issuance or verification failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Secret is shorter than 32 bytes.
    WeakSecret(usize),
    /// A rotating key ring contained zero or more than eight keys.
    InvalidKeyCount(usize),
    /// HMAC rejected key initialization.
    InvalidSecret,
    /// Token is not exact URL-safe fixed-width data.
    Malformed,
    /// Token version is unsupported.
    UnsupportedVersion(u8),
    /// HMAC signature does not verify.
    InvalidSignature,
    /// Expiration is not in the future.
    Expired,
    /// Checked byte read failed.
    Decode(DecodeError),
    /// Bounded byte write failed.
    Encode(EncodeError),
}

impl fmt::Display for TokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WeakSecret(length) => {
                write!(formatter, "token secret is too short: {length} bytes")
            }
            Self::InvalidKeyCount(count) => {
                write!(
                    formatter,
                    "token key ring must contain 1..=8 keys, got {count}"
                )
            }
            Self::InvalidSecret => formatter.write_str("invalid token secret"),
            Self::Malformed => formatter.write_str("malformed access token"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported access token version {version}")
            }
            Self::InvalidSignature => formatter.write_str("invalid access token signature"),
            Self::Expired => formatter.write_str("access token expired"),
            Self::Decode(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TokenError {}

impl From<DecodeError> for TokenError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<EncodeError> for TokenError {
    fn from(value: EncodeError) -> Self {
        Self::Encode(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Claims, Scopes, TokenCodec, TokenError, TokenKeyRing};

    fn codec() -> TokenCodec {
        TokenCodec::new(b"0123456789abcdef0123456789abcdef".to_vec()).expect("strong key")
    }

    #[test]
    fn issues_and_verifies_scoped_token() {
        let claims = Claims {
            subject: u128::MAX,
            room_id: 42,
            expires_at_millis: 10_000,
            nonce: 7,
            scopes: Scopes::ROOM_JOIN.union(Scopes::MEDIA_PUBLISH),
        };
        let token = codec().issue(claims).expect("issue");
        let verified = codec().verify(&token, 9_999).expect("verify");
        assert_eq!(verified, claims);
        assert!(verified.scopes.contains(Scopes::ROOM_JOIN));
        assert!(!verified.scopes.contains(Scopes::GIFT_VERIFY));
    }

    #[test]
    fn rejects_tampering_and_expiration() {
        let claims = Claims {
            subject: 1,
            room_id: 0,
            expires_at_millis: 10,
            nonce: 2,
            scopes: Scopes::empty(),
        };
        let token = codec().issue(claims).expect("issue");
        assert_eq!(codec().verify(&token, 10), Err(TokenError::Expired));
        let mut tampered = token.into_bytes();
        tampered[10] = if tampered[10] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("still ASCII");
        assert_eq!(
            codec().verify(&tampered, 1),
            Err(TokenError::InvalidSignature)
        );
    }

    #[test]
    fn rotates_keys_without_invalidating_existing_tokens() {
        let old = b"old-0123456789abcdef0123456789abcdef".to_vec();
        let new = b"new-0123456789abcdef0123456789abcdef".to_vec();
        let claims = Claims {
            subject: 9,
            room_id: 0,
            expires_at_millis: 50,
            nonce: 11,
            scopes: Scopes::ROOM_CREATE,
        };
        let old_token = TokenCodec::new(old.clone())
            .expect("old")
            .issue(claims)
            .expect("token");
        let ring = TokenKeyRing::new([new, old]).expect("rotating ring");
        assert_eq!(ring.verify(&old_token, 1), Ok(claims));
        let new_token = ring.issue(claims).expect("new token");
        assert_eq!(ring.verify(&new_token, 1), Ok(claims));
        assert_eq!(ring.len(), 2);
    }
}
