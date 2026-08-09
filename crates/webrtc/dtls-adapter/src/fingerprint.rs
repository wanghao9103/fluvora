use std::fmt;

use sha2::{Digest, Sha256};

use crate::DtlsError;

/// Exact SHA-256 certificate fingerprint exchanged through authenticated SDP.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Fingerprint([u8; 32]);

impl Sha256Fingerprint {
    /// Parses an SDP algorithm and colon-separated fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] unless the algorithm is `sha-256` and exactly 32 hexadecimal octets
    /// are present.
    pub fn parse(algorithm: &str, value: &str) -> Result<Self, DtlsError> {
        if !algorithm.eq_ignore_ascii_case("sha-256") {
            return Err(DtlsError::UnsupportedFingerprintAlgorithm(
                algorithm.to_owned(),
            ));
        }
        let mut bytes = [0_u8; 32];
        let mut parts = value.split(':');
        for byte in &mut bytes {
            let part = parts.next().ok_or(DtlsError::InvalidFingerprint)?;
            if part.len() != 2 {
                return Err(DtlsError::InvalidFingerprint);
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| DtlsError::InvalidFingerprint)?;
        }
        if parts.next().is_some() {
            return Err(DtlsError::InvalidFingerprint);
        }
        Ok(Self(bytes))
    }

    /// Hashes a DER-encoded leaf certificate.
    #[must_use]
    pub fn from_certificate_der(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    /// Returns raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Sha256Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sha256Fingerprint({self})")
    }
}

impl fmt::Display for Sha256Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(":")?;
            }
            write!(formatter, "{byte:02X}")?;
        }
        Ok(())
    }
}
