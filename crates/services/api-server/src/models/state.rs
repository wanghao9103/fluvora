use super::media::TrackEncodingRequest;
use super::signaling::EventTicket;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use fluvora_auth::TokenKeyRing;
use fluvora_domain::{CommandId, RoomId};
use fluvora_observability::MediaNodeMetrics;
use fluvora_transcode_bridge::{
    Coordinator as TranscodeCoordinator, JobId as TranscodeJobId, MediaCodec,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::persistence::{ManagedRoom, RoomPersistence};
use crate::protocol::ProtocolSession;

#[derive(Debug, Clone)]
pub(crate) struct RegisteredTrack {
    pub(crate) participant: u128,
    pub(crate) kind: String,
    pub(crate) codec: MediaCodec,
    pub(crate) codec_name: String,
    pub(crate) clock_rate: u32,
    pub(crate) payload_type: u8,
    pub(crate) encodings: Vec<TrackEncodingRequest>,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) frames_per_second: u16,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegisteredSubscription {
    pub(crate) source_track_id: u64,
}

pub(crate) type SubscriptionKey = (RoomId, u128, u64);
pub(crate) type SubscriptionCatalog = HashMap<SubscriptionKey, RegisteredSubscription>;

#[derive(Debug, Clone)]
pub(crate) struct ActiveTranscode {
    pub(crate) room_id: RoomId,
    pub(crate) worker_job_id: u64,
    pub(crate) worker_endpoint: String,
    pub(crate) worker_placement_id: String,
    pub(crate) worker_placement_generation: u64,
    pub(crate) ingress_id: u64,
    pub(crate) output_track_id: u64,
    pub(crate) output_ssrc: u32,
    pub(crate) output_codec: MediaCodec,
    pub(crate) source_track_id: u64,
    pub(crate) source_destination: SocketAddr,
    pub(crate) output_destination: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct TranscodeRegistry {
    pub(crate) coordinator: TranscodeCoordinator,
    pub(crate) active: HashMap<TranscodeJobId, ActiveTranscode>,
    pub(crate) subscriptions: HashMap<(RoomId, u128, u64), TranscodeJobId>,
    pub(crate) health_failures: HashMap<TranscodeJobId, u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SignalRecord {
    #[serde(skip)]
    pub(crate) command_id: Option<CommandId>,
    pub(crate) sequence: u64,
    pub(crate) from: String,
    pub(crate) to: Option<String>,
    pub(crate) kind: String,
    pub(crate) payload: serde_json::Value,
    pub(crate) timestamp_millis: u64,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) rooms: Arc<RwLock<HashMap<RoomId, ManagedRoom>>>,
    pub(crate) room_creations: Arc<RwLock<HashMap<CommandId, RoomId>>>,
    pub(crate) event_channels: Arc<RwLock<HashMap<RoomId, broadcast::Sender<SignalRecord>>>>,
    pub(crate) event_tickets: Arc<RwLock<HashMap<String, EventTicket>>>,
    pub(crate) protocol_sessions: Arc<RwLock<HashMap<u64, ProtocolSession>>>,
    pub(crate) protocol_updates: Arc<AsyncMutex<()>>,
    pub(crate) tracks: Arc<RwLock<HashMap<(RoomId, u64), RegisteredTrack>>>,
    pub(crate) subscriptions: Arc<RwLock<SubscriptionCatalog>>,
    pub(crate) transcodes: Arc<AsyncMutex<TranscodeRegistry>>,
    pub(crate) persistence: Arc<RoomPersistence>,
    pub(crate) room_mutations: Arc<AsyncMutex<()>>,
    pub(crate) region: Arc<str>,
    pub(crate) placement_stale_after: std::time::Duration,
    pub(crate) tokens: Arc<TokenKeyRing>,
    pub(crate) metrics: Arc<MediaNodeMetrics>,
    pub(crate) dtls_fingerprint: Arc<str>,
    pub(crate) candidate: Option<Arc<str>>,
    pub(crate) media_control_url: Arc<str>,
    pub(crate) media_control_token: Arc<str>,
    pub(crate) gateway_control_url: Arc<str>,
    pub(crate) gateway_control_token: Arc<str>,
    pub(crate) worker_control_url: Arc<str>,
    pub(crate) worker_control_token: Arc<str>,
    pub(crate) ice_urls: Arc<Vec<String>>,
    pub(crate) turn_rest_secret: Arc<[u8]>,
    pub(crate) http_client: reqwest::Client,
    pub(crate) event_bus_ready: Arc<AtomicBool>,
    pub(crate) revoked_tokens: Arc<RwLock<HashMap<(u128, u64), u64>>>,
    pub(crate) gift_webhook_secret: Arc<[u8]>,
}
