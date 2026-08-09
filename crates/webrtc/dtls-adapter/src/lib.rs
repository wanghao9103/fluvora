//! DTLS-SRTP profile, certificate-fingerprint, and exporter integration.
//!
//! The media transport remains owned by Fluvora. The optional `openssl-backend` feature delegates
//! audited certificate/ECDHE/DTLS cryptography to OpenSSL.

mod error;
mod fingerprint;
mod keying;

#[cfg(feature = "openssl-backend")]
pub mod openssl_backend;

pub use error::DtlsError;
pub use fingerprint::Sha256Fingerprint;
pub use keying::{DirectionalKeyingMaterial, DtlsRole, DtlsSrtpProfile, split_srtp_exporter};
