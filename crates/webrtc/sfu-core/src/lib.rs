//! Deterministic, I/O-free selective forwarding core.

mod down_track;
mod error;
mod model;
mod room;

pub use error::SfuError;
pub use model::{
    ControlOutput, Encoding, ForwardOutcome, ForwardedPacket, Layer, MediaKind, ParticipantId,
    PublishedTrack, RoomConfig, SfuEvent, SubscriptionConfig, SubscriptionId, TrackId,
};
pub use room::Room;
