use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateRoomRequest {
    pub(crate) mode: String,
    pub(crate) max_members: Option<usize>,
    pub(crate) max_publishers: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RoomResponse {
    pub(crate) room_id: String,
    pub(crate) mode: String,
    pub(crate) sequence: u64,
    pub(crate) duplicate: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct CommandResponse {
    pub(crate) sequence: u64,
    pub(crate) duplicate: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatRequest {
    pub(crate) message_id: String,
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CustomDataRequest {
    pub(crate) namespace: String,
    pub(crate) schema_version: u16,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoleRequest {
    pub(crate) user_id: String,
    pub(crate) role: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RevokeTokenRequest {
    pub(crate) subject_id: String,
    pub(crate) nonce: u64,
    pub(crate) expires_at_millis: u64,
    pub(crate) reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct RoomSnapshotResponse {
    pub(crate) room_id: String,
    pub(crate) mode: &'static str,
    pub(crate) sequence: u64,
    pub(crate) ended: bool,
    pub(crate) member_count: usize,
    pub(crate) publisher_count: usize,
}
