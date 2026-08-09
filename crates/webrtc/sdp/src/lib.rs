//! A controlled SDP/JSEP parser and answer generator for Fluvora.
//!
//! This crate intentionally implements the WebRTC subset used by Fluvora instead of a general
//! SIP SDP stack. Parsing and semantic validation are separate operations.

mod answer;
mod error;
mod model;
mod parser;

pub use answer::{AnswerConfig, CodecCapability, create_sfu_answer};
pub use error::{SdpError, SdpErrorKind};
pub use model::{
    Attribute, Direction, ExtMap, Fingerprint, MediaDescription, MediaKind, Rid, RtpCodec,
    SessionDescription, SetupRole,
};
