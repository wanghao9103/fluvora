//! SRTP and SRTCP protection with replay defense and RFC 3711 key derivation.

mod context;
mod error;
mod keys;
mod replay;

pub use context::SrtpContext;
pub use error::SrtpError;
pub use keys::{KeyingMaterial, ProtectionProfile};
