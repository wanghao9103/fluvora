use std::fmt;

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::SrtpError;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

/// DTLS-SRTP protection profile supported by the AES-CM context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionProfile {
    /// `SRTP_AES128_CM_HMAC_SHA1_80`.
    Aes128CmSha1_80,
    /// `SRTP_AES128_CM_HMAC_SHA1_32`; SRTCP still uses an 80-bit tag.
    Aes128CmSha1_32,
}

impl ProtectionProfile {
    pub(crate) const fn rtp_tag_len(self) -> usize {
        match self {
            Self::Aes128CmSha1_80 => 10,
            Self::Aes128CmSha1_32 => 4,
        }
    }

    pub(crate) const fn rtcp_tag_len(self) -> usize {
        match self {
            Self::Aes128CmSha1_80 | Self::Aes128CmSha1_32 => 10,
        }
    }
}

/// One direction of DTLS-exported SRTP master keying material.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct KeyingMaterial {
    master_key: [u8; 16],
    master_salt: [u8; 14],
}

impl KeyingMaterial {
    /// Copies validated AES-128 master key and 112-bit master salt bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SrtpError`] when either slice has the wrong profile-specific length.
    pub fn new(master_key: &[u8], master_salt: &[u8]) -> Result<Self, SrtpError> {
        let master_key = master_key
            .try_into()
            .map_err(|_| SrtpError::InvalidMasterKeyLength(master_key.len()))?;
        let master_salt = master_salt
            .try_into()
            .map_err(|_| SrtpError::InvalidMasterSaltLength(master_salt.len()))?;
        Ok(Self {
            master_key,
            master_salt,
        })
    }
}

impl fmt::Debug for KeyingMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KeyingMaterial([REDACTED])")
    }
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(crate) struct SessionKeys {
    pub encryption: [u8; 16],
    pub authentication: [u8; 20],
    pub salt: [u8; 14],
}

impl fmt::Debug for SessionKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionKeys([REDACTED])")
    }
}

impl KeyingMaterial {
    pub(crate) fn derive_srtp(&self) -> SessionKeys {
        self.derive(0, 1, 2)
    }

    pub(crate) fn derive_srtcp(&self) -> SessionKeys {
        self.derive(3, 4, 5)
    }

    fn derive(
        &self,
        encryption_label: u8,
        authentication_label: u8,
        salt_label: u8,
    ) -> SessionKeys {
        SessionKeys {
            encryption: derive_key::<16>(self, encryption_label),
            authentication: derive_key::<20>(self, authentication_label),
            salt: derive_key::<14>(self, salt_label),
        }
    }
}

fn derive_key<const LENGTH: usize>(material: &KeyingMaterial, label: u8) -> [u8; LENGTH] {
    let mut iv = [0_u8; 16];
    iv[..14].copy_from_slice(&material.master_salt);
    iv[7] ^= label;
    let mut output = [0_u8; LENGTH];
    let mut cipher = Aes128Ctr::new((&material.master_key).into(), (&iv).into());
    cipher.apply_keystream(&mut output);
    iv.zeroize();
    output
}

#[cfg(test)]
mod tests {
    use super::KeyingMaterial;

    #[test]
    fn matches_rfc_3711_key_derivation_vector() {
        let material = KeyingMaterial::new(
            &[
                0xe1, 0xf9, 0x7a, 0x0d, 0x3e, 0x01, 0x8b, 0xe0, 0xd6, 0x4f, 0xa3, 0x2c, 0x06, 0xde,
                0x41, 0x39,
            ],
            &[
                0x0e, 0xc6, 0x75, 0xad, 0x49, 0x8a, 0xfe, 0xeb, 0xb6, 0x96, 0x0b, 0x3a, 0xab, 0xe6,
            ],
        )
        .expect("valid vector");
        let keys = material.derive_srtp();

        assert_eq!(
            keys.encryption,
            [
                0xc6, 0x1e, 0x7a, 0x93, 0x74, 0x4f, 0x39, 0xee, 0x10, 0x73, 0x4a, 0xfe, 0x3f, 0xf7,
                0xa0, 0x87,
            ]
        );
        assert_eq!(
            keys.authentication,
            [
                0xce, 0xbe, 0x32, 0x1f, 0x6f, 0xf7, 0x71, 0x6b, 0x6f, 0xd4, 0xab, 0x49, 0xaf, 0x25,
                0x6a, 0x15, 0x6d, 0x38, 0xba, 0xa4,
            ]
        );
        assert_eq!(
            keys.salt,
            [
                0x30, 0xcb, 0xbc, 0x08, 0x86, 0x3d, 0x8c, 0x85, 0xd4, 0x9d, 0xb3, 0x4a, 0x9a, 0xe1,
            ]
        );
    }
}
