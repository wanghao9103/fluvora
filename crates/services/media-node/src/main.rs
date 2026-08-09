use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
#[cfg(feature = "openssl-backend")]
use fluvora_data_channel::{
    Association, AssociationConfig, AssociationEvent, AssociationOutput, MessageKind,
};
use fluvora_media_codec::Codec;
use fluvora_media_node::{
    PublishTrack, RegistryError, RoutedActions, SessionIceRestart, SessionProvision,
    SessionRegistry, SfuRegistry, SfuRoute, SubscribeTrack,
};
use fluvora_observability::MediaNodeMetrics;
#[cfg(any(test, feature = "openssl-backend"))]
use fluvora_protocol::ENVELOPE_HEADER_BYTES;
#[cfg(feature = "openssl-backend")]
use fluvora_protocol::{DataKind, Envelope};
use fluvora_rtc_session::SessionAction;
use fluvora_rtcp::{Packet as RtcpPacket, parse_compound};
use fluvora_rtp::ExtensionRewrite;
use fluvora_sfu_core::{Encoding, MediaKind};
use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;

const MAX_DATAGRAM_BYTES: usize = 65_535;
const MAX_TRANSCODE_INGRESSES: usize = 1_024;
#[cfg(any(test, feature = "openssl-backend"))]
const MAX_DATA_CHANNEL_MESSAGE_BYTES: usize = 16 * 1_024;
#[cfg(any(test, feature = "openssl-backend"))]
const MAX_DATA_CHANNEL_PAYLOAD_BYTES: usize =
    MAX_DATA_CHANNEL_MESSAGE_BYTES - ENVELOPE_HEADER_BYTES;

#[cfg(feature = "openssl-backend")]
struct CryptoRuntime {
    server: fluvora_dtls_adapter::openssl_backend::DtlsServer,
    sessions: Mutex<HashMap<String, fluvora_dtls_adapter::openssl_backend::DatagramDtlsSession>>,
    data_channels: Mutex<HashMap<String, DataChannelSession>>,
    data_sequences: Mutex<HashMap<String, u64>>,
    metrics: Arc<MediaNodeMetrics>,
    epoch: Instant,
}

#[cfg(feature = "openssl-backend")]
#[derive(Debug)]
struct DataChannelSession {
    association: Association,
    stream_labels: HashMap<u16, String>,
    label_streams: HashMap<String, u16>,
}

#[cfg(not(feature = "openssl-backend"))]
struct CryptoRuntime;

#[derive(Clone)]
struct AppState {
    registry: Arc<SessionRegistry>,
    metrics: Arc<MediaNodeMetrics>,
    sfu: Arc<SfuRegistry>,
    crypto: Arc<CryptoRuntime>,
    token: Arc<str>,
    epoch: Instant,
    media_socket: Arc<UdpSocket>,
    transcode_ingresses: Arc<Mutex<HashMap<u64, TranscodeIngress>>>,
    next_transcode_ingress_id: Arc<AtomicU64>,
}

#[derive(Debug)]
struct TranscodeIngress {
    room_id: String,
    track_id: u64,
    cancellation: oneshot::Sender<()>,
}

#[derive(Debug, Deserialize)]
struct ProvisionRequest {
    session_id: String,
    room_id: String,
    participant_id: String,
    local_username_fragment: String,
    local_password: String,
    remote_username_fragment: String,
    remote_password: String,
    expected_peer_fingerprint: String,
    tie_breaker: u64,
}

#[derive(Debug, Deserialize)]
struct IceRestartRequest {
    local_username_fragment: String,
    local_password: String,
    remote_username_fragment: String,
    remote_password: String,
    tie_breaker: u64,
}

#[derive(Debug, Deserialize)]
struct PublishTrackRequest {
    room_id: String,
    participant_id: String,
    track_id: u64,
    kind: String,
    codec: String,
    clock_rate: u32,
    payload_type: u8,
    encodings: Vec<EncodingRequest>,
}

#[derive(Debug, Deserialize)]
struct UnpublishTrackRequest {
    room_id: String,
    participant_id: String,
}

#[derive(Debug, Deserialize)]
struct EncodingRequest {
    ssrc: u32,
    rid: Option<String>,
    spatial_layer: u8,
    max_bitrate_bps: u64,
}

#[derive(Debug, Deserialize)]
struct SubscribeTrackRequest {
    room_id: String,
    participant_id: String,
    subscription_id: u64,
    track_id: u64,
    output_ssrc: u32,
    output_payload_type: u8,
    spatial_layer: u8,
    temporal_layer: u8,
    initial_sequence_number: u16,
    initial_timestamp: u32,
    #[serde(default)]
    extension_rewrites: Vec<ExtensionRewriteRequest>,
    transport_wide_extension_id: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct ExtensionRewriteRequest {
    source_id: u8,
    destination_id: Option<u8>,
    replacement: Option<Vec<u8>>,
}

#[derive(Debug, Deserialize)]
struct LayerRequest {
    room_id: String,
    participant_id: String,
    spatial_layer: u8,
    temporal_layer: u8,
}

#[derive(Debug, Deserialize)]
struct UnsubscribeRequest {
    room_id: String,
    participant_id: String,
}

#[derive(Debug, Deserialize)]
struct RecordingSinkRequest {
    room_id: String,
    track_id: u64,
    destination: SocketAddr,
    source_ssrc: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CreateTranscodeIngressRequest {
    room_id: String,
    participant_id: String,
    track_id: u64,
    kind: String,
    codec: String,
    clock_rate: u32,
    payload_type: u8,
    ssrc: u32,
    max_bitrate_bps: u64,
}

#[derive(Debug, Serialize)]
struct CreateTranscodeIngressResponse {
    ingress_id: u64,
    destination: SocketAddr,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    session_id: String,
    room_id: String,
    participant_id: String,
    state: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() {
    let udp_bind =
        env::var("FLUVORA_MEDIA_UDP_BIND").unwrap_or_else(|_| "0.0.0.0:50000".to_owned());
    let control_bind =
        env::var("FLUVORA_MEDIA_CONTROL_BIND").unwrap_or_else(|_| "127.0.0.1:8092".to_owned());
    let udp_address: SocketAddr = udp_bind
        .parse()
        .expect("FLUVORA_MEDIA_UDP_BIND must be host:port");
    let control_address: SocketAddr = control_bind
        .parse()
        .expect("FLUVORA_MEDIA_CONTROL_BIND must be host:port");
    let token =
        env::var("FLUVORA_MEDIA_CONTROL_TOKEN").expect("FLUVORA_MEDIA_CONTROL_TOKEN is required");
    assert!(
        (16..=4_096).contains(&token.len()) && !token.bytes().any(|byte| byte.is_ascii_control()),
        "FLUVORA_MEDIA_CONTROL_TOKEN must contain 16..=4096 non-control bytes"
    );
    let capacity = env::var("FLUVORA_MEDIA_MAX_SESSIONS")
        .map_or(Ok(10_000), |value| value.parse::<usize>())
        .expect("FLUVORA_MEDIA_MAX_SESSIONS must be an integer");
    let socket = Arc::new(UdpSocket::bind(udp_address).await.expect("media UDP bind"));
    let node_metrics = Arc::new(MediaNodeMetrics::default());
    let registry = Arc::new(SessionRegistry::new(Arc::clone(&node_metrics), capacity));
    let sfu = Arc::new(SfuRegistry::default());
    let crypto = Arc::new(create_crypto_runtime(Arc::clone(&node_metrics)));
    let state = AppState {
        registry: Arc::clone(&registry),
        metrics: Arc::clone(&node_metrics),
        sfu: Arc::clone(&sfu),
        crypto: Arc::clone(&crypto),
        token: Arc::from(token),
        epoch: Instant::now(),
        media_socket: Arc::clone(&socket),
        transcode_ingresses: Arc::new(Mutex::new(HashMap::new())),
        next_transcode_ingress_id: Arc::new(AtomicU64::new(1)),
    };
    let app = build_router(state.clone());
    println!(
        "{} media node UDP {udp_address}, control {control_address}",
        fluvora_domain::PLATFORM_NAME
    );
    let udp_task = tokio::spawn(run_udp(
        Arc::clone(&socket),
        Arc::clone(&registry),
        Arc::clone(&sfu),
        Arc::clone(&crypto),
        Arc::clone(&node_metrics),
        Instant::now(),
    ));
    let timer_task = tokio::spawn(run_timers(
        socket,
        registry,
        sfu,
        crypto,
        node_metrics,
        Instant::now(),
    ));
    let (heartbeat, heartbeat_task) = start_heartbeat(state.clone(), capacity);
    let listener = tokio::net::TcpListener::bind(control_address)
        .await
        .expect("media control bind");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let shutdown_heartbeat = heartbeat.clone();
    let shutdown_state = state.clone();
    let shutdown_task = tokio::spawn(async move {
        shutdown_signal().await;
        if let Some(client) = shutdown_heartbeat.as_ref() {
            client.mark_draining();
        }
        let _ = shutdown_tx.send(());
        if let Some(client) = shutdown_heartbeat.as_ref() {
            let _ = client
                .report(media_node_capacity(&shutdown_state, capacity), true)
                .await;
        }
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await
        .expect("media control server");
    let _ = shutdown_task.await;
    stop_heartbeat(heartbeat.as_ref(), heartbeat_task, &state, capacity).await;
    udp_task.abort();
    timer_task.abort();
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(render_metrics))
        .route("/v1/sessions", post(provision_session))
        .route(
            "/v1/sessions/{session_id}",
            get(get_session).delete(delete_session),
        )
        .route(
            "/v1/sessions/{session_id}/ice-restart",
            post(restart_session_ice),
        )
        .route("/v1/sfu/tracks", post(publish_track))
        .route("/v1/sfu/tracks/{track_id}", delete(unpublish_track))
        .route("/v1/sfu/subscriptions", post(subscribe_track))
        .route(
            "/v1/sfu/subscriptions/{subscription_id}",
            delete(unsubscribe_track),
        )
        .route(
            "/v1/sfu/subscriptions/{subscription_id}/layer",
            post(set_subscription_layer),
        )
        .route(
            "/v1/sfu/recordings",
            post(add_recording_sink).delete(remove_recording_sink),
        )
        .route(
            "/v1/sfu/transcode-ingresses",
            post(create_transcode_ingress),
        )
        .route(
            "/v1/sfu/transcode-ingresses/{ingress_id}",
            delete(delete_transcode_ingress),
        )
        .with_state(state)
}

fn start_heartbeat(
    state: AppState,
    session_limit: usize,
) -> (Option<HeartbeatClient>, Option<tokio::task::JoinHandle<()>>) {
    let client = HeartbeatClient::from_env(ServiceKind::MediaNode)
        .expect("valid status heartbeat configuration");
    let task = client.as_ref().map(|client| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .run(|| std::future::ready(media_node_capacity(&state, session_limit)))
                .await;
        })
    });
    (client, task)
}

async fn stop_heartbeat(
    client: Option<&HeartbeatClient>,
    task: Option<tokio::task::JoinHandle<()>>,
    state: &AppState,
    session_limit: usize,
) {
    if let Some(client) = client {
        client.mark_draining();
        if let Err(error) = client
            .report(media_node_capacity(state, session_limit), true)
            .await
        {
            eprintln!("failed to report draining media-node heartbeat: {error}");
        }
    }
    if let Some(task) = task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            eprintln!("media-node heartbeat task failed during shutdown: {error}");
        }
    }
}

fn media_node_capacity(state: &AppState, session_limit: usize) -> NodeCapacity {
    let allocation = state.sfu.stats();
    NodeCapacity {
        rooms_limit: u64::try_from(session_limit).unwrap_or(u64::MAX),
        rooms_used: u64::try_from(allocation.rooms).unwrap_or(u64::MAX),
        sessions_limit: u64::try_from(session_limit).unwrap_or(u64::MAX),
        sessions_used: u64::try_from(state.registry.len()).unwrap_or(u64::MAX),
        publisher_tracks: u64::try_from(allocation.publisher_tracks).unwrap_or(u64::MAX),
        subscriber_tracks: u64::try_from(allocation.subscriber_tracks).unwrap_or(u64::MAX),
        cpu_per_mille: 0,
        memory_bytes: process_memory_bytes(),
        ..NodeCapacity::default()
    }
}

async fn run_udp(
    socket: Arc<UdpSocket>,
    registry: Arc<SessionRegistry>,
    sfu: Arc<SfuRegistry>,
    crypto: Arc<CryptoRuntime>,
    metrics: Arc<MediaNodeMetrics>,
    epoch: Instant,
) {
    let mut buffer = vec![0_u8; MAX_DATAGRAM_BYTES];
    loop {
        let Ok((length, remote)) = socket.recv_from(&mut buffer).await else {
            continue;
        };
        let Ok(local) = socket.local_addr() else {
            continue;
        };
        let now = epoch.elapsed();
        let started = Instant::now();
        let output = match registry.handle_datagram(now, local, remote, &buffer[..length]) {
            Ok(output) => output,
            Err(error) => {
                metrics.packets_dropped.increment();
                if matches!(
                    error,
                    RegistryError::UnknownRemote
                        | RegistryError::Stun(_)
                        | RegistryError::MissingUsername
                        | RegistryError::UnknownUsernameFragment
                        | RegistryError::RemoteCollision
                        | RegistryError::Transport(_)
                ) {
                    metrics.authentication_failures.increment();
                }
                metrics
                    .packet_processing_micros
                    .observe_micros(elapsed_micros(started));
                continue;
            }
        };
        execute_actions(&socket, &registry, &sfu, &crypto, &metrics, now, &output).await;
        metrics
            .packet_processing_micros
            .observe_micros(elapsed_micros(started));
    }
}

async fn run_timers(
    socket: Arc<UdpSocket>,
    registry: Arc<SessionRegistry>,
    sfu: Arc<SfuRegistry>,
    crypto: Arc<CryptoRuntime>,
    metrics: Arc<MediaNodeMetrics>,
    epoch: Instant,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        for output in registry.tick(epoch.elapsed()) {
            execute_actions(
                &socket,
                &registry,
                &sfu,
                &crypto,
                &metrics,
                epoch.elapsed(),
                &output,
            )
            .await;
        }
        poll_crypto(&socket, &registry, &crypto).await;
    }
}

async fn execute_actions(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    sfu: &SfuRegistry,
    crypto: &CryptoRuntime,
    metrics: &MediaNodeMetrics,
    now: Duration,
    output: &RoutedActions,
) {
    for action in &output.actions {
        match action {
            SessionAction::Transmit(transmit) => {
                if socket
                    .send_to(&transmit.payload, transmit.destination)
                    .await
                    .is_err()
                {
                    metrics.packets_dropped.increment();
                }
            }
            SessionAction::DtlsInput(datagram) => {
                process_dtls(
                    socket,
                    registry,
                    crypto,
                    &output.session_id,
                    &output.expected_peer_fingerprint,
                    datagram,
                )
                .await;
            }
            SessionAction::InboundRtp(packet) => {
                if let Ok(routes) =
                    sfu.handle_rtp(now, &output.room_id, &output.participant_id, packet)
                {
                    send_sfu_routes(socket, registry, sfu, metrics, now, routes).await;
                } else {
                    metrics.packets_dropped.increment();
                }
            }
            SessionAction::InboundRtcp { bytes, .. } => {
                if let Ok(packets) = parse_compound(bytes) {
                    for packet in packets {
                        match packet {
                            RtcpPacket::GenericNack(nack) => metrics
                                .nack_requests
                                .add(u64::try_from(nack.entries.len()).unwrap_or(u64::MAX)),
                            RtcpPacket::PictureLossIndication(_) => {
                                metrics.pli_requests.increment();
                            }
                            _ => {}
                        }
                    }
                }
                if let Ok(routes) =
                    sfu.handle_rtcp(now, &output.room_id, &output.participant_id, bytes)
                {
                    send_sfu_routes(socket, registry, sfu, metrics, now, routes).await;
                } else {
                    metrics.packets_dropped.increment();
                }
            }
            SessionAction::StateChanged { .. } => {}
        }
    }
}

async fn send_sfu_routes(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    sfu: &SfuRegistry,
    metrics: &MediaNodeMetrics,
    now: Duration,
    routes: Vec<SfuRoute>,
) {
    for route in routes {
        match route {
            SfuRoute::Rtp {
                session_id,
                room_id,
                subscriber,
                subscription_id,
                register_twcc,
                packet,
            } => {
                if register_twcc {
                    let _ = sfu.register_sent(now, &room_id, subscriber, subscription_id, &packet);
                }
                if let Ok(transmit) = registry.protect_rtp(&session_id, packet) {
                    if socket
                        .send_to(&transmit.payload, transmit.destination)
                        .await
                        .is_ok()
                    {
                        metrics.rtp_packets_sent.increment();
                        metrics
                            .media_bytes_sent
                            .add(u64::try_from(transmit.payload.len()).unwrap_or(u64::MAX));
                    } else {
                        metrics.packets_dropped.increment();
                    }
                } else {
                    metrics.packets_dropped.increment();
                }
            }
            SfuRoute::Rtcp { session_id, packet } => {
                if let Ok(transmit) = registry.protect_rtcp(&session_id, packet) {
                    if socket
                        .send_to(&transmit.payload, transmit.destination)
                        .await
                        .is_ok()
                    {
                        metrics
                            .media_bytes_sent
                            .add(u64::try_from(transmit.payload.len()).unwrap_or(u64::MAX));
                    } else {
                        metrics.packets_dropped.increment();
                    }
                } else {
                    metrics.packets_dropped.increment();
                }
            }
            SfuRoute::RecorderRtp {
                destination,
                packet,
            } => {
                if socket.send_to(&packet, destination).await.is_err() {
                    metrics.packets_dropped.increment();
                }
            }
        }
    }
}

#[cfg(feature = "openssl-backend")]
fn create_crypto_runtime(metrics: Arc<MediaNodeMetrics>) -> CryptoRuntime {
    use fluvora_dtls_adapter::openssl_backend::{DtlsServer, Identity};

    let certificate_path =
        env::var("FLUVORA_DTLS_CERT_PEM").expect("FLUVORA_DTLS_CERT_PEM is required");
    let key_path = env::var("FLUVORA_DTLS_KEY_PEM").expect("FLUVORA_DTLS_KEY_PEM is required");
    let certificate = std::fs::read(certificate_path).expect("read DTLS certificate");
    let key = std::fs::read(key_path).expect("read DTLS private key");
    let identity = Identity::from_pem(&certificate, &key).expect("valid DTLS identity");
    let fingerprint = identity
        .fingerprint()
        .expect("DTLS fingerprint")
        .to_string();
    println!("DTLS certificate SHA-256 fingerprint {fingerprint}");
    if let Ok(path) = env::var("FLUVORA_DTLS_FINGERPRINT_FILE") {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create DTLS fingerprint directory");
        }
        std::fs::write(path, fingerprint).expect("write DTLS fingerprint file");
    }
    CryptoRuntime {
        server: DtlsServer::new(&identity).expect("secure DTLS server configuration"),
        sessions: Mutex::new(HashMap::new()),
        data_channels: Mutex::new(HashMap::new()),
        data_sequences: Mutex::new(HashMap::new()),
        metrics,
        epoch: Instant::now(),
    }
}

#[cfg(not(feature = "openssl-backend"))]
fn create_crypto_runtime(_metrics: Arc<MediaNodeMetrics>) -> CryptoRuntime {
    CryptoRuntime
}

#[cfg(feature = "openssl-backend")]
async fn process_dtls(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    session_id: &str,
    expected_fingerprint: &str,
    datagram: &[u8],
) {
    use fluvora_dtls_adapter::Sha256Fingerprint;

    let Ok(fingerprint) = Sha256Fingerprint::parse("sha-256", expected_fingerprint) else {
        crypto.metrics.authentication_failures.increment();
        return;
    };
    let progress = {
        let mut sessions = crypto
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = sessions
            .entry(session_id.to_owned())
            .or_insert_with(|| crypto.server.datagram_session(fingerprint));
        session.handle_datagram(datagram)
    };
    match progress {
        Ok(progress) => {
            apply_crypto_progress(socket, registry, crypto, session_id, progress).await;
        }
        Err(_) => crypto.metrics.authentication_failures.increment(),
    }
}

#[cfg(not(feature = "openssl-backend"))]
fn process_dtls(
    _socket: &UdpSocket,
    _registry: &SessionRegistry,
    _crypto: &CryptoRuntime,
    _session_id: &str,
    _expected_fingerprint: &str,
    _datagram: &[u8],
) -> std::future::Ready<()> {
    std::future::ready(())
}

#[cfg(feature = "openssl-backend")]
async fn apply_crypto_progress(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    session_id: &str,
    progress: fluvora_dtls_adapter::openssl_backend::DatagramProgress,
) {
    for datagram in progress.outbound_datagrams {
        if let Ok(transmit) = registry.transmit_dtls(session_id, datagram) {
            let _ = socket
                .send_to(&transmit.payload, transmit.destination)
                .await;
        }
    }
    if let Some(keying) = progress.established_keying_material {
        let _ = registry.install_dtls_keying_material(session_id, &keying);
    }
    for application_data in progress.application_data {
        process_sctp_packet(socket, registry, crypto, session_id, &application_data).await;
    }
}

#[cfg(feature = "openssl-backend")]
async fn poll_crypto(socket: &UdpSocket, registry: &SessionRegistry, crypto: &CryptoRuntime) {
    let outputs = {
        let mut sessions = crypto
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .iter_mut()
            .filter_map(|(session_id, session)| {
                session
                    .poll()
                    .ok()
                    .map(|progress| (session_id.clone(), progress))
            })
            .collect::<Vec<_>>()
    };
    for (session_id, progress) in outputs {
        apply_crypto_progress(socket, registry, crypto, &session_id, progress).await;
    }
    poll_data_channels(socket, registry, crypto).await;
}

#[cfg(feature = "openssl-backend")]
async fn process_sctp_packet(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    session_id: &str,
    packet: &[u8],
) {
    let Some(session) = new_data_channel_session() else {
        return;
    };
    let output = {
        let mut channels = crypto
            .data_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let channel = match channels.entry(session_id.to_owned()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                crypto.metrics.active_data_channel_associations.add(1);
                entry.insert(session)
            }
        };
        channel
            .association
            .handle_packet(crypto.epoch.elapsed(), packet)
    };
    let Ok(output) = output else {
        crypto.metrics.data_channel_rejections.increment();
        return;
    };
    let routed = route_data_channel_output(registry, crypto, session_id, output);
    transmit_sctp_packets(socket, registry, crypto, routed).await;
}

#[cfg(feature = "openssl-backend")]
fn new_data_channel_session() -> Option<DataChannelSession> {
    let mut entropy = [0_u8; 40];
    getrandom::fill(&mut entropy).ok()?;
    let verification_tag = u32::from_be_bytes(entropy[0..4].try_into().ok()?) | 1;
    let initial_tsn = u32::from_be_bytes(entropy[4..8].try_into().ok()?);
    Some(DataChannelSession {
        association: Association::new(AssociationConfig {
            local_port: 5_000,
            remote_port: 5_000,
            verification_tag,
            initial_tsn,
            cookie: entropy[8..].to_vec(),
            maximum_channels: 256,
            maximum_message_bytes: MAX_DATA_CHANNEL_MESSAGE_BYTES,
        })
        .ok()?,
        stream_labels: HashMap::new(),
        label_streams: HashMap::new(),
    })
}

#[cfg(feature = "openssl-backend")]
fn route_data_channel_output(
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    session_id: &str,
    output: AssociationOutput,
) -> Vec<(String, Vec<Vec<u8>>)> {
    let mut routed = Vec::new();
    let mut close_association = false;
    if !output.packets.is_empty() {
        routed.push((session_id.to_owned(), output.packets));
    }
    for event in output.events {
        match event {
            AssociationEvent::ChannelOpened {
                stream_id, label, ..
            } => {
                let mut channels = crypto
                    .data_channels
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(channel) = channels.get_mut(session_id) {
                    if channel
                        .stream_labels
                        .insert(stream_id, label.clone())
                        .is_none()
                    {
                        crypto.metrics.active_data_channels.add(1);
                    }
                    channel.label_streams.entry(label).or_insert(stream_id);
                }
            }
            AssociationEvent::Message {
                stream_id,
                kind,
                payload,
            } => {
                routed.extend(route_room_data(
                    registry, crypto, session_id, stream_id, kind, payload,
                ));
            }
            AssociationEvent::ChannelClosed { stream_id } => {
                let mut channels = crypto
                    .data_channels
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(channel) = channels.get_mut(session_id)
                    && let Some(label) = channel.stream_labels.remove(&stream_id)
                {
                    if channel.label_streams.get(&label) == Some(&stream_id) {
                        channel.label_streams.remove(&label);
                    }
                    crypto.metrics.active_data_channels.add(-1);
                }
            }
            AssociationEvent::Established => {}
            AssociationEvent::Closed => close_association = true,
            AssociationEvent::DeliveryFailed { .. } => {
                crypto.metrics.data_channel_delivery_failures.increment();
                close_association = true;
            }
            AssociationEvent::MessageAbandoned { .. } => {
                crypto.metrics.data_channel_messages_abandoned.increment();
            }
        }
    }
    if close_association {
        remove_data_channel_session(crypto, session_id);
    }
    routed
}

#[cfg(feature = "openssl-backend")]
fn route_room_data(
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    session_id: &str,
    stream_id: u16,
    kind: MessageKind,
    payload: Vec<u8>,
) -> Vec<(String, Vec<Vec<u8>>)> {
    let Some(source) = registry.session_snapshot(session_id) else {
        return Vec::new();
    };
    let label = {
        let channels = crypto
            .data_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels
            .get(session_id)
            .and_then(|channel| channel.stream_labels.get(&stream_id))
            .cloned()
    };
    let Some(label) = label else {
        crypto.metrics.data_channel_rejections.increment();
        return Vec::new();
    };
    let Some((kind, payload)) = authorize_data_payload(crypto, &source, &label, kind, payload)
    else {
        crypto.metrics.data_channel_rejections.increment();
        return Vec::new();
    };
    crypto.metrics.data_channel_messages_received.increment();
    let target_sessions = registry.session_ids_in_room(&source.room_id);
    let mut channels = crypto
        .data_channels
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let routed = target_sessions
        .into_iter()
        .filter_map(|target_session| {
            let target = channels.get_mut(&target_session)?;
            let stream_id = *target.label_streams.get(&label)?;
            let packets = target
                .association
                .send_message(crypto.epoch.elapsed(), stream_id, kind, &payload)
                .ok()?;
            Some((target_session, packets))
        })
        .collect::<Vec<_>>();
    crypto
        .metrics
        .data_channel_messages_sent
        .add(u64::try_from(routed.len()).unwrap_or(u64::MAX));
    routed
}

#[cfg(feature = "openssl-backend")]
fn authorize_data_payload(
    crypto: &CryptoRuntime,
    source: &fluvora_media_node::SessionSnapshot,
    label: &str,
    kind: MessageKind,
    payload: Vec<u8>,
) -> Option<(MessageKind, Vec<u8>)> {
    if label != "fluvora.room.v1" {
        return Some((kind, payload));
    }
    if kind != MessageKind::Binary {
        return None;
    }
    let room_id = u128::from_str_radix(&source.room_id, 16).ok()?;
    let participant_id = u128::from_str_radix(&source.participant_id, 16).ok()?;
    let mut envelope = Envelope::decode(&payload, MAX_DATA_CHANNEL_PAYLOAD_BYTES).ok()?;
    if !matches!(
        envelope.kind,
        DataKind::Chat | DataKind::Control | DataKind::Custom(_)
    ) || (envelope.room_id != 0 && envelope.room_id != room_id)
        || (envelope.sender_id != 0 && envelope.sender_id != participant_id)
    {
        return None;
    }
    envelope.room_id = room_id;
    envelope.sender_id = participant_id;
    envelope.sequence = {
        let mut sequences = crypto
            .data_sequences
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = sequences.entry(source.room_id.clone()).or_default();
        *sequence = sequence.saturating_add(1);
        *sequence
    };
    envelope.timestamp_millis = unix_time_millis();
    envelope
        .encode(MAX_DATA_CHANNEL_PAYLOAD_BYTES)
        .ok()
        .map(|payload| (MessageKind::Binary, payload))
}

#[cfg(feature = "openssl-backend")]
async fn poll_data_channels(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
) {
    let outputs = {
        let mut channels = crypto
            .data_channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels
            .iter_mut()
            .filter_map(|(session_id, channel)| {
                let output = channel.association.tick(crypto.epoch.elapsed());
                (!output.packets.is_empty() || !output.events.is_empty())
                    .then_some((session_id.clone(), output))
            })
            .collect::<Vec<_>>()
    };
    let mut packets = Vec::new();
    for (session_id, output) in outputs {
        crypto
            .metrics
            .data_channel_retransmissions
            .add(output.retransmitted_packets);
        packets.extend(route_data_channel_output(
            registry,
            crypto,
            &session_id,
            output,
        ));
    }
    transmit_sctp_packets(socket, registry, crypto, packets).await;
}

#[cfg(feature = "openssl-backend")]
async fn transmit_sctp_packets(
    socket: &UdpSocket,
    registry: &SessionRegistry,
    crypto: &CryptoRuntime,
    packets: Vec<(String, Vec<Vec<u8>>)>,
) {
    let datagrams = {
        let mut sessions = crypto
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut datagrams = Vec::new();
        for (session_id, packets) in packets {
            let Some(session) = sessions.get_mut(&session_id) else {
                continue;
            };
            for packet in packets {
                if let Ok(encrypted) = session.write_application_data(&packet) {
                    datagrams.extend(
                        encrypted
                            .into_iter()
                            .map(|datagram| (session_id.clone(), datagram)),
                    );
                }
            }
        }
        datagrams
    };
    for (session_id, datagram) in datagrams {
        if let Ok(transmit) = registry.transmit_dtls(&session_id, datagram) {
            let _ = socket
                .send_to(&transmit.payload, transmit.destination)
                .await;
        }
    }
}

#[cfg(feature = "openssl-backend")]
fn unix_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(not(feature = "openssl-backend"))]
fn poll_crypto(
    _socket: &UdpSocket,
    _registry: &SessionRegistry,
    _crypto: &CryptoRuntime,
) -> std::future::Ready<()> {
    std::future::ready(())
}

async fn live() -> StatusCode {
    StatusCode::OK
}

#[cfg(feature = "openssl-backend")]
async fn ready() -> StatusCode {
    StatusCode::OK
}

#[cfg(not(feature = "openssl-backend"))]
async fn ready() -> StatusCode {
    StatusCode::SERVICE_UNAVAILABLE
}

async fn render_metrics(State(state): State<AppState>) -> String {
    state.metrics.render_prometheus()
}

async fn provision_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProvisionRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    let session_id = request.session_id.clone();
    let room_id = request.room_id.clone();
    let participant_id = request.participant_id.clone();
    state
        .registry
        .provision(SessionProvision {
            session_id: request.session_id,
            room_id: request.room_id,
            participant_id: request.participant_id,
            local_username_fragment: request.local_username_fragment,
            local_password: request.local_password,
            remote_username_fragment: request.remote_username_fragment,
            remote_password: request.remote_password,
            expected_peer_fingerprint: request.expected_peer_fingerprint,
            tie_breaker: request.tie_breaker,
        })
        .map_err(|error| registry_error(&error))?;
    if let Err(error) = state
        .sfu
        .bind_session(&room_id, &participant_id, &session_id)
    {
        let _ = state.registry.remove(&session_id);
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "sfu_session_bind_failed",
            message: error.to_string(),
        });
    }
    Ok(StatusCode::CREATED)
}

async fn restart_session_ice(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<IceRestartRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .registry
        .restart_ice(SessionIceRestart {
            session_id,
            local_username_fragment: request.local_username_fragment,
            local_password: request.local_password,
            remote_username_fragment: request.remote_username_fragment,
            remote_password: request.remote_password,
            tie_breaker: request.tie_breaker,
        })
        .map_err(|error| registry_error(&error))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_track(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PublishTrackRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .publish(PublishTrack {
            room_id: request.room_id,
            participant_id: request.participant_id,
            track_id: request.track_id,
            kind: parse_media_kind(&request.kind)?,
            codec: parse_codec(&request.codec)?,
            clock_rate: request.clock_rate,
            payload_type: request.payload_type,
            encodings: request
                .encodings
                .into_iter()
                .map(|encoding| Encoding {
                    ssrc: encoding.ssrc,
                    rid: encoding.rid,
                    spatial_layer: encoding.spatial_layer,
                    max_bitrate_bps: encoding.max_bitrate_bps,
                })
                .collect(),
        })
        .map_err(|error| sfu_error(&error))?;
    state.metrics.publisher_tracks.add(1);
    Ok(StatusCode::CREATED)
}

async fn unpublish_track(
    State(state): State<AppState>,
    Path(track_id): Path<u64>,
    headers: HeaderMap,
    Json(request): Json<UnpublishTrackRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .unpublish_owned(&request.room_id, &request.participant_id, track_id)
        .map_err(|error| sfu_error(&error))?;
    let allocation = state.sfu.stats();
    state
        .metrics
        .active_rooms
        .set(i64::try_from(allocation.rooms).unwrap_or(i64::MAX));
    state
        .metrics
        .publisher_tracks
        .set(i64::try_from(allocation.publisher_tracks).unwrap_or(i64::MAX));
    state
        .metrics
        .subscriber_tracks
        .set(i64::try_from(allocation.subscriber_tracks).unwrap_or(i64::MAX));
    Ok(StatusCode::NO_CONTENT)
}

async fn subscribe_track(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubscribeTrackRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .subscribe(&SubscribeTrack {
            room_id: request.room_id,
            participant_id: request.participant_id,
            subscription_id: request.subscription_id,
            track_id: request.track_id,
            output_ssrc: request.output_ssrc,
            output_payload_type: request.output_payload_type,
            spatial_layer: request.spatial_layer,
            temporal_layer: request.temporal_layer,
            initial_sequence_number: request.initial_sequence_number,
            initial_timestamp: request.initial_timestamp,
            extension_rewrites: request
                .extension_rewrites
                .into_iter()
                .map(|rewrite| ExtensionRewrite {
                    source_id: rewrite.source_id,
                    destination_id: rewrite.destination_id,
                    replacement: rewrite.replacement,
                })
                .collect(),
            transport_wide_extension_id: request.transport_wide_extension_id,
        })
        .map_err(|error| sfu_error(&error))?;
    state.metrics.subscriber_tracks.add(1);
    Ok(StatusCode::CREATED)
}

async fn set_subscription_layer(
    State(state): State<AppState>,
    Path(subscription_id): Path<u64>,
    headers: HeaderMap,
    Json(request): Json<LayerRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .set_layer(
            state.epoch.elapsed(),
            &request.room_id,
            &request.participant_id,
            subscription_id,
            request.spatial_layer,
            request.temporal_layer,
        )
        .map_err(|error| sfu_error(&error))?;
    state.metrics.layer_switches.increment();
    Ok(StatusCode::NO_CONTENT)
}

async fn unsubscribe_track(
    State(state): State<AppState>,
    Path(subscription_id): Path<u64>,
    headers: HeaderMap,
    Json(request): Json<UnsubscribeRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .unsubscribe(&request.room_id, &request.participant_id, subscription_id)
        .map_err(|error| sfu_error(&error))?;
    state.metrics.subscriber_tracks.add(-1);
    Ok(StatusCode::NO_CONTENT)
}

async fn add_recording_sink(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RecordingSinkRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    state
        .sfu
        .add_recording_sink_for_ssrc(
            &request.room_id,
            request.track_id,
            request.destination,
            request.source_ssrc,
        )
        .map_err(|error| sfu_error(&error))?;
    if let Some(route) = state
        .sfu
        .request_keyframe(&request.room_id, request.track_id, request.source_ssrc)
        .map_err(|error| sfu_error(&error))?
    {
        state.metrics.pli_requests.increment();
        send_sfu_routes(
            &state.media_socket,
            &state.registry,
            &state.sfu,
            &state.metrics,
            state.epoch.elapsed(),
            vec![route],
        )
        .await;
    }
    Ok(StatusCode::CREATED)
}

async fn remove_recording_sink(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RecordingSinkRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    if state.sfu.remove_recording_sink_destination(
        &request.room_id,
        request.track_id,
        request.destination,
    ) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "recording_sink_not_found",
            message: "unknown recording sink".to_owned(),
        })
    }
}

async fn create_transcode_ingress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateTranscodeIngressRequest>,
) -> Result<(StatusCode, Json<CreateTranscodeIngressResponse>), ApiError> {
    authorize(&headers, &state.token)?;
    if request.ssrc == 0
        || request.max_bitrate_bps == 0
        || !(96..=127).contains(&request.payload_type)
    {
        return Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_transcode_ingress",
            message: "transcode SSRC, payload type, or bitrate is invalid".to_owned(),
        });
    }
    let socket = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|error| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "transcode_ingress_bind_failed",
            message: error.to_string(),
        })?;
    let destination = socket.local_addr().map_err(|error| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "transcode_ingress_bind_failed",
        message: error.to_string(),
    })?;
    state
        .sfu
        .publish_transcoded(PublishTrack {
            room_id: request.room_id.clone(),
            participant_id: request.participant_id.clone(),
            track_id: request.track_id,
            kind: parse_media_kind(&request.kind)?,
            codec: parse_codec(&request.codec)?,
            clock_rate: request.clock_rate,
            payload_type: request.payload_type,
            encodings: vec![Encoding {
                ssrc: request.ssrc,
                rid: None,
                spatial_layer: 0,
                max_bitrate_bps: request.max_bitrate_bps,
            }],
        })
        .map_err(|error| sfu_error(&error))?;
    let ingress_id = state
        .next_transcode_ingress_id
        .fetch_add(1, Ordering::Relaxed);
    if ingress_id == u64::MAX {
        let _ = state.sfu.unpublish(&request.room_id, request.track_id);
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "transcode_ingress_id_exhausted",
            message: "transcode ingress identifier exhausted".to_owned(),
        });
    }
    let (cancel, cancellation) = oneshot::channel();
    {
        let mut ingresses = state
            .transcode_ingresses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if ingresses.len() >= MAX_TRANSCODE_INGRESSES {
            let _ = state.sfu.unpublish(&request.room_id, request.track_id);
            return Err(ApiError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "transcode_ingress_capacity",
                message: "media node transcode ingress capacity reached".to_owned(),
            });
        }
        ingresses.insert(
            ingress_id,
            TranscodeIngress {
                room_id: request.room_id.clone(),
                track_id: request.track_id,
                cancellation: cancel,
            },
        );
    }
    let task_state = state.clone();
    tokio::spawn(async move {
        run_transcode_ingress(
            task_state,
            socket,
            request.room_id,
            request.participant_id,
            cancellation,
        )
        .await;
    });
    Ok((
        StatusCode::CREATED,
        Json(CreateTranscodeIngressResponse {
            ingress_id,
            destination,
        }),
    ))
}

async fn run_transcode_ingress(
    state: AppState,
    socket: UdpSocket,
    room_id: String,
    participant_id: String,
    mut cancellation: oneshot::Receiver<()>,
) {
    let mut buffer = vec![0_u8; MAX_DATAGRAM_BYTES];
    loop {
        tokio::select! {
            _ = &mut cancellation => break,
            received = socket.recv_from(&mut buffer) => {
                let Ok((length, remote)) = received else {
                    break;
                };
                if !remote.ip().is_loopback() {
                    continue;
                }
                if let Ok(routes) = state.sfu.handle_transcoded_rtp(
                    state.epoch.elapsed(),
                    &room_id,
                    &participant_id,
                    &buffer[..length],
                ) {
                    send_sfu_routes(
                        &state.media_socket,
                        &state.registry,
                        &state.sfu,
                        &state.metrics,
                        state.epoch.elapsed(),
                        routes,
                    )
                    .await;
                }
            }
        }
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

async fn delete_transcode_ingress(
    State(state): State<AppState>,
    Path(ingress_id): Path<u64>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    let ingress = state
        .transcode_ingresses
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&ingress_id)
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "transcode_ingress_not_found",
            message: "unknown transcode ingress".to_owned(),
        })?;
    let _ = ingress.cancellation.send(());
    let _ = state.sfu.unpublish(&ingress.room_id, ingress.track_id);
    Ok(StatusCode::NO_CONTENT)
}

async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, ApiError> {
    authorize(&headers, &state.token)?;
    let snapshot = state
        .registry
        .session_snapshot(&session_id)
        .ok_or(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "session_not_found",
            message: "unknown media session".to_owned(),
        })?;
    Ok(Json(SessionResponse {
        session_id: snapshot.session_id,
        room_id: snapshot.room_id,
        participant_id: snapshot.participant_id,
        state: match snapshot.state {
            fluvora_rtc_session::SessionState::New => "new",
            fluvora_rtc_session::SessionState::DtlsHandshaking => "dtls_handshaking",
            fluvora_rtc_session::SessionState::Connected => "connected",
            fluvora_rtc_session::SessionState::Disconnected => "disconnected",
            fluvora_rtc_session::SessionState::Failed => "failed",
            fluvora_rtc_session::SessionState::Closed => "closed",
        },
    }))
}

async fn delete_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    authorize(&headers, &state.token)?;
    let snapshot = state.registry.session_snapshot(&session_id);
    if state.registry.remove(&session_id) {
        remove_crypto_session(&state.crypto, &session_id);
        remove_data_channel_session(&state.crypto, &session_id);
        if let Some(snapshot) = snapshot {
            let _ =
                state
                    .sfu
                    .unbind_session(&snapshot.room_id, &snapshot.participant_id, &session_id);
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "session_not_found",
            message: "unknown media session".to_owned(),
        })
    }
}

#[cfg(feature = "openssl-backend")]
fn remove_data_channel_session(crypto: &CryptoRuntime, session_id: &str) {
    let removed = crypto
        .data_channels
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
    if let Some(removed) = removed {
        crypto.metrics.active_data_channel_associations.add(-1);
        crypto
            .metrics
            .active_data_channels
            .add(-i64::try_from(removed.stream_labels.len()).unwrap_or(i64::MAX));
    }
}

#[cfg(not(feature = "openssl-backend"))]
const fn remove_data_channel_session(_crypto: &CryptoRuntime, _session_id: &str) {}

#[cfg(feature = "openssl-backend")]
fn remove_crypto_session(crypto: &CryptoRuntime, session_id: &str) {
    crypto
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id);
}

#[cfg(not(feature = "openssl-backend"))]
const fn remove_crypto_session(_crypto: &CryptoRuntime, _session_id: &str) {}

fn authorize(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied == Some(expected) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "invalid media control bearer token".to_owned(),
        })
    }
}

fn registry_error(error: &RegistryError) -> ApiError {
    let status = match error {
        RegistryError::Capacity => StatusCode::SERVICE_UNAVAILABLE,
        RegistryError::DuplicateSession | RegistryError::DuplicateUsernameFragment => {
            StatusCode::CONFLICT
        }
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    ApiError {
        status,
        code: "session_provision_failed",
        message: error.to_string(),
    }
}

fn sfu_error(error: &fluvora_media_node::SfuRuntimeError) -> ApiError {
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "sfu_operation_failed",
        message: error.to_string(),
    }
}

fn parse_media_kind(value: &str) -> Result<MediaKind, ApiError> {
    match value {
        "audio" => Ok(MediaKind::Audio),
        "video" => Ok(MediaKind::Video),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_media_kind",
            message: "media kind must be audio or video".to_owned(),
        }),
    }
}

fn parse_codec(value: &str) -> Result<Codec, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "opus" => Ok(Codec::Opus),
        "vp8" => Ok(Codec::Vp8),
        "vp9" => Ok(Codec::Vp9),
        "h264" => Ok(Codec::H264),
        "av1" => Ok(Codec::Av1),
        _ => Err(ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "unsupported_codec",
            message: "codec must be opus, vp8, vp9, h264, or av1".to_owned(),
        }),
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DATA_CHANNEL_MESSAGE_BYTES, MAX_DATA_CHANNEL_PAYLOAD_BYTES};
    use fluvora_protocol::{DataKind, Envelope, EnvelopeFlags};

    #[test]
    fn authoritative_envelope_limit_includes_the_wire_header() {
        let envelope = Envelope {
            flags: EnvelopeFlags::new(true, true, false),
            kind: DataKind::Chat,
            room_id: 1,
            sender_id: 2,
            sequence: 3,
            timestamp_millis: 4,
            payload: vec![0; MAX_DATA_CHANNEL_PAYLOAD_BYTES],
        };
        let encoded = envelope
            .encode(MAX_DATA_CHANNEL_PAYLOAD_BYTES)
            .expect("maximum authoritative envelope");
        assert_eq!(encoded.len(), MAX_DATA_CHANNEL_MESSAGE_BYTES);
    }
}
