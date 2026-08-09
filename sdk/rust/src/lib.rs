//! Rust SDK for Fluvora room control, interaction data, and WebRTC offer/answer signaling.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Reliable ordered room `DataChannel` label shared by every SDK.
pub const ROOM_DATA_CHANNEL_LABEL: &str = "fluvora.room.v1";
/// WebRTC `DataChannel` subprotocol shared by every SDK.
pub const ROOM_DATA_CHANNEL_PROTOCOL: &str = "fluvora.v1";
const MAX_JSON_RESPONSE_BYTES: usize = 32 * 1_024 * 1_024;
const MAX_ERROR_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_JSON_REQUEST_BYTES: usize = 1_024 * 1_024;
const MAX_CHAT_BYTES: usize = 4_096;
const MAX_CUSTOM_PAYLOAD_BYTES: usize = 60 * 1_024;
const MAX_SIGNAL_PAYLOAD_BYTES: usize = 64 * 1_024;
const MAX_SDP_BYTES: usize = 256 * 1_024;
const MAX_MEDIA_UPLOAD_BYTES: usize = 8 * 1_024 * 1_024;
const SIGNAL_PAGE_MESSAGES: usize = 128;

/// Boxed asynchronous operation used by WebRTC implementation adapters.
pub type WebRtcFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SdkError>> + Send + 'a>>;
/// Owned async callback result used by [`CallbackWebRtcPeer`].
pub type WebRtcOwnedFuture<T> = Pin<Box<dyn Future<Output = Result<T, SdkError>> + Send + 'static>>;

/// Adapter implemented by a platform WebRTC peer connection.
///
/// Fluvora remains compatible with browser/native WebRTC while the SDK avoids imposing a specific
/// Rust peer-connection library on applications.
pub trait WebRtcPeer: Send {
    /// Creates the reliable ordered Fluvora room `DataChannel` before SDP offer generation.
    ///
    /// Adapters without `DataChannel` support can retain this default; media negotiation still works.
    fn prepare_room_data_channel(&mut self) -> WebRtcFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    /// Creates and locally applies a complete SDP offer.
    fn create_offer(&mut self) -> WebRtcFuture<'_, String>;

    /// Applies the server's SDP answer.
    fn set_remote_answer(&mut self, answer_sdp: String) -> WebRtcFuture<'_, ()>;
}

/// Dependency-neutral adapter for an application's existing WebRTC peer connection.
///
/// This keeps the SDK independent of a particular native WebRTC binary while avoiding a bespoke
/// trait implementation at every integration site. Closures normally capture an `Arc` to the
/// platform peer and bridge its callback or async API.
pub struct CallbackWebRtcPeer {
    prepare_data_channel: Box<dyn FnMut() -> WebRtcOwnedFuture<()> + Send>,
    create_offer: Box<dyn FnMut() -> WebRtcOwnedFuture<String> + Send>,
    set_remote_answer: Box<dyn FnMut(String) -> WebRtcOwnedFuture<()> + Send>,
}

impl fmt::Debug for CallbackWebRtcPeer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CallbackWebRtcPeer")
    }
}

impl CallbackWebRtcPeer {
    /// Creates a media-only adapter from offer and answer callbacks.
    pub fn new(
        create_offer: impl FnMut() -> WebRtcOwnedFuture<String> + Send + 'static,
        set_remote_answer: impl FnMut(String) -> WebRtcOwnedFuture<()> + Send + 'static,
    ) -> Self {
        Self {
            prepare_data_channel: Box::new(|| Box::pin(async { Ok(()) })),
            create_offer: Box::new(create_offer),
            set_remote_answer: Box::new(set_remote_answer),
        }
    }

    /// Adds the callback that creates the standard reliable ordered room `DataChannel`.
    #[must_use]
    pub fn with_room_data_channel(
        mut self,
        prepare_data_channel: impl FnMut() -> WebRtcOwnedFuture<()> + Send + 'static,
    ) -> Self {
        self.prepare_data_channel = Box::new(prepare_data_channel);
        self
    }
}

impl WebRtcPeer for CallbackWebRtcPeer {
    fn prepare_room_data_channel(&mut self) -> WebRtcFuture<'_, ()> {
        (self.prepare_data_channel)()
    }

    fn create_offer(&mut self) -> WebRtcFuture<'_, String> {
        (self.create_offer)()
    }

    fn set_remote_answer(&mut self, answer_sdp: String) -> WebRtcFuture<'_, ()> {
        (self.set_remote_answer)(answer_sdp)
    }
}

/// Room topology.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomMode {
    /// Selective forwarding.
    Sfu,
    /// Direct peer-to-peer media.
    P2p,
    /// Host-to-audience live room.
    Live,
    /// Stored playback room.
    Vod,
}

/// Room creation response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Room {
    /// Hexadecimal room identifier.
    #[serde(rename = "room_id")]
    pub id: String,
    /// Room topology.
    pub mode: RoomMode,
    /// Event sequence.
    pub sequence: u64,
    /// Whether the idempotency key had already been applied.
    pub duplicate: bool,
}

/// Current room counters and lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomSnapshot {
    /// Hexadecimal room identifier.
    pub room_id: String,
    /// Room topology.
    pub mode: RoomMode,
    /// Latest durable event sequence.
    pub sequence: u64,
    /// Whether the room is terminal.
    pub ended: bool,
    /// Current member count.
    pub member_count: usize,
    /// Current publishing-member count.
    pub publisher_count: usize,
}

/// Room member authorization role.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    /// Room owner.
    Host,
    /// Active media publisher.
    Publisher,
    /// Receive-only audience member.
    Viewer,
}

/// Idempotent room command result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandResult {
    /// Event sequence.
    pub sequence: u64,
    /// Whether this command was already present.
    pub duplicate: bool,
}

/// Trusted payment result accepted by the gift ledger endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifiedGift {
    /// Trusted payment-provider identifier.
    pub provider: String,
    /// Provider verification time as Unix milliseconds.
    pub provider_timestamp_millis: u64,
    /// Base64url-no-pad HMAC-SHA256 receipt signature.
    pub provider_signature: String,
    /// Paying participant.
    pub sender_id: String,
    /// Gift recipient.
    pub recipient_id: String,
    /// Payment-provider transaction identifier.
    pub transaction_id: String,
    /// Catalog gift identifier.
    pub gift_id: String,
    /// Number of gifts.
    pub quantity: u32,
    /// Value of one gift in the smallest currency unit.
    pub unit_value: u64,
    /// Three-letter uppercase currency.
    pub currency: String,
}

/// Completed server-side WebRTC negotiation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebRtcSession {
    /// Media-node session identifier.
    pub session_id: String,
    /// SDP answer.
    pub answer_sdp: String,
}

/// P2P signaling record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Signal {
    /// Monotonic room signal sequence.
    #[serde(default)]
    pub sequence: u64,
    /// Sender identifier.
    #[serde(default)]
    pub from: String,
    /// Optional recipient identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Offer, answer, ICE candidate/restart, renegotiate, or bye.
    pub kind: String,
    /// JSON signaling payload.
    pub payload: serde_json::Value,
    /// Server timestamp.
    #[serde(default)]
    pub timestamp_millis: u64,
}

/// One published simulcast/SVC encoding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackEncoding {
    /// Publisher SSRC.
    pub ssrc: u32,
    /// Optional negotiated RID.
    pub rid: Option<String>,
    /// Zero-based spatial layer.
    pub spatial_layer: u8,
    /// Declared bitrate ceiling.
    pub max_bitrate_bps: u64,
}

/// Per-subscriber RFC 8285 header-extension transformation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderExtensionRewrite {
    /// Publisher extension identifier.
    pub source_id: u8,
    /// Subscriber extension identifier, or `None` to remove it.
    pub destination_id: Option<u8>,
    /// Optional replacement bytes, used for subscriber-specific MID values.
    pub replacement: Option<Vec<u8>>,
}

/// Track registration sent after WebRTC negotiation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishTrack {
    /// Application-allocated track identifier.
    pub track_id: u64,
    /// `audio` or `video`.
    pub kind: String,
    /// `opus`, `vp8`, `vp9`, `h264`, or `av1`.
    pub codec: String,
    /// RTP clock rate.
    pub clock_rate: u32,
    /// Publisher-negotiated payload type.
    pub payload_type: u8,
    /// Published encodings.
    pub encodings: Vec<TrackEncoding>,
    /// Source width for bounded realtime transcoding; zero for audio or unknown.
    #[serde(default)]
    pub width: u16,
    /// Source height for bounded realtime transcoding; zero for audio or unknown.
    #[serde(default)]
    pub height: u16,
    /// Source frame rate; zero for audio or unknown.
    #[serde(default)]
    pub frames_per_second: u16,
}

/// Subscriber down-track registration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscribeTrack {
    /// Application-allocated subscription identifier.
    pub subscription_id: u64,
    /// Published track identifier.
    pub track_id: u64,
    /// Subscriber-visible SSRC.
    pub output_ssrc: u32,
    /// Subscriber-negotiated payload type.
    pub output_payload_type: u8,
    /// Initial spatial layer.
    pub spatial_layer: u8,
    /// Initial temporal layer.
    pub temporal_layer: u8,
    /// First output sequence number.
    pub initial_sequence_number: u16,
    /// First output timestamp.
    pub initial_timestamp: u32,
    /// MID/RID/TWCC extension mappings.
    #[serde(default)]
    pub extension_rewrites: Vec<HeaderExtensionRewrite>,
    /// Subscriber-side transport-wide sequence extension identifier.
    pub transport_wide_extension_id: Option<u8>,
    /// Supported decoder codecs in preference order; empty selects direct forwarding.
    #[serde(default)]
    pub subscriber_codecs: Vec<String>,
    /// Whether a shared server-side encoder may be allocated.
    #[serde(default)]
    pub allow_transcoding: bool,
    /// `good`, `constrained`, or `critical`.
    pub network_quality: Option<String>,
    /// Buffered HLS fallback URL for persistently unusable realtime paths.
    pub hls_fallback_url: Option<String>,
    /// Requested output width ceiling.
    pub target_width: Option<u16>,
    /// Requested output height ceiling.
    pub target_height: Option<u16>,
    /// Requested output frame-rate ceiling.
    pub target_frames_per_second: Option<u16>,
    /// Requested output bitrate.
    pub target_bitrate_bps: Option<u64>,
}

/// Server-selected direct, shared-transcode, or buffered fallback path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscribeTrackResult {
    /// `direct`, `transcode`, `hls`, or `existing`.
    pub path: String,
    /// Original source track.
    pub source_track_id: u64,
    /// Actual SFU track, absent for HLS fallback.
    pub selected_track_id: Option<u64>,
    /// Selected encoded codec.
    pub codec: Option<String>,
    /// Shared transcode allocation identifier.
    pub transcode_job_id: Option<u64>,
    /// HLS URL when realtime playback is replaced.
    pub fallback_url: Option<String>,
}

/// One VOD adaptive-bitrate rendition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rendition {
    /// Maximum output width.
    pub width: u16,
    /// Maximum output height.
    pub height: u16,
    /// Video bitrate.
    pub video_bitrate_bps: u64,
    /// Audio bitrate.
    pub audio_bitrate_bps: u32,
}

/// VOD asset lifecycle exposed by the control API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VodAssetState {
    /// Metadata exists.
    Created,
    /// Resumable upload is active.
    Uploading,
    /// Immutable source upload is complete.
    Uploaded,
    /// Media probe is active.
    Probing,
    /// Renditions are being generated.
    Transcoding,
    /// HLS output is published.
    Ready,
    /// Processing failed.
    Failed,
    /// Deletion is in progress.
    Deleting,
    /// Tombstone state.
    Deleted,
}

/// VOD asset status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VodAsset {
    /// Asset identifier.
    pub asset_id: String,
    /// Owning tenant identifier.
    pub tenant_id: String,
    /// Optimistic lifecycle version.
    pub version: u64,
    /// Current lifecycle.
    pub state: VodAssetState,
    /// Uploaded bytes for an active upload.
    pub received_bytes: Option<u64>,
    /// Final source size.
    pub source_bytes: Option<u64>,
    /// Absolute HLS manifest URL when ready.
    pub manifest_url: Option<String>,
    /// Asset duration when ready.
    pub duration_millis: Option<u64>,
    /// Bounded worker failure reason.
    pub failure_reason: Option<String>,
    /// Whether a failed asset can be retried.
    pub retryable: Option<bool>,
    /// Worker job identifier.
    pub job_id: Option<u64>,
}

/// Live HLS/CMAF output status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveOutput {
    /// Stream identifier.
    pub stream_id: String,
    /// Next expected media sequence.
    pub next_sequence: u64,
    /// Absolute HLS manifest URL.
    pub manifest_url: String,
    /// Live worker job for RTP-backed outputs.
    pub worker_job_id: Option<u64>,
}

/// One SFU publisher track routed into live packaging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSourceTrack {
    /// Source room hexadecimal identifier.
    pub room_id: String,
    /// Published SFU track identifier.
    pub track_id: u64,
    /// `audio` or `video`.
    pub kind: String,
    /// RTP codec name.
    pub codec: String,
    /// RTP payload type.
    pub payload_type: u8,
    /// RTP clock rate.
    pub clock_rate: u32,
    /// Audio channel count.
    pub channels: Option<u8>,
    /// Negotiated RTP format parameters.
    pub fmtp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SignalPage {
    signals: Vec<Signal>,
    latest_sequence: u64,
}

/// One-time credential for opening a room event WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventTicket {
    /// Opaque single-use ticket.
    pub ticket: String,
    /// Ticket expiration as Unix milliseconds.
    pub expires_at_millis: u64,
}

/// One STUN/TURN entry suitable for a native WebRTC peer connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IceServer {
    /// STUN/TURN URLs, including transport query parameters.
    pub urls: Vec<String>,
    /// Time-limited TURN REST username.
    pub username: String,
    /// Time-limited TURN REST password.
    pub credential: String,
}

/// Room-scoped ICE configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IceConfiguration {
    /// Ordered ICE servers.
    pub ice_servers: Vec<IceServer>,
    /// Credential expiry as Unix milliseconds.
    pub expires_at_millis: u64,
}

#[derive(Debug, Serialize)]
struct CreateRoomRequest {
    mode: RoomMode,
    max_members: Option<usize>,
    max_publishers: Option<usize>,
}

#[derive(Debug, Serialize)]
struct OfferRequest<'a> {
    sdp: &'a str,
}

#[derive(Debug, Serialize)]
struct PostSignalRequest {
    to: Option<String>,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: Option<String>,
    message: Option<String>,
}

/// Thread-safe Fluvora API client.
#[derive(Debug, Clone)]
pub struct Client {
    base_url: Arc<str>,
    token: Arc<RwLock<String>>,
    http: reqwest::Client,
}

impl Client {
    /// Creates a client with a short-lived access token.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTP base URLs and empty tokens.
    pub fn new(
        base_url: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Result<Self, SdkError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let parsed = reqwest::Url::parse(&base_url).map_err(|_| SdkError::InvalidBaseUrl)?;
        if base_url.is_empty()
            || base_url.len() > 2_048
            || base_url.bytes().any(|byte| byte.is_ascii_control())
            || !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(SdkError::InvalidBaseUrl);
        }
        let token = access_token.into();
        if !valid_access_token(&token) {
            return Err(SdkError::EmptyAccessToken);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| SdkError::HttpClientConfiguration)?;
        Ok(Self {
            base_url: Arc::from(base_url),
            token: Arc::new(RwLock::new(token)),
            http,
        })
    }

    /// Atomically replaces the access token after application refresh.
    ///
    /// # Errors
    ///
    /// Rejects empty tokens.
    pub fn set_access_token(&self, token: impl Into<String>) -> Result<(), SdkError> {
        let token = token.into();
        if !valid_access_token(&token) {
            return Err(SdkError::EmptyAccessToken);
        }
        *self
            .token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
        Ok(())
    }

    /// Creates an idempotent room.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for random generation, transport, HTTP, or JSON failures.
    pub async fn create_room(
        &self,
        mode: RoomMode,
        max_members: Option<usize>,
        max_publishers: Option<usize>,
    ) -> Result<Room, SdkError> {
        self.request(
            Method::POST,
            "/v1/rooms",
            Some(&CreateRoomRequest {
                mode,
                max_members,
                max_publishers,
            }),
            true,
        )
        .await
    }

    /// Reads the current room snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for an invalid room ID or API failure.
    pub async fn get_room(&self, room_id: &str) -> Result<RoomSnapshot, SdkError> {
        validate_id(room_id)?;
        self.request::<(), _>(Method::GET, &format!("/v1/rooms/{room_id}"), None, false)
            .await
    }

    /// Joins a room.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] when the command is rejected or cannot be delivered.
    pub async fn join(&self, room_id: &str) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        self.request::<(), _>(
            Method::POST,
            &format!("/v1/rooms/{room_id}/join"),
            None,
            true,
        )
        .await
    }

    /// Leaves a room and releases this participant's media resources.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] when the command is rejected or cannot be delivered.
    pub async fn leave(&self, room_id: &str) -> Result<CommandResult, SdkError> {
        self.room_command(room_id, "leave").await
    }

    /// Permanently ends a room as its host.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] when the command is rejected or cannot be delivered.
    pub async fn end(&self, room_id: &str) -> Result<CommandResult, SdkError> {
        self.room_command(room_id, "end").await
    }

    /// Marks the authenticated participant as an active publisher.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] when the command is rejected or cannot be delivered.
    pub async fn start_publishing(&self, room_id: &str) -> Result<CommandResult, SdkError> {
        self.room_command(room_id, "publish/start").await
    }

    /// Stops publishing and triggers track/session cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] when the command is rejected or cannot be delivered.
    pub async fn stop_publishing(&self, room_id: &str) -> Result<CommandResult, SdkError> {
        self.room_command(room_id, "publish/stop").await
    }

    /// Assigns a room role as a moderator.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or API rejection.
    pub async fn set_role(
        &self,
        room_id: &str,
        user_id: &str,
        role: MemberRole,
    ) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        validate_id(user_id)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/roles"),
            Some(&serde_json::json!({"user_id": user_id, "role": role})),
            true,
        )
        .await
    }

    async fn room_command(
        &self,
        room_id: &str,
        operation: &str,
    ) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        self.request::<(), _>(
            Method::POST,
            &format!("/v1/rooms/{room_id}/{operation}"),
            None,
            true,
        )
        .await
    }

    /// Sends chat through the durable room command path.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or API failure.
    pub async fn send_chat(
        &self,
        room_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        validate_id(message_id)?;
        if text.is_empty() {
            return Err(SdkError::EmptyChatMessage);
        }
        validate_payload_size("chat message", text.len(), MAX_CHAT_BYTES)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/chat"),
            Some(&serde_json::json!({"message_id": message_id, "text": text})),
            true,
        )
        .await
    }

    /// Sends versioned application JSON through the durable room command path.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or API failure.
    pub async fn send_custom_data(
        &self,
        room_id: &str,
        namespace: &str,
        schema_version: u16,
        payload: serde_json::Value,
    ) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        validate_custom_namespace(namespace)?;
        validate_json_size("custom payload", &payload, MAX_CUSTOM_PAYLOAD_BYTES)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/custom"),
            Some(&serde_json::json!({
                "namespace": namespace,
                "schema_version": schema_version,
                "payload": payload
            })),
            true,
        )
        .await
    }

    /// Records a gift after a trusted payment service has verified it.
    ///
    /// This requires the `gift_verify` scope and must not be exposed directly to an untrusted app.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid participant IDs or API failure.
    pub async fn record_verified_gift(
        &self,
        room_id: &str,
        gift: &VerifiedGift,
    ) -> Result<CommandResult, SdkError> {
        validate_id(room_id)?;
        validate_id(&gift.sender_id)?;
        validate_id(&gift.recipient_id)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/gifts"),
            Some(gift),
            true,
        )
        .await
    }

    /// Exchanges an SDP offer and applies the answer through a user-provided WebRTC adapter.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] from the adapter or signaling API.
    pub async fn connect_sfu(
        &self,
        room_id: &str,
        peer: &mut impl WebRtcPeer,
    ) -> Result<WebRtcSession, SdkError> {
        validate_id(room_id)?;
        peer.prepare_room_data_channel().await?;
        let offer = peer.create_offer().await?;
        validate_payload_size("SDP offer", offer.len(), MAX_SDP_BYTES)?;
        let session: WebRtcSession = self
            .request(
                Method::POST,
                &format!("/v1/rooms/{room_id}/webrtc/offer"),
                Some(&OfferRequest { sdp: &offer }),
                false,
            )
            .await?;
        peer.set_remote_answer(session.answer_sdp.clone()).await?;
        Ok(session)
    }

    /// Exchanges a caller-created WebRTC offer for the media-node answer.
    ///
    /// This lower-level method is intended for C, mobile, and engine bindings whose native
    /// peer-connection object cannot implement a Rust trait.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or signaling API failure.
    pub async fn exchange_offer(
        &self,
        room_id: &str,
        offer_sdp: &str,
    ) -> Result<WebRtcSession, SdkError> {
        validate_id(room_id)?;
        validate_payload_size("SDP offer", offer_sdp.len(), MAX_SDP_BYTES)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/webrtc/offer"),
            Some(&OfferRequest { sdp: offer_sdp }),
            false,
        )
        .await
    }

    /// Posts one P2P signaling message.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or signaling rejection.
    pub async fn post_signal(
        &self,
        room_id: &str,
        to: Option<String>,
        kind: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<Signal, SdkError> {
        validate_id(room_id)?;
        if let Some(recipient) = &to {
            validate_id(recipient)?;
        }
        let kind = kind.into();
        if !matches!(
            kind.as_str(),
            "offer" | "answer" | "ice-candidate" | "ice-restart" | "renegotiate" | "bye"
        ) {
            return Err(SdkError::InvalidSignalKind);
        }
        validate_json_size("signal payload", &payload, MAX_SIGNAL_PAYLOAD_BYTES)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/signals"),
            Some(&PostSignalRequest { to, kind, payload }),
            true,
        )
        .await
    }

    /// Polls the bounded P2P signaling backlog.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs or API failure.
    pub async fn poll_signals(
        &self,
        room_id: &str,
        after: u64,
    ) -> Result<(Vec<Signal>, u64), SdkError> {
        validate_id(room_id)?;
        let page: SignalPage = self
            .request::<(), _>(
                Method::GET,
                &format!("/v1/rooms/{room_id}/signals?after={after}&limit={SIGNAL_PAGE_MESSAGES}"),
                None,
                false,
            )
            .await?;
        Ok((page.signals, page.latest_sequence))
    }

    /// Issues a short-lived, single-use WebSocket ticket.
    ///
    /// This avoids placing a reusable API access token in WebSocket URLs.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or API rejection.
    pub async fn issue_event_ticket(&self, room_id: &str) -> Result<EventTicket, SdkError> {
        validate_id(room_id)?;
        self.request::<(), _>(
            Method::POST,
            &format!("/v1/rooms/{room_id}/events/tickets"),
            None,
            false,
        )
        .await
    }

    /// Retrieves room-scoped STUN/TURN configuration with expiring credentials.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or API rejection.
    pub async fn get_ice_configuration(&self, room_id: &str) -> Result<IceConfiguration, SdkError> {
        validate_id(room_id)?;
        self.request::<(), _>(
            Method::GET,
            &format!("/v1/rooms/{room_id}/ice-servers"),
            None,
            false,
        )
        .await
    }

    /// Registers a publisher RTP track with the SFU.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or rejected media configuration.
    pub async fn publish_track(&self, room_id: &str, track: &PublishTrack) -> Result<(), SdkError> {
        validate_id(room_id)?;
        self.request_no_content(
            Method::POST,
            &format!("/v1/rooms/{room_id}/tracks"),
            Some(track),
            true,
        )
        .await
    }

    /// Removes a publisher source track and any dependent down-tracks/transcoders.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs, unknown tracks, or ownership violations.
    pub async fn unpublish_track(&self, room_id: &str, track_id: u64) -> Result<(), SdkError> {
        validate_id(room_id)?;
        self.request_no_content::<()>(
            Method::DELETE,
            &format!("/v1/rooms/{room_id}/tracks/{track_id}"),
            None,
            false,
        )
        .await
    }

    /// Creates one independently negotiated SFU down-track.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or rejected subscription parameters.
    pub async fn subscribe_track(
        &self,
        room_id: &str,
        subscription: &SubscribeTrack,
    ) -> Result<SubscribeTrackResult, SdkError> {
        validate_id(room_id)?;
        self.request(
            Method::POST,
            &format!("/v1/rooms/{room_id}/subscriptions"),
            Some(subscription),
            true,
        )
        .await
    }

    /// Removes a down-track and releases its shared transcoder reference, if any.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or unknown subscriptions.
    pub async fn unsubscribe_track(
        &self,
        room_id: &str,
        subscription_id: u64,
    ) -> Result<(), SdkError> {
        validate_id(room_id)?;
        self.request_no_content::<()>(
            Method::DELETE,
            &format!("/v1/rooms/{room_id}/subscriptions/{subscription_id}"),
            None,
            false,
        )
        .await
    }

    /// Requests a keyframe-safe adaptive layer change.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid room IDs or unknown subscriptions/layers.
    pub async fn set_subscription_layer(
        &self,
        room_id: &str,
        subscription_id: u64,
        spatial_layer: u8,
        temporal_layer: u8,
    ) -> Result<(), SdkError> {
        validate_id(room_id)?;
        self.request_no_content(
            Method::POST,
            &format!("/v1/rooms/{room_id}/subscriptions/{subscription_id}/layer"),
            Some(&serde_json::json!({
                "spatial_layer": spatial_layer,
                "temporal_layer": temporal_layer,
            })),
            true,
        )
        .await
    }

    /// Creates VOD metadata for a resumable source upload.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid identifiers or API rejection.
    pub async fn create_asset(
        &self,
        asset_id: &str,
        tenant_id: &str,
    ) -> Result<VodAsset, SdkError> {
        validate_media_id(asset_id)?;
        validate_media_id(tenant_id)?;
        self.request(
            Method::POST,
            "/v1/assets",
            Some(&serde_json::json!({
                "asset_id": asset_id,
                "tenant_id": tenant_id,
            })),
            true,
        )
        .await
    }

    /// Gets VOD lifecycle status.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid identifiers or API failure.
    pub async fn get_asset(&self, asset_id: &str) -> Result<VodAsset, SdkError> {
        validate_media_id(asset_id)?;
        self.request::<(), _>(Method::GET, &format!("/v1/assets/{asset_id}"), None, false)
            .await
    }

    /// Deletes a VOD asset and all published objects.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for an invalid asset ID or API failure.
    pub async fn delete_asset(&self, asset_id: &str) -> Result<(), SdkError> {
        validate_media_id(asset_id)?;
        self.request_no_content::<()>(
            Method::DELETE,
            &format!("/v1/assets/{asset_id}"),
            None,
            true,
        )
        .await
    }

    /// Appends one bounded source chunk at an exact byte offset.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for empty chunks, invalid IDs, offset conflicts, or transport failure.
    pub async fn upload_asset_chunk(
        &self,
        asset_id: &str,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<VodAsset, SdkError> {
        validate_media_id(asset_id)?;
        if bytes.is_empty() {
            return Err(SdkError::EmptyUploadChunk);
        }
        validate_payload_size("media upload", bytes.len(), MAX_MEDIA_UPLOAD_BYTES)?;
        let response = self
            .send_raw(
                Method::PATCH,
                &format!("/v1/assets/{asset_id}/source?offset={offset}"),
                bytes,
                "application/octet-stream",
                false,
            )
            .await?;
        decode_json_response(response, MAX_JSON_RESPONSE_BYTES).await
    }

    /// Completes upload and starts adaptive HLS packaging.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid IDs, invalid ladders, or worker/API failure.
    pub async fn complete_asset(
        &self,
        asset_id: &str,
        source_bytes: u64,
        segment_duration_millis: u32,
        renditions: &[Rendition],
    ) -> Result<VodAsset, SdkError> {
        validate_media_id(asset_id)?;
        self.request(
            Method::POST,
            &format!("/v1/assets/{asset_id}/complete"),
            Some(&serde_json::json!({
                "source_bytes": source_bytes,
                "segment_duration_millis": segment_duration_millis,
                "renditions": renditions,
            })),
            true,
        )
        .await
    }

    /// Creates a bounded live HLS window.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid identifiers or API rejection.
    pub async fn create_live_output(
        &self,
        stream_id: &str,
        window_segments: usize,
        first_sequence: u64,
    ) -> Result<LiveOutput, SdkError> {
        validate_media_id(stream_id)?;
        self.request(
            Method::POST,
            &format!("/v1/live/{stream_id}"),
            Some(&serde_json::json!({
                "window_segments": window_segments,
                "first_sequence": first_sequence,
            })),
            true,
        )
        .await
    }

    /// Creates a live output and binds SFU publisher RTP directly to the packager.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for malformed source room IDs or orchestration failure.
    pub async fn create_live_output_from_tracks(
        &self,
        stream_id: &str,
        window_segments: usize,
        first_sequence: u64,
        segment_duration_millis: u32,
        source_tracks: &[LiveSourceTrack],
    ) -> Result<LiveOutput, SdkError> {
        self.create_live_output_from_tracks_with_renditions(
            stream_id,
            window_segments,
            first_sequence,
            segment_duration_millis,
            source_tracks,
            &[],
        )
        .await
    }

    /// Creates a worker-backed live ABR output from SFU publisher tracks.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for an empty rendition ladder, malformed IDs, or orchestration failure.
    pub async fn create_live_abr_output_from_tracks(
        &self,
        stream_id: &str,
        window_segments: usize,
        first_sequence: u64,
        segment_duration_millis: u32,
        source_tracks: &[LiveSourceTrack],
        renditions: &[Rendition],
    ) -> Result<LiveOutput, SdkError> {
        if renditions.is_empty() {
            return Err(SdkError::EmptyRenditionLadder);
        }
        self.create_live_output_from_tracks_with_renditions(
            stream_id,
            window_segments,
            first_sequence,
            segment_duration_millis,
            source_tracks,
            renditions,
        )
        .await
    }

    async fn create_live_output_from_tracks_with_renditions(
        &self,
        stream_id: &str,
        window_segments: usize,
        first_sequence: u64,
        segment_duration_millis: u32,
        source_tracks: &[LiveSourceTrack],
        renditions: &[Rendition],
    ) -> Result<LiveOutput, SdkError> {
        validate_media_id(stream_id)?;
        for track in source_tracks {
            validate_id(&track.room_id)?;
        }
        self.request(
            Method::POST,
            &format!("/v1/live/{stream_id}"),
            Some(&serde_json::json!({
                "window_segments": window_segments,
                "first_sequence": first_sequence,
                "segment_duration_millis": segment_duration_millis,
                "source_tracks": source_tracks,
                "renditions": renditions,
            })),
            true,
        )
        .await
    }

    /// Gets a live output snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for an invalid stream ID or API failure.
    pub async fn get_live_output(&self, stream_id: &str) -> Result<LiveOutput, SdkError> {
        validate_media_id(stream_id)?;
        self.request::<(), _>(Method::GET, &format!("/v1/live/{stream_id}"), None, false)
            .await
    }

    /// Deletes a live output and all published objects.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for an invalid stream ID or API failure.
    pub async fn delete_live_output(&self, stream_id: &str) -> Result<(), SdkError> {
        validate_media_id(stream_id)?;
        self.request_no_content::<()>(Method::DELETE, &format!("/v1/live/{stream_id}"), None, true)
            .await
    }

    /// Uploads the CMAF initialization segment for a live output.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for empty bytes, invalid identifiers, or API failure.
    pub async fn upload_live_init(&self, stream_id: &str, bytes: Vec<u8>) -> Result<(), SdkError> {
        validate_media_id(stream_id)?;
        if bytes.is_empty() {
            return Err(SdkError::EmptyUploadChunk);
        }
        validate_payload_size("media upload", bytes.len(), MAX_MEDIA_UPLOAD_BYTES)?;
        let _ = self
            .send_raw(
                Method::PUT,
                &format!("/v1/live/{stream_id}/init"),
                bytes,
                "video/mp4",
                false,
            )
            .await?;
        Ok(())
    }

    /// Appends one exact-sequence live CMAF segment.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid data, sequence conflicts, or API failure.
    pub async fn upload_live_segment(
        &self,
        stream_id: &str,
        sequence: u64,
        duration_millis: u64,
        discontinuity: bool,
        bytes: Vec<u8>,
    ) -> Result<LiveOutput, SdkError> {
        validate_media_id(stream_id)?;
        if bytes.is_empty() {
            return Err(SdkError::EmptyUploadChunk);
        }
        validate_payload_size("media upload", bytes.len(), MAX_MEDIA_UPLOAD_BYTES)?;
        let response = self
            .send_raw(
            Method::PUT,
            &format!(
                "/v1/live/{stream_id}/segments/{sequence}?duration_millis={duration_millis}&discontinuity={discontinuity}"
            ),
            bytes,
            "video/iso.segment",
            false,
        )
            .await?;
        decode_json_response(response, MAX_JSON_RESPONSE_BYTES).await
    }

    /// Finalizes a live playlist with `EXT-X-ENDLIST`.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError`] for invalid identifiers or API failure.
    pub async fn finish_live_output(&self, stream_id: &str) -> Result<(), SdkError> {
        validate_media_id(stream_id)?;
        self.request_no_content::<()>(
            Method::POST,
            &format!("/v1/live/{stream_id}/finish"),
            None,
            true,
        )
        .await
    }

    async fn request<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        idempotent: bool,
    ) -> Result<R, SdkError> {
        let response = self.send(method, path, body, idempotent).await?;
        decode_json_response(response, MAX_JSON_RESPONSE_BYTES).await
    }

    async fn request_no_content<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        idempotent: bool,
    ) -> Result<(), SdkError> {
        let _ = self.send(method, path, body, idempotent).await?;
        Ok(())
    }

    async fn send<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        idempotent: bool,
    ) -> Result<reqwest::Response, SdkError> {
        let token = self
            .token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut request = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json");
        if idempotent {
            request = request.header("Idempotency-Key", random_id()?);
        }
        if let Some(body) = body {
            let encoded = serde_json::to_vec(body).map_err(SdkError::InvalidJsonRequest)?;
            validate_payload_size("JSON request body", encoded.len(), MAX_JSON_REQUEST_BYTES)?;
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(encoded);
        }
        let response = request.send().await.map_err(SdkError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            let error = decode_json_response::<ErrorBody>(response, MAX_ERROR_RESPONSE_BYTES)
                .await
                .unwrap_or(ErrorBody {
                    code: None,
                    message: None,
                });
            return Err(SdkError::Api {
                status,
                code: error.code.unwrap_or_else(|| "http_error".to_owned()),
                message: error
                    .message
                    .unwrap_or_else(|| format!("Fluvora API returned {status}")),
            });
        }
        Ok(response)
    }

    async fn send_raw(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        content_type: &'static str,
        idempotent: bool,
    ) -> Result<reqwest::Response, SdkError> {
        let token = self
            .token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut request = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        if idempotent {
            request = request.header("Idempotency-Key", random_id()?);
        }
        let response = request.send().await.map_err(SdkError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            let error = decode_json_response::<ErrorBody>(response, MAX_ERROR_RESPONSE_BYTES)
                .await
                .unwrap_or(ErrorBody {
                    code: None,
                    message: None,
                });
            return Err(SdkError::Api {
                status,
                code: error.code.unwrap_or_else(|| "http_error".to_owned()),
                message: error
                    .message
                    .unwrap_or_else(|| format!("Fluvora API returned {status}")),
            });
        }
        Ok(response)
    }
}

fn valid_access_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4_096 && !value.bytes().any(|byte| byte.is_ascii_control())
}

async fn decode_json_response<R: DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<R, SdkError> {
    let bytes = bounded_response_bytes(response, limit).await?;
    serde_json::from_slice(&bytes).map_err(SdkError::InvalidJsonResponse)
}

async fn bounded_response_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, SdkError> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(limit).unwrap_or(u64::MAX))
    {
        return Err(SdkError::ResponseTooLarge { limit });
    }
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default()
        .min(limit);
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(SdkError::Transport)? {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(SdkError::ResponseTooLarge { limit })?;
        if next > limit {
            return Err(SdkError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_id(value: &str) -> Result<(), SdkError> {
    if value.is_empty() || value.len() > 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Err(SdkError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn validate_media_id(value: &str) -> Result<(), SdkError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(SdkError::InvalidMediaIdentifier)
    } else {
        Ok(())
    }
}

fn validate_custom_namespace(value: &str) -> Result<(), SdkError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Err(SdkError::InvalidCustomNamespace)
    } else {
        Ok(())
    }
}

fn validate_json_size(
    field: &'static str,
    value: &serde_json::Value,
    limit: usize,
) -> Result<(), SdkError> {
    let size = serde_json::to_vec(value)
        .map_err(SdkError::InvalidJsonRequest)?
        .len();
    validate_payload_size(field, size, limit)
}

fn validate_payload_size(field: &'static str, size: usize, limit: usize) -> Result<(), SdkError> {
    if size > limit {
        Err(SdkError::PayloadTooLarge { field, limit })
    } else {
        Ok(())
    }
}

fn random_id() -> Result<String, SdkError> {
    use fmt::Write as _;
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| SdkError::RandomUnavailable)?;
    let mut output = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

/// SDK operation error.
#[derive(Debug)]
pub enum SdkError {
    /// API base URL is malformed.
    InvalidBaseUrl,
    /// Access token is empty, oversized, or contains control characters.
    EmptyAccessToken,
    /// Public identifier is not hexadecimal.
    InvalidIdentifier,
    /// Public media identifier is not safe ASCII.
    InvalidMediaIdentifier,
    /// P2P signal kind is outside the supported protocol vocabulary.
    InvalidSignalKind,
    /// A durable chat message cannot be empty.
    EmptyChatMessage,
    /// Custom-data namespaces use a bounded safe-ASCII vocabulary.
    InvalidCustomNamespace,
    /// A request field exceeded its wire-contract byte limit.
    PayloadTooLarge {
        /// Stable field label.
        field: &'static str,
        /// Maximum accepted encoded bytes.
        limit: usize,
    },
    /// ABR operations require at least one rendition.
    EmptyRenditionLadder,
    /// Upload APIs reject empty chunks.
    EmptyUploadChunk,
    /// Cryptographically secure randomness is unavailable.
    RandomUnavailable,
    /// The HTTP client could not be initialized safely.
    HttpClientConfiguration,
    /// A JSON response exceeded the SDK's bounded buffering limit.
    ResponseTooLarge {
        /// Maximum accepted response bytes.
        limit: usize,
    },
    /// A bounded response did not contain valid JSON for the requested operation.
    InvalidJsonResponse(serde_json::Error),
    /// A request body could not be encoded as JSON.
    InvalidJsonRequest(serde_json::Error),
    /// HTTP transport or response decoding failed.
    Transport(reqwest::Error),
    /// API returned a structured non-success response.
    Api {
        /// HTTP status.
        status: StatusCode,
        /// Stable machine-readable code.
        code: String,
        /// Operator/developer-readable message.
        message: String,
    },
    /// Platform WebRTC adapter failed.
    WebRtc(String),
}

impl fmt::Display for SdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => formatter.write_str("invalid Fluvora base URL"),
            Self::EmptyAccessToken => formatter.write_str("invalid Fluvora access token"),
            Self::InvalidIdentifier => formatter.write_str("invalid hexadecimal identifier"),
            Self::InvalidMediaIdentifier => formatter.write_str("invalid media identifier"),
            Self::InvalidSignalKind => formatter.write_str("unsupported P2P signal kind"),
            Self::EmptyChatMessage => formatter.write_str("chat message cannot be empty"),
            Self::InvalidCustomNamespace => formatter.write_str("invalid custom-data namespace"),
            Self::PayloadTooLarge { field, limit } => {
                write!(formatter, "{field} exceeds {limit} bytes")
            }
            Self::EmptyRenditionLadder => formatter.write_str("rendition ladder cannot be empty"),
            Self::EmptyUploadChunk => formatter.write_str("upload chunk cannot be empty"),
            Self::RandomUnavailable => formatter.write_str("secure random generator unavailable"),
            Self::HttpClientConfiguration => {
                formatter.write_str("failed to configure the Fluvora HTTP client")
            }
            Self::ResponseTooLarge { limit } => {
                write!(formatter, "Fluvora response exceeds {limit} bytes")
            }
            Self::InvalidJsonResponse(error) => write!(formatter, "invalid Fluvora JSON: {error}"),
            Self::InvalidJsonRequest(error) => {
                write!(formatter, "invalid Fluvora request JSON: {error}")
            }
            Self::Transport(error) => error.fmt(formatter),
            Self::Api {
                status,
                code,
                message,
            } => write!(formatter, "Fluvora API {status} {code}: {message}"),
            Self::WebRtc(message) => write!(formatter, "WebRTC adapter: {message}"),
        }
    }
}

impl std::error::Error for SdkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::InvalidJsonResponse(error) | Self::InvalidJsonRequest(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{
        CallbackWebRtcPeer, Client, MAX_CUSTOM_PAYLOAD_BYTES, MAX_SIGNAL_PAYLOAD_BYTES, MemberRole,
        RoomMode, RoomSnapshot, SdkError, VerifiedGift, WebRtcPeer, decode_json_response,
        validate_custom_namespace, validate_id, validate_json_size,
    };

    fn block_on_ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test callback unexpectedly pending"),
        }
    }

    #[test]
    fn validates_configuration_and_identifiers() {
        assert!(Client::new("https://api.example.com/", "token").is_ok());
        assert!(matches!(
            Client::new("file:///tmp/socket", "token"),
            Err(SdkError::InvalidBaseUrl)
        ));
        for base_url in [
            "https://token@api.example.com",
            "https://api.example.com?redirect=true",
            "https://api.example.com#fragment",
            "https://",
        ] {
            assert!(matches!(
                Client::new(base_url, "token"),
                Err(SdkError::InvalidBaseUrl)
            ));
        }
        for token in ["", "line\nbreak"] {
            assert!(matches!(
                Client::new("https://api.example.com", token),
                Err(SdkError::EmptyAccessToken)
            ));
        }
        assert!(validate_id("0123abcdef").is_ok());
        assert!(validate_id("../room").is_err());
        assert!(validate_custom_namespace("com.example.state").is_ok());
        assert!(validate_custom_namespace(".invalid").is_err());
        assert!(matches!(
            validate_json_size(
                "custom payload",
                &serde_json::json!("x".repeat(MAX_CUSTOM_PAYLOAD_BYTES)),
                MAX_CUSTOM_PAYLOAD_BYTES
            ),
            Err(SdkError::PayloadTooLarge { .. })
        ));
        assert!(matches!(
            validate_json_size(
                "signal payload",
                &serde_json::json!("x".repeat(MAX_SIGNAL_PAYLOAD_BYTES)),
                MAX_SIGNAL_PAYLOAD_BYTES
            ),
            Err(SdkError::PayloadTooLarge { .. })
        ));
        assert!(matches!(
            super::validate_payload_size(
                "media upload",
                super::MAX_MEDIA_UPLOAD_BYTES + 1,
                super::MAX_MEDIA_UPLOAD_BYTES
            ),
            Err(SdkError::PayloadTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn disables_redirects_and_bounds_json_responses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener");
        let address = listener.local_addr().expect("listener address");
        tokio::spawn(async move {
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n{}",
            ] {
                let (mut stream, _) = listener.accept().await.expect("test connection");
                let mut request = [0_u8; 1_024];
                let _ = stream.read(&mut request).await.expect("request");
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("response");
            }
        });

        let client = Client::new(format!("http://{address}"), "token").expect("client");
        let redirect = client
            .http
            .get(format!("http://{address}/redirect"))
            .send()
            .await
            .expect("redirect response");
        assert_eq!(redirect.status(), reqwest::StatusCode::FOUND);

        let response = client
            .http
            .get(format!("http://{address}/large"))
            .send()
            .await
            .expect("large response");
        assert!(matches!(
            decode_json_response::<serde_json::Value>(response, 16).await,
            Err(SdkError::ResponseTooLarge { limit: 16 })
        ));
    }

    #[test]
    fn preserves_v1_room_and_gift_wire_contracts() {
        let gift = VerifiedGift {
            provider: "payment-provider".to_owned(),
            provider_timestamp_millis: 1_800_000_000_000,
            provider_signature: "base64url-signature".to_owned(),
            sender_id: "00000000000000000000000000000001".to_owned(),
            recipient_id: "00000000000000000000000000000002".to_owned(),
            transaction_id: "transaction-42".to_owned(),
            gift_id: "rocket".to_owned(),
            quantity: 2,
            unit_value: 500,
            currency: "CNY".to_owned(),
        };
        let value = serde_json::to_value(&gift).expect("gift JSON");
        assert_eq!(value["provider_timestamp_millis"], 1_800_000_000_000_u64);
        assert_eq!(value["provider_signature"], "base64url-signature");
        assert_eq!(
            serde_json::to_value(MemberRole::Publisher).expect("role"),
            "publisher"
        );

        let snapshot: RoomSnapshot = serde_json::from_value(serde_json::json!({
            "room_id": "00000000000000000000000000000003",
            "mode": "sfu",
            "sequence": 9,
            "ended": false,
            "member_count": 3,
            "publisher_count": 1
        }))
        .expect("room snapshot");
        assert_eq!(snapshot.mode, RoomMode::Sfu);
        assert_eq!(snapshot.publisher_count, 1);
    }

    #[test]
    fn callback_webrtc_adapter_bridges_the_standard_negotiation_order() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let create_calls = Arc::clone(&calls);
        let answer_calls = Arc::clone(&calls);
        let channel_calls = Arc::clone(&calls);
        let mut peer = CallbackWebRtcPeer::new(
            move || {
                create_calls.lock().expect("calls").push("offer".to_owned());
                Box::pin(async { Ok("v=0\r\n".to_owned()) })
            },
            move |answer| {
                answer_calls
                    .lock()
                    .expect("calls")
                    .push(format!("answer:{answer}"));
                Box::pin(async { Ok(()) })
            },
        )
        .with_room_data_channel(move || {
            channel_calls
                .lock()
                .expect("calls")
                .push("data-channel".to_owned());
            Box::pin(async { Ok(()) })
        });

        block_on_ready(peer.prepare_room_data_channel()).expect("data channel");
        assert_eq!(
            block_on_ready(peer.create_offer()).expect("offer"),
            "v=0\r\n"
        );
        block_on_ready(peer.set_remote_answer("v=0 answer".to_owned())).expect("answer");
        assert_eq!(
            *calls.lock().expect("calls"),
            ["data-channel", "offer", "answer:v=0 answer"]
        );
    }
}
