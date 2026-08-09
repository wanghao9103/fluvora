//! A deterministic, Sans-I/O ICE-lite state machine for Fluvora.
//!
//! The crate owns no sockets and reads no clocks. Callers feed datagrams and monotonic
//! timestamps into [`Agent`] and execute the returned [`Transmit`] actions.

mod agent;
mod config;

pub use agent::{
    Agent, CandidatePair, Event, HandleOutput, IceError, IceState, IntegrityAlgorithm, Transmit,
};
pub use config::{Configuration, CredentialError, Credentials};
