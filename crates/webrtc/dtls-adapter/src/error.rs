use std::fmt;

/// DTLS configuration, identity, fingerprint, or exporter failure.
#[derive(Debug)]
pub enum DtlsError {
    /// SDP fingerprint algorithm is not SHA-256.
    UnsupportedFingerprintAlgorithm(String),
    /// SHA-256 fingerprint is not 32 colon-separated hexadecimal bytes.
    InvalidFingerprint,
    /// DTLS-SRTP exporter output length does not match the selected profile.
    InvalidExporterLength {
        /// Required bytes.
        expected: usize,
        /// Supplied bytes.
        actual: usize,
    },
    /// DTLS negotiated an unsupported SRTP protection profile.
    UnsupportedSrtpProfile(String),
    /// Peer certificate did not match the authenticated SDP fingerprint.
    FingerprintMismatch,
    /// Peer did not provide a certificate.
    MissingPeerCertificate,
    /// SRTP keying-material construction failed.
    Srtp(fluvora_srtp::SrtpError),
    /// OpenSSL backend failure.
    #[cfg(feature = "openssl-backend")]
    OpenSsl(openssl::error::ErrorStack),
    /// DTLS handshake failed.
    #[cfg(feature = "openssl-backend")]
    Handshake(String),
    /// Datagram I/O failed.
    #[cfg(feature = "openssl-backend")]
    Io(std::io::Error),
}

impl fmt::Display for DtlsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFingerprintAlgorithm(algorithm) => {
                write!(
                    formatter,
                    "unsupported DTLS fingerprint algorithm {algorithm}"
                )
            }
            Self::InvalidFingerprint => formatter.write_str("invalid SHA-256 DTLS fingerprint"),
            Self::InvalidExporterLength { expected, actual } => write!(
                formatter,
                "invalid DTLS-SRTP exporter length: expected {expected}, got {actual}"
            ),
            Self::UnsupportedSrtpProfile(profile) => {
                write!(formatter, "unsupported DTLS-SRTP profile {profile}")
            }
            Self::FingerprintMismatch => {
                formatter.write_str("DTLS certificate fingerprint mismatch")
            }
            Self::MissingPeerCertificate => formatter.write_str("DTLS peer certificate is missing"),
            Self::Srtp(error) => error.fmt(formatter),
            #[cfg(feature = "openssl-backend")]
            Self::OpenSsl(error) => error.fmt(formatter),
            #[cfg(feature = "openssl-backend")]
            Self::Handshake(error) => write!(formatter, "DTLS handshake failed: {error}"),
            #[cfg(feature = "openssl-backend")]
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DtlsError {}

impl From<fluvora_srtp::SrtpError> for DtlsError {
    fn from(value: fluvora_srtp::SrtpError) -> Self {
        Self::Srtp(value)
    }
}

#[cfg(feature = "openssl-backend")]
impl From<openssl::error::ErrorStack> for DtlsError {
    fn from(value: openssl::error::ErrorStack) -> Self {
        Self::OpenSsl(value)
    }
}

#[cfg(feature = "openssl-backend")]
impl From<std::io::Error> for DtlsError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
