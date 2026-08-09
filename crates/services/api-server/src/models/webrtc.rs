use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct OfferRequest {
    pub(crate) sdp: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct OfferResponse {
    pub(crate) session_id: String,
    pub(crate) answer_sdp: String,
}

pub(crate) struct NegotiatedSession {
    pub(crate) session_id: u64,
    pub(crate) answer_sdp: String,
    pub(crate) local_username_fragment: String,
    pub(crate) local_password: String,
    pub(crate) remote_username_fragment: String,
    pub(crate) remote_password: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaSessionProvision {
    pub(crate) session_id: String,
    pub(crate) room_id: String,
    pub(crate) participant_id: String,
    pub(crate) local_username_fragment: String,
    pub(crate) local_password: String,
    pub(crate) remote_username_fragment: String,
    pub(crate) remote_password: String,
    pub(crate) expected_peer_fingerprint: String,
    pub(crate) tie_breaker: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct MediaSessionIceRestart {
    pub(crate) local_username_fragment: String,
    pub(crate) local_password: String,
    pub(crate) remote_username_fragment: String,
    pub(crate) remote_password: String,
    pub(crate) tie_breaker: u64,
}
