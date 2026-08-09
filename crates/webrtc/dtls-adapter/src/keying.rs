use fluvora_srtp::{KeyingMaterial, ProtectionProfile};

use crate::DtlsError;

const MASTER_KEY_LEN: usize = 16;
const MASTER_SALT_LEN: usize = 14;
const EXPORTER_LEN: usize = 2 * (MASTER_KEY_LEN + MASTER_SALT_LEN);

/// DTLS role used to map client/server exporter bytes to inbound/outbound SRTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsRole {
    /// Endpoint sent `ClientHello`.
    Client,
    /// Endpoint sent `ServerHello`.
    Server,
}

/// DTLS `use_srtp` protection profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtlsSrtpProfile {
    /// `SRTP_AES128_CM_SHA1_80`.
    Aes128CmSha1_80,
    /// `SRTP_AES128_CM_SHA1_32`.
    Aes128CmSha1_32,
}

impl DtlsSrtpProfile {
    /// Maps the DTLS extension name.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] for any unimplemented profile.
    pub fn parse_name(name: &str) -> Result<Self, DtlsError> {
        match name {
            "SRTP_AES128_CM_SHA1_80" => Ok(Self::Aes128CmSha1_80),
            "SRTP_AES128_CM_SHA1_32" => Ok(Self::Aes128CmSha1_32),
            _ => Err(DtlsError::UnsupportedSrtpProfile(name.to_owned())),
        }
    }

    /// Maps to the SRTP packet-protection implementation.
    #[must_use]
    pub const fn protection_profile(self) -> ProtectionProfile {
        match self {
            Self::Aes128CmSha1_80 => ProtectionProfile::Aes128CmSha1_80,
            Self::Aes128CmSha1_32 => ProtectionProfile::Aes128CmSha1_32,
        }
    }
}

/// Directional SRTP master material produced by the DTLS exporter.
#[derive(Debug, Clone)]
pub struct DirectionalKeyingMaterial {
    /// Selected packet-protection profile.
    pub profile: ProtectionProfile,
    /// Material used to protect local outbound packets.
    pub outbound: KeyingMaterial,
    /// Material used to authenticate/decrypt remote inbound packets.
    pub inbound: KeyingMaterial,
}

/// Splits RFC 5764 `EXTRACTOR-dtls_srtp` bytes and maps them by endpoint role.
///
/// Exporter order is client master key, server master key, client master salt, server master salt.
///
/// # Errors
///
/// Returns [`DtlsError`] unless exactly 60 bytes are supplied.
pub fn split_srtp_exporter(
    profile: DtlsSrtpProfile,
    local_role: DtlsRole,
    exported: &[u8],
) -> Result<DirectionalKeyingMaterial, DtlsError> {
    if exported.len() != EXPORTER_LEN {
        return Err(DtlsError::InvalidExporterLength {
            expected: EXPORTER_LEN,
            actual: exported.len(),
        });
    }
    let client_key = &exported[..16];
    let server_key = &exported[16..32];
    let client_salt = &exported[32..46];
    let server_salt = &exported[46..60];
    let client = KeyingMaterial::new(client_key, client_salt)?;
    let server = KeyingMaterial::new(server_key, server_salt)?;
    let (outbound, inbound) = match local_role {
        DtlsRole::Client => (client, server),
        DtlsRole::Server => (server, client),
    };
    Ok(DirectionalKeyingMaterial {
        profile: profile.protection_profile(),
        outbound,
        inbound,
    })
}

#[cfg(test)]
mod tests {
    use fluvora_srtp::SrtpContext;

    use super::{DtlsRole, DtlsSrtpProfile, split_srtp_exporter};

    #[test]
    fn maps_client_and_server_directions_symmetrically() {
        let exported: Vec<u8> = (0..60).collect();
        let client = split_srtp_exporter(
            DtlsSrtpProfile::Aes128CmSha1_80,
            DtlsRole::Client,
            &exported,
        )
        .expect("valid exporter");
        let server = split_srtp_exporter(
            DtlsSrtpProfile::Aes128CmSha1_80,
            DtlsRole::Server,
            &exported,
        )
        .expect("valid exporter");

        let _client_context = SrtpContext::new(client.profile, &client.outbound, &client.inbound);
        let _server_context = SrtpContext::new(server.profile, &server.outbound, &server.inbound);
    }
}
