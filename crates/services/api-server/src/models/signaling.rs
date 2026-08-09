use super::state::SignalRecord;
use fluvora_domain::RoomId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct SignalRequest {
    pub(crate) to: Option<String>,
    pub(crate) kind: String,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SignalQuery {
    pub(crate) after: Option<u64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventQuery {
    pub(crate) after: Option<u64>,
    pub(crate) limit: Option<usize>,
    pub(crate) ticket: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EventTicket {
    pub(crate) room_id: RoomId,
    pub(crate) participant: u128,
    pub(crate) expires_at_millis: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct EventTicketResponse {
    pub(crate) ticket: String,
    pub(crate) expires_at_millis: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct IceServer {
    pub(crate) urls: Vec<String>,
    pub(crate) username: String,
    pub(crate) credential: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct IceServersResponse {
    pub(crate) ice_servers: Vec<IceServer>,
    pub(crate) expires_at_millis: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SignalResponse {
    pub(crate) signals: Vec<SignalRecord>,
    pub(crate) latest_sequence: u64,
}
