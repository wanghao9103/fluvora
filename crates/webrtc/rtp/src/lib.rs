//! Strict RTP packet parsing, encoding, and SFU header rewriting.

mod error;
mod extension;
mod packet;
mod sequence;

pub use error::RtpError;
pub use extension::{ExtensionFormat, HeaderExtension, OwnedHeaderExtension};
pub use packet::{
    ExtensionRewrite, Header, Packet, PacketBuilder, Rewrite, parse_header_length,
    rewrite_header_extensions,
};
pub use sequence::{SequenceNumberExtender, TimestampExtender};
