use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use fluvora_stun::{
    AttributeType, Message, MessageBuilder, MessageClass, MessageType, Method, StunError,
    TransactionId,
};
use fluvora_turn::{
    Allocation, ChannelData, PeerRoute, TurnError, long_term_key, rest_credential_password,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_rustls::TlsAcceptor;

use crate::nonce::{NonceError, NonceManager};

const MAX_DATAGRAM_BYTES: usize = 65_535;
const MAX_RELAY_DATA_BYTES: usize = 65_000;
const SOFTWARE: &str = "Fluvora TURN 0.1";
const TCP_WRITE_QUEUE: usize = 256;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub control_bind: SocketAddr,
    pub status_bind: SocketAddr,
    pub advertised_ip: IpAddr,
    pub relay_bind_ip: IpAddr,
    pub relay_port_min: u16,
    pub relay_port_max: u16,
    pub allow_private_peers: bool,
    pub maximum_relay_bytes_per_second: u64,
    pub username: String,
    pub password: String,
    pub realm: String,
    pub nonce_secret: Vec<u8>,
    pub rest_secret: Vec<u8>,
    pub maximum_allocations: usize,
    pub maximum_allocations_per_ip: usize,
    pub tls: Option<TlsConfiguration>,
}

#[derive(Debug, Clone)]
pub struct TlsConfiguration {
    pub bind: SocketAddr,
    pub certificate_pem: PathBuf,
    pub private_key_pem: PathBuf,
}

struct ManagedAllocation {
    allocation: Allocation,
    relay_socket: Arc<UdpSocket>,
    cancellation: oneshot::Sender<()>,
    allocate_transaction: TransactionId,
    allocate_response: Vec<u8>,
    control_sender: ClientSender,
    bandwidth: BandwidthLimiter,
}

#[derive(Debug)]
struct BandwidthLimiter {
    available_bytes: u64,
    updated_at: Duration,
}

impl BandwidthLimiter {
    const fn new(bytes_per_second: u64, now: Duration) -> Self {
        Self {
            available_bytes: bytes_per_second.saturating_mul(2),
            updated_at: now,
        }
    }

    fn consume(&mut self, now: Duration, bytes: usize, bytes_per_second: u64) -> bool {
        let elapsed = now.saturating_sub(self.updated_at);
        let replenished =
            u128::from(bytes_per_second).saturating_mul(elapsed.as_nanos()) / 1_000_000_000;
        if replenished != 0 {
            self.available_bytes = self
                .available_bytes
                .saturating_add(u64::try_from(replenished).unwrap_or(u64::MAX))
                .min(bytes_per_second.saturating_mul(2));
            self.updated_at = now;
        }
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        if bytes > self.available_bytes {
            return false;
        }
        self.available_bytes -= bytes;
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ClientKey {
    Udp(SocketAddr),
    Tcp {
        connection_id: u64,
        address: SocketAddr,
    },
}

impl ClientKey {
    const fn address(&self) -> SocketAddr {
        match self {
            Self::Udp(address) | Self::Tcp { address, .. } => *address,
        }
    }
}

#[derive(Debug, Clone)]
enum ClientSender {
    Udp {
        socket: Arc<UdpSocket>,
        address: SocketAddr,
    },
    Tcp {
        sender: mpsc::Sender<Vec<u8>>,
    },
}

#[derive(Debug, Default)]
struct Metrics {
    allocations_created: AtomicU64,
    allocations_rejected: AtomicU64,
    client_bytes_relayed: AtomicU64,
    peer_bytes_relayed: AtomicU64,
    dropped_datagrams: AtomicU64,
    authentication_failures: AtomicU64,
    tcp_connections_active: AtomicU64,
    tls_connections_active: AtomicU64,
    tls_handshake_failures: AtomicU64,
    rate_limited_datagrams: AtomicU64,
}

pub struct TurnServer {
    configuration: ServerConfig,
    control_socket: Arc<UdpSocket>,
    tcp_listener: TcpListener,
    tls_listener: Option<TcpListener>,
    tls_acceptor: Option<TlsAcceptor>,
    allocations: Mutex<HashMap<ClientKey, ManagedAllocation>>,
    nonce: NonceManager,
    credential_key: [u8; 16],
    epoch: Instant,
    metrics: Metrics,
    next_tcp_connection_id: AtomicU64,
    next_relay_port: AtomicU16,
}

impl fmt::Debug for TurnServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnServer")
            .field("configuration", &self.configuration)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl TurnServer {
    pub async fn bind(configuration: ServerConfig) -> Result<Self, ServerError> {
        if !same_family(configuration.advertised_ip, configuration.relay_bind_ip)
            || configuration.maximum_allocations == 0
            || configuration.maximum_allocations_per_ip == 0
            || configuration.maximum_allocations_per_ip > configuration.maximum_allocations
            || configuration.maximum_relay_bytes_per_second
                < u64::try_from(MAX_RELAY_DATA_BYTES).unwrap_or(u64::MAX)
            || !valid_relay_port_range(
                configuration.relay_port_min,
                configuration.relay_port_max,
                configuration.maximum_allocations,
            )
        {
            return Err(ServerError::InvalidConfiguration);
        }
        let control_socket = Arc::new(UdpSocket::bind(configuration.control_bind).await?);
        let tcp_listener = TcpListener::bind(configuration.control_bind).await?;
        let (tls_listener, tls_acceptor) = if let Some(tls) = &configuration.tls {
            let listener = TcpListener::bind(tls.bind).await?;
            let acceptor = load_tls_acceptor(tls)?;
            (Some(listener), Some(acceptor))
        } else {
            (None, None)
        };
        let nonce = NonceManager::new(configuration.nonce_secret.clone(), Duration::from_mins(10))
            .map_err(|_| ServerError::InvalidConfiguration)?;
        let credential_key = long_term_key(
            &configuration.username,
            &configuration.realm,
            &configuration.password,
        );
        let first_relay_port = configuration.relay_port_min;
        Ok(Self {
            configuration,
            control_socket,
            tcp_listener,
            tls_listener,
            tls_acceptor,
            allocations: Mutex::new(HashMap::new()),
            nonce,
            credential_key,
            epoch: Instant::now(),
            metrics: Metrics::default(),
            next_tcp_connection_id: AtomicU64::new(1),
            next_relay_port: AtomicU16::new(first_relay_port),
        })
    }

    /// Returns the exact current allocation count.
    pub async fn active_allocations(&self) -> usize {
        self.allocations.lock().await.len()
    }

    /// Returns the configured global allocation limit.
    #[must_use]
    pub const fn allocation_limit(&self) -> usize {
        self.configuration.maximum_allocations
    }

    pub async fn run<F>(self: Arc<Self>, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send,
    {
        let status_listener = tokio::net::TcpListener::bind(self.configuration.status_bind).await?;
        let status_router = Router::new()
            .route("/health/live", get(live))
            .route("/health/ready", get(live))
            .route("/metrics", get(metrics))
            .with_state(Arc::clone(&self));
        let control = Arc::clone(&self).control_loop();
        let tcp_control = Arc::clone(&self).tcp_control_loop();
        let tls_control = Arc::clone(&self).tls_control_loop();
        let status = axum::serve(status_listener, status_router);
        tokio::select! {
            result = control => result,
            result = tcp_control => result,
            result = tls_control => result,
            result = status => result.map_err(ServerError::Io),
            () = shutdown => Ok(()),
        }
    }

    async fn control_loop(self: Arc<Self>) -> Result<(), ServerError> {
        let mut buffer = vec![0_u8; MAX_DATAGRAM_BYTES];
        let mut maintenance = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                received = self.control_socket.recv_from(&mut buffer) => {
                    let (length, client) = received?;
                    self.handle_client_frame(
                        ClientKey::Udp(client),
                        ClientSender::Udp {
                            socket: Arc::clone(&self.control_socket),
                            address: client,
                        },
                        &buffer[..length],
                    ).await;
                }
                _ = maintenance.tick() => self.remove_expired().await,
            }
        }
    }

    async fn tcp_control_loop(self: Arc<Self>) -> Result<(), ServerError> {
        loop {
            let (stream, address) = self.tcp_listener.accept().await?;
            let connection_id = self.next_tcp_connection_id.fetch_add(1, Ordering::Relaxed);
            if connection_id == u64::MAX {
                return Err(ServerError::ConnectionIdExhausted);
            }
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                server
                    .metrics
                    .tcp_connections_active
                    .fetch_add(1, Ordering::Relaxed);
                server
                    .clone()
                    .handle_tcp_connection(connection_id, address, stream)
                    .await;
                server
                    .metrics
                    .tcp_connections_active
                    .fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    async fn tls_control_loop(self: Arc<Self>) -> Result<(), ServerError> {
        let (Some(listener), Some(acceptor)) = (&self.tls_listener, &self.tls_acceptor) else {
            return std::future::pending().await;
        };
        loop {
            let (stream, address) = listener.accept().await?;
            let acceptor = acceptor.clone();
            let connection_id = self.next_tcp_connection_id.fetch_add(1, Ordering::Relaxed);
            if connection_id == u64::MAX {
                return Err(ServerError::ConnectionIdExhausted);
            }
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                server
                    .metrics
                    .tls_connections_active
                    .fetch_add(1, Ordering::Relaxed);
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
                    Ok(Ok(stream)) => {
                        server
                            .clone()
                            .handle_control_stream(connection_id, address, stream)
                            .await;
                    }
                    Ok(Err(_)) | Err(_) => {
                        server
                            .metrics
                            .tls_handshake_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                server
                    .metrics
                    .tls_connections_active
                    .fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    async fn handle_tcp_connection(
        self: Arc<Self>,
        connection_id: u64,
        address: SocketAddr,
        stream: TcpStream,
    ) {
        self.handle_control_stream(connection_id, address, stream)
            .await;
    }

    async fn handle_control_stream<Stream>(
        self: Arc<Self>,
        connection_id: u64,
        address: SocketAddr,
        stream: Stream,
    ) where
        Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let key = ClientKey::Tcp {
            connection_id,
            address,
        };
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(TCP_WRITE_QUEUE);
        let write_task = tokio::spawn(async move {
            while let Some(frame) = receiver.recv().await {
                if writer.write_all(&frame).await.is_err() {
                    break;
                }
            }
        });
        let client_sender = ClientSender::Tcp { sender };
        while let Ok(Some(frame)) = read_turn_tcp_frame(&mut reader).await {
            self.handle_client_frame(key.clone(), client_sender.clone(), &frame)
                .await;
        }
        if let Some(allocation) = self.allocations.lock().await.remove(&key) {
            let _ = allocation.cancellation.send(());
        }
        drop(client_sender);
        let _ = write_task.await;
    }

    async fn handle_client_frame(
        self: &Arc<Self>,
        client: ClientKey,
        sender: ClientSender,
        datagram: &[u8],
    ) {
        let result = if datagram.first().is_some_and(|first| first & 0xc0 == 0x40) {
            self.handle_channel_data(&client, datagram).await
        } else {
            self.handle_stun(&client, &sender, datagram).await
        };
        if result.is_err() {
            self.metrics
                .dropped_datagrams
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn handle_stun(
        self: &Arc<Self>,
        client: &ClientKey,
        sender: &ClientSender,
        datagram: &[u8],
    ) -> Result<(), ServerError> {
        let message = Message::parse(datagram)?;
        let kind = message.message_type();
        let response = if kind.method() == Method::BINDING && kind.class() == MessageClass::Request
        {
            Some(binding_response(&message, client.address())?)
        } else if kind.class() == MessageClass::Indication && kind.method() == Method::SEND {
            self.handle_send_indication(client, &message).await?;
            None
        } else if kind.class() == MessageClass::Request {
            Some(
                self.handle_authenticated_request(client, sender, &message)
                    .await?,
            )
        } else {
            None
        };
        if let Some(response) = response {
            sender.send(response).await?;
        }
        Ok(())
    }

    async fn handle_authenticated_request(
        self: &Arc<Self>,
        client: &ClientKey,
        sender: &ClientSender,
        message: &Message<'_>,
    ) -> Result<Vec<u8>, ServerError> {
        let authentication = match self.authenticate(client.address(), message) {
            Ok(authentication) => authentication,
            Err(rejection) => {
                self.metrics
                    .authentication_failures
                    .fetch_add(1, Ordering::Relaxed);
                return self.challenge_response(client.address(), message, rejection);
            }
        };
        let unknown = message.unknown_required_attributes();
        if !unknown.is_empty() {
            return authenticated_error(
                message,
                420,
                "Unknown Attribute",
                Some(&authentication.key),
                Some(&unknown),
            );
        }
        match message.message_type().method() {
            Method::ALLOCATE => {
                self.handle_allocate(client, sender, message, &authentication)
                    .await
            }
            Method::REFRESH => self.handle_refresh(client, message, &authentication).await,
            Method::CREATE_PERMISSION => {
                self.handle_create_permission(client, message, &authentication)
                    .await
            }
            Method::CHANNEL_BIND => {
                self.handle_channel_bind(client, message, &authentication)
                    .await
            }
            _ => authenticated_error(message, 400, "Bad Request", Some(&authentication.key), None),
        }
    }

    fn authenticate(
        &self,
        client: SocketAddr,
        message: &Message<'_>,
    ) -> Result<Authentication, AuthRejection> {
        let username = message
            .username()
            .map_err(|_| AuthRejection::Unauthorized)?
            .ok_or(AuthRejection::Unauthorized)?;
        let realm = message
            .realm()
            .map_err(|_| AuthRejection::Unauthorized)?
            .ok_or(AuthRejection::Unauthorized)?;
        let nonce = message
            .nonce()
            .map_err(|_| AuthRejection::Unauthorized)?
            .ok_or(AuthRejection::Unauthorized)?;
        if realm != self.configuration.realm {
            return Err(AuthRejection::Unauthorized);
        }
        let key = self
            .credential_key(username)
            .ok_or(AuthRejection::Unauthorized)?;
        match self.nonce.validate(nonce, unix_seconds(), client.ip()) {
            Ok(()) => {}
            Err(NonceError::Stale) => return Err(AuthRejection::StaleNonce),
            Err(_) => return Err(AuthRejection::Unauthorized),
        }
        message
            .verify_message_integrity_sha1(&key)
            .map_err(|_| AuthRejection::Unauthorized)?;
        if message.attribute(AttributeType::FINGERPRINT).is_some() {
            message
                .verify_fingerprint()
                .map_err(|_| AuthRejection::Unauthorized)?;
        }
        Ok(Authentication {
            username: username.to_owned(),
            key,
        })
    }

    fn credential_key(&self, username: &str) -> Option<[u8; 16]> {
        if username == self.configuration.username {
            return Some(self.credential_key);
        }
        let (expires, subject) = username.split_once(':')?;
        let expires = expires.parse::<u64>().ok()?;
        let now = unix_seconds();
        if subject.is_empty()
            || subject.len() > 128
            || !subject.bytes().all(|byte| byte.is_ascii_hexdigit())
            || expires <= now
            || expires > now.saturating_add(86_400)
        {
            return None;
        }
        let password = rest_credential_password(&self.configuration.rest_secret, username).ok()?;
        Some(long_term_key(
            username,
            &self.configuration.realm,
            &password,
        ))
    }

    fn challenge_response(
        &self,
        client: SocketAddr,
        message: &Message<'_>,
        rejection: AuthRejection,
    ) -> Result<Vec<u8>, ServerError> {
        let (code, reason) = match rejection {
            AuthRejection::Unauthorized => (401, "Unauthorized"),
            AuthRejection::StaleNonce => (438, "Stale Nonce"),
        };
        let nonce = self
            .nonce
            .issue(unix_seconds(), client.ip())
            .map_err(|_| ServerError::InvalidConfiguration)?;
        Ok(MessageBuilder::new(
            MessageType::new(message.message_type().method(), MessageClass::ErrorResponse),
            message.transaction_id(),
        )
        .error_code(code, reason)?
        .raw_attribute(
            AttributeType::REALM,
            self.configuration.realm.as_bytes().to_vec(),
        )
        .raw_attribute(AttributeType::NONCE, nonce.into_bytes())
        .software(SOFTWARE)
        .fingerprint()
        .build()?)
    }

    async fn handle_allocate(
        self: &Arc<Self>,
        client: &ClientKey,
        sender: &ClientSender,
        message: &Message<'_>,
        authentication: &Authentication,
    ) -> Result<Vec<u8>, ServerError> {
        if requested_transport(message)? != Some(17) {
            return authenticated_error(
                message,
                442,
                "Unsupported Transport Protocol",
                Some(&authentication.key),
                None,
            );
        }
        {
            let allocations = self.allocations.lock().await;
            if let Some(existing) = allocations.get(client) {
                if existing.allocate_transaction == message.transaction_id() {
                    return Ok(existing.allocate_response.clone());
                }
                return authenticated_error(
                    message,
                    437,
                    "Allocation Mismatch",
                    Some(&authentication.key),
                    None,
                );
            }
            let from_ip = allocations
                .keys()
                .filter(|key| key.address().ip() == client.address().ip())
                .count();
            if allocations.len() >= self.configuration.maximum_allocations
                || from_ip >= self.configuration.maximum_allocations_per_ip
            {
                self.metrics
                    .allocations_rejected
                    .fetch_add(1, Ordering::Relaxed);
                return authenticated_error(
                    message,
                    486,
                    "Allocation Quota Reached",
                    Some(&authentication.key),
                    None,
                );
            }
        }
        let relay_socket = self.bind_relay_socket().await?;
        let relay_port = relay_socket.local_addr()?.port();
        let relayed_address = SocketAddr::new(self.configuration.advertised_ip, relay_port);
        let now = self.epoch.elapsed();
        let allocation = Allocation::new(
            now,
            authentication.username.clone(),
            client.address(),
            relayed_address,
        );
        let response = allocation_success(
            message,
            relayed_address,
            client.address(),
            allocation.remaining_lifetime(now),
            &authentication.key,
        )?;
        let (cancellation, cancelled) = oneshot::channel();
        {
            let mut allocations = self.allocations.lock().await;
            if allocations.contains_key(client) {
                return authenticated_error(
                    message,
                    437,
                    "Allocation Mismatch",
                    Some(&authentication.key),
                    None,
                );
            }
            allocations.insert(
                client.clone(),
                ManagedAllocation {
                    allocation,
                    relay_socket: Arc::clone(&relay_socket),
                    cancellation,
                    allocate_transaction: message.transaction_id(),
                    allocate_response: response.clone(),
                    control_sender: sender.clone(),
                    bandwidth: BandwidthLimiter::new(
                        self.configuration.maximum_relay_bytes_per_second,
                        now,
                    ),
                },
            );
        }
        self.metrics
            .allocations_created
            .fetch_add(1, Ordering::Relaxed);
        let relay_server = Arc::clone(self);
        let relay_client = client.clone();
        tokio::spawn(async move {
            relay_server
                .relay_peer_datagrams(relay_client, relay_socket, cancelled)
                .await;
        });
        Ok(response)
    }

    async fn bind_relay_socket(&self) -> Result<Arc<UdpSocket>, ServerError> {
        if self.configuration.relay_port_min == 0 {
            return Ok(Arc::new(
                UdpSocket::bind(SocketAddr::new(self.configuration.relay_bind_ip, 0)).await?,
            ));
        }
        let port_count = self
            .configuration
            .relay_port_max
            .saturating_sub(self.configuration.relay_port_min)
            .saturating_add(1);
        for _ in 0..port_count {
            let candidate = self.next_relay_port.fetch_add(1, Ordering::Relaxed);
            let offset = candidate.wrapping_sub(self.configuration.relay_port_min) % port_count;
            let port = self.configuration.relay_port_min + offset;
            match UdpSocket::bind(SocketAddr::new(self.configuration.relay_bind_ip, port)).await {
                Ok(socket) => return Ok(Arc::new(socket)),
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
                Err(error) => return Err(ServerError::Io(error)),
            }
        }
        Err(ServerError::Io(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "TURN relay UDP port range is exhausted",
        )))
    }

    async fn handle_refresh(
        &self,
        client: &ClientKey,
        message: &Message<'_>,
        authentication: &Authentication,
    ) -> Result<Vec<u8>, ServerError> {
        let requested_lifetime = u32_attribute(message, AttributeType::LIFETIME)?;
        let mut allocations = self.allocations.lock().await;
        let Some(existing) = allocations.get_mut(client) else {
            return authenticated_error(
                message,
                437,
                "Allocation Mismatch",
                Some(&authentication.key),
                None,
            );
        };
        if existing.allocation.username != authentication.username {
            return authenticated_error(
                message,
                441,
                "Wrong Credentials",
                Some(&authentication.key),
                None,
            );
        }
        let lifetime = if requested_lifetime == Some(0) {
            0
        } else {
            existing
                .allocation
                .refresh(self.epoch.elapsed(), requested_lifetime)
        };
        let response = lifetime_success(message, lifetime, &authentication.key)?;
        if lifetime == 0
            && let Some(removed) = allocations.remove(client)
        {
            let _ = removed.cancellation.send(());
        }
        Ok(response)
    }

    async fn handle_create_permission(
        &self,
        client: &ClientKey,
        message: &Message<'_>,
        authentication: &Authentication,
    ) -> Result<Vec<u8>, ServerError> {
        let peers = message.xor_addresses(AttributeType::XOR_PEER_ADDRESS)?;
        if peers.is_empty() || peers.iter().any(|peer| !self.safe_peer(*peer)) {
            return authenticated_error(
                message,
                400,
                "Bad Request",
                Some(&authentication.key),
                None,
            );
        }
        let mut allocations = self.allocations.lock().await;
        let Some(existing) = allocations.get_mut(client) else {
            return authenticated_error(
                message,
                437,
                "Allocation Mismatch",
                Some(&authentication.key),
                None,
            );
        };
        if existing.allocation.username != authentication.username {
            return authenticated_error(
                message,
                441,
                "Wrong Credentials",
                Some(&authentication.key),
                None,
            );
        }
        match existing
            .allocation
            .create_permissions(self.epoch.elapsed(), &peers)
        {
            Ok(()) => empty_success(message, &authentication.key),
            Err(TurnError::PeerAddressFamilyMismatch) => authenticated_error(
                message,
                443,
                "Peer Address Family Mismatch",
                Some(&authentication.key),
                None,
            ),
            Err(TurnError::Capacity) => authenticated_error(
                message,
                508,
                "Insufficient Capacity",
                Some(&authentication.key),
                None,
            ),
            Err(error) => Err(ServerError::Turn(error)),
        }
    }

    async fn handle_channel_bind(
        &self,
        client: &ClientKey,
        message: &Message<'_>,
        authentication: &Authentication,
    ) -> Result<Vec<u8>, ServerError> {
        let channel = channel_number(message)?;
        let peer = message.xor_address(AttributeType::XOR_PEER_ADDRESS)?;
        let (Some(channel), Some(peer)) = (channel, peer) else {
            return authenticated_error(
                message,
                400,
                "Bad Request",
                Some(&authentication.key),
                None,
            );
        };
        if !self.safe_peer(peer) {
            return authenticated_error(
                message,
                403,
                "Forbidden Peer Address",
                Some(&authentication.key),
                None,
            );
        }
        let mut allocations = self.allocations.lock().await;
        let Some(existing) = allocations.get_mut(client) else {
            return authenticated_error(
                message,
                437,
                "Allocation Mismatch",
                Some(&authentication.key),
                None,
            );
        };
        if existing.allocation.username != authentication.username {
            return authenticated_error(
                message,
                441,
                "Wrong Credentials",
                Some(&authentication.key),
                None,
            );
        }
        match existing
            .allocation
            .bind_channel(self.epoch.elapsed(), channel, peer)
        {
            Ok(()) => empty_success(message, &authentication.key),
            Err(TurnError::PeerAddressFamilyMismatch) => authenticated_error(
                message,
                443,
                "Peer Address Family Mismatch",
                Some(&authentication.key),
                None,
            ),
            Err(TurnError::Capacity) => authenticated_error(
                message,
                508,
                "Insufficient Capacity",
                Some(&authentication.key),
                None,
            ),
            Err(TurnError::InvalidChannelNumber(_) | TurnError::ChannelConflict) => {
                authenticated_error(message, 400, "Bad Request", Some(&authentication.key), None)
            }
            Err(error) => Err(ServerError::Turn(error)),
        }
    }

    async fn handle_send_indication(
        &self,
        client: &ClientKey,
        message: &Message<'_>,
    ) -> Result<(), ServerError> {
        let Some(peer) = message.xor_address(AttributeType::XOR_PEER_ADDRESS)? else {
            return Ok(());
        };
        let Some(data_attribute) = message.attribute(AttributeType::DATA) else {
            return Ok(());
        };
        let data = data_attribute.value();
        if data.len() > MAX_RELAY_DATA_BYTES || !self.safe_peer(peer) {
            return Ok(());
        }
        let route = {
            let mut allocations = self.allocations.lock().await;
            let Some(existing) = allocations.get_mut(client) else {
                return Ok(());
            };
            let now = self.epoch.elapsed();
            if !existing.allocation.permits(now, peer) {
                None
            } else if existing.bandwidth.consume(
                now,
                data.len(),
                self.configuration.maximum_relay_bytes_per_second,
            ) {
                Some((Arc::clone(&existing.relay_socket), peer))
            } else {
                self.metrics
                    .rate_limited_datagrams
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
        };
        if let Some((socket, destination)) = route {
            socket.send_to(data, destination).await?;
            self.metrics.client_bytes_relayed.fetch_add(
                u64::try_from(data.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    async fn handle_channel_data(
        &self,
        client: &ClientKey,
        datagram: &[u8],
    ) -> Result<(), ServerError> {
        let channel_data = ChannelData::parse(datagram)?;
        if channel_data.data.len() > MAX_RELAY_DATA_BYTES {
            return Err(ServerError::Turn(TurnError::DataTooLarge));
        }
        let route = {
            let mut allocations = self.allocations.lock().await;
            let Some(existing) = allocations.get_mut(client) else {
                return Ok(());
            };
            let now = self.epoch.elapsed();
            let peer = existing
                .allocation
                .channel_peer(now, channel_data.channel_number);
            match peer {
                Some(peer)
                    if existing.bandwidth.consume(
                        now,
                        channel_data.data.len(),
                        self.configuration.maximum_relay_bytes_per_second,
                    ) =>
                {
                    Some((Arc::clone(&existing.relay_socket), peer))
                }
                Some(_) => {
                    self.metrics
                        .rate_limited_datagrams
                        .fetch_add(1, Ordering::Relaxed);
                    None
                }
                None => None,
            }
        };
        if let Some((socket, peer)) = route {
            socket.send_to(channel_data.data, peer).await?;
            self.metrics.client_bytes_relayed.fetch_add(
                u64::try_from(channel_data.data.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    async fn relay_peer_datagrams(
        self: Arc<Self>,
        client: ClientKey,
        relay_socket: Arc<UdpSocket>,
        mut cancellation: oneshot::Receiver<()>,
    ) {
        let mut buffer = vec![0_u8; MAX_DATAGRAM_BYTES];
        loop {
            let received = tokio::select! {
                _ = &mut cancellation => break,
                received = relay_socket.recv_from(&mut buffer) => received,
            };
            let Ok((length, peer)) = received else {
                break;
            };
            if length > MAX_RELAY_DATA_BYTES {
                self.metrics
                    .dropped_datagrams
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let route = {
                let mut allocations = self.allocations.lock().await;
                allocations.get_mut(&client).and_then(|existing| {
                    let now = self.epoch.elapsed();
                    let route = existing.allocation.peer_route(now, peer)?;
                    if !existing.bandwidth.consume(
                        now,
                        length,
                        self.configuration.maximum_relay_bytes_per_second,
                    ) {
                        self.metrics
                            .rate_limited_datagrams
                            .fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                    Some((route, existing.control_sender.clone()))
                })
            };
            let Some((route, sender)) = route else {
                continue;
            };
            let packet = match route {
                PeerRoute::Channel(channel) => {
                    ChannelData::encode(channel, &buffer[..length]).map_err(ServerError::Turn)
                }
                PeerRoute::DataIndication => {
                    data_indication(peer, &buffer[..length]).map_err(ServerError::Stun)
                }
            };
            let Ok(packet) = packet else {
                continue;
            };
            if sender.send(packet).await.is_ok() {
                self.metrics
                    .peer_bytes_relayed
                    .fetch_add(u64::try_from(length).unwrap_or(u64::MAX), Ordering::Relaxed);
            }
        }
    }

    async fn remove_expired(&self) {
        let now = self.epoch.elapsed();
        let removed = {
            let mut allocations = self.allocations.lock().await;
            let expired = allocations
                .iter()
                .filter_map(|(client, allocation)| {
                    allocation.allocation.expired(now).then_some(client.clone())
                })
                .collect::<Vec<_>>();
            expired
                .into_iter()
                .filter_map(|client| allocations.remove(&client))
                .collect::<Vec<_>>()
        };
        for allocation in removed {
            let _ = allocation.cancellation.send(());
        }
    }

    fn safe_peer(&self, peer: SocketAddr) -> bool {
        if peer.port() == 0
            || peer.ip().is_unspecified()
            || is_multicast(peer.ip())
            || (!self.configuration.allow_private_peers && is_private_peer(peer.ip()))
            || self.is_server_endpoint(peer)
        {
            return false;
        }
        true
    }

    fn is_server_endpoint(&self, peer: SocketAddr) -> bool {
        let local_address = peer.ip() == self.configuration.advertised_ip
            || concrete_ip_matches(peer.ip(), self.configuration.control_bind.ip())
            || concrete_ip_matches(peer.ip(), self.configuration.status_bind.ip())
            || concrete_ip_matches(peer.ip(), self.configuration.relay_bind_ip)
            || self
                .configuration
                .tls
                .as_ref()
                .is_some_and(|tls| concrete_ip_matches(peer.ip(), tls.bind.ip()));
        if !local_address {
            return false;
        }
        if peer.port() == self.configuration.control_bind.port()
            || peer.port() == self.configuration.status_bind.port()
            || self
                .configuration
                .tls
                .as_ref()
                .is_some_and(|tls| peer.port() == tls.bind.port())
        {
            return true;
        }
        self.configuration.relay_port_min != 0
            && (self.configuration.relay_port_min..=self.configuration.relay_port_max)
                .contains(&peer.port())
    }

    async fn render_metrics(&self) -> String {
        let active = self.allocations.lock().await.len();
        format!(
            "# TYPE fluvora_turn_active_allocations gauge\n\
             fluvora_turn_active_allocations {active}\n\
             # TYPE fluvora_turn_allocation_limit gauge\n\
             fluvora_turn_allocation_limit {}\n\
             # TYPE fluvora_turn_allocations_created_total counter\n\
             fluvora_turn_allocations_created_total {}\n\
             # TYPE fluvora_turn_allocations_rejected_total counter\n\
             fluvora_turn_allocations_rejected_total {}\n\
             # TYPE fluvora_turn_client_bytes_relayed_total counter\n\
             fluvora_turn_client_bytes_relayed_total {}\n\
             # TYPE fluvora_turn_peer_bytes_relayed_total counter\n\
             fluvora_turn_peer_bytes_relayed_total {}\n\
             # TYPE fluvora_turn_dropped_datagrams_total counter\n\
             fluvora_turn_dropped_datagrams_total {}\n\
             # TYPE fluvora_turn_authentication_failures_total counter\n\
             fluvora_turn_authentication_failures_total {}\n\
             # TYPE fluvora_turn_tcp_connections_active gauge\n\
             fluvora_turn_tcp_connections_active {}\n\
             # TYPE fluvora_turn_tls_connections_active gauge\n\
             fluvora_turn_tls_connections_active {}\n\
             # TYPE fluvora_turn_tls_handshake_failures_total counter\n\
             fluvora_turn_tls_handshake_failures_total {}\n\
             # TYPE fluvora_turn_rate_limited_datagrams_total counter\n\
             fluvora_turn_rate_limited_datagrams_total {}\n",
            self.configuration.maximum_allocations,
            self.metrics.allocations_created.load(Ordering::Relaxed),
            self.metrics.allocations_rejected.load(Ordering::Relaxed),
            self.metrics.client_bytes_relayed.load(Ordering::Relaxed),
            self.metrics.peer_bytes_relayed.load(Ordering::Relaxed),
            self.metrics.dropped_datagrams.load(Ordering::Relaxed),
            self.metrics.authentication_failures.load(Ordering::Relaxed),
            self.metrics.tcp_connections_active.load(Ordering::Relaxed),
            self.metrics.tls_connections_active.load(Ordering::Relaxed),
            self.metrics.tls_handshake_failures.load(Ordering::Relaxed),
            self.metrics.rate_limited_datagrams.load(Ordering::Relaxed),
        )
    }
}

pub(crate) async fn shutdown_signal() {
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

fn concrete_ip_matches(candidate: IpAddr, configured: IpAddr) -> bool {
    !configured.is_unspecified() && candidate == configured
}

fn load_tls_acceptor(configuration: &TlsConfiguration) -> Result<TlsAcceptor, ServerError> {
    use tokio_rustls::rustls::pki_types::pem::PemObject as _;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let certificates = CertificateDer::pem_file_iter(&configuration.certificate_pem)
        .map_err(|_| ServerError::InvalidConfiguration)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ServerError::InvalidConfiguration)?;
    if certificates.is_empty() {
        return Err(ServerError::InvalidConfiguration);
    }
    let private_key = PrivateKeyDer::from_pem_file(&configuration.private_key_pem)
        .map_err(|_| ServerError::InvalidConfiguration)?;
    let tls = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|_| ServerError::InvalidConfiguration)?;
    Ok(TlsAcceptor::from(Arc::new(tls)))
}

impl ClientSender {
    async fn send(&self, mut frame: Vec<u8>) -> Result<(), ServerError> {
        match self {
            Self::Udp { socket, address } => {
                socket.send_to(&frame, address).await?;
            }
            Self::Tcp { sender } => {
                if frame.first().is_some_and(|first| first & 0xc0 == 0x40) {
                    let padding = (4 - frame.len() % 4) % 4;
                    frame.resize(frame.len() + padding, 0);
                }
                sender.send(frame).await.map_err(|_| {
                    ServerError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "TURN TCP connection closed",
                    ))
                })?;
            }
        }
        Ok(())
    }
}

async fn read_turn_tcp_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, ServerError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut prefix = [0_u8; 4];
    if let Err(error) = reader.read_exact(&mut prefix).await {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            Ok(None)
        } else {
            Err(ServerError::Io(error))
        };
    }
    let body_length = usize::from(u16::from_be_bytes([prefix[2], prefix[3]]));
    let channel_data = prefix[0] & 0xc0 == 0x40;
    let stun = prefix[0] & 0xc0 == 0;
    if !channel_data && !stun {
        return Err(ServerError::MalformedRequest);
    }
    let total_length = if channel_data {
        4usize.saturating_add((body_length.saturating_add(3)) & !3)
    } else {
        if !body_length.is_multiple_of(4) {
            return Err(ServerError::MalformedRequest);
        }
        20usize.saturating_add(body_length)
    };
    if !(4..=MAX_DATAGRAM_BYTES + 4).contains(&total_length) {
        return Err(ServerError::MalformedRequest);
    }
    let mut frame = Vec::with_capacity(total_length);
    frame.extend_from_slice(&prefix);
    frame.resize(total_length, 0);
    reader.read_exact(&mut frame[4..]).await?;
    Ok(Some(frame))
}

#[derive(Debug, Clone)]
struct Authentication {
    username: String,
    key: [u8; 16],
}

#[derive(Debug, Clone, Copy)]
enum AuthRejection {
    Unauthorized,
    StaleNonce,
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn metrics(State(server): State<Arc<TurnServer>>) -> String {
    server.render_metrics().await
}

fn binding_response(message: &Message<'_>, client: SocketAddr) -> Result<Vec<u8>, ServerError> {
    Ok(MessageBuilder::new(
        MessageType::new(Method::BINDING, MessageClass::SuccessResponse),
        message.transaction_id(),
    )
    .xor_mapped_address(client)
    .software(SOFTWARE)
    .fingerprint()
    .build()?)
}

fn allocation_success(
    message: &Message<'_>,
    relayed_address: SocketAddr,
    mapped_address: SocketAddr,
    lifetime: u32,
    key: &[u8],
) -> Result<Vec<u8>, ServerError> {
    Ok(MessageBuilder::new(
        MessageType::new(Method::ALLOCATE, MessageClass::SuccessResponse),
        message.transaction_id(),
    )
    .xor_address(AttributeType::XOR_RELAYED_ADDRESS, relayed_address)
    .xor_mapped_address(mapped_address)
    .raw_attribute(AttributeType::LIFETIME, lifetime.to_be_bytes().to_vec())
    .software(SOFTWARE)
    .message_integrity_sha1(key.to_vec())
    .fingerprint()
    .build()?)
}

fn lifetime_success(
    message: &Message<'_>,
    lifetime: u32,
    key: &[u8],
) -> Result<Vec<u8>, ServerError> {
    Ok(MessageBuilder::new(
        MessageType::new(Method::REFRESH, MessageClass::SuccessResponse),
        message.transaction_id(),
    )
    .raw_attribute(AttributeType::LIFETIME, lifetime.to_be_bytes().to_vec())
    .software(SOFTWARE)
    .message_integrity_sha1(key.to_vec())
    .fingerprint()
    .build()?)
}

fn empty_success(message: &Message<'_>, key: &[u8]) -> Result<Vec<u8>, ServerError> {
    Ok(MessageBuilder::new(
        MessageType::new(
            message.message_type().method(),
            MessageClass::SuccessResponse,
        ),
        message.transaction_id(),
    )
    .software(SOFTWARE)
    .message_integrity_sha1(key.to_vec())
    .fingerprint()
    .build()?)
}

fn authenticated_error(
    message: &Message<'_>,
    code: u16,
    reason: &str,
    key: Option<&[u8]>,
    unknown: Option<&[AttributeType]>,
) -> Result<Vec<u8>, ServerError> {
    let mut builder = MessageBuilder::new(
        MessageType::new(message.message_type().method(), MessageClass::ErrorResponse),
        message.transaction_id(),
    )
    .error_code(code, reason)?;
    if let Some(unknown) = unknown {
        builder = builder.unknown_attributes(unknown);
    }
    builder = builder.software(SOFTWARE);
    if let Some(key) = key {
        builder = builder.message_integrity_sha1(key.to_vec());
    }
    Ok(builder.fingerprint().build()?)
}

fn data_indication(peer: SocketAddr, data: &[u8]) -> Result<Vec<u8>, StunError> {
    let mut transaction = [0_u8; 12];
    getrandom::fill(&mut transaction).map_err(|_| StunError::InvalidIntegrityKey)?;
    MessageBuilder::new(
        MessageType::new(Method::DATA, MessageClass::Indication),
        TransactionId::new(transaction),
    )
    .xor_address(AttributeType::XOR_PEER_ADDRESS, peer)
    .raw_attribute(AttributeType::DATA, data.to_vec())
    .fingerprint()
    .build()
}

fn requested_transport(message: &Message<'_>) -> Result<Option<u8>, ServerError> {
    let Some(attribute) = message.attribute(AttributeType::REQUESTED_TRANSPORT) else {
        return Ok(None);
    };
    let value: [u8; 4] = attribute
        .value()
        .try_into()
        .map_err(|_| ServerError::MalformedRequest)?;
    if value[1..] != [0, 0, 0] {
        return Err(ServerError::MalformedRequest);
    }
    Ok(Some(value[0]))
}

fn u32_attribute(
    message: &Message<'_>,
    attribute_type: AttributeType,
) -> Result<Option<u32>, ServerError> {
    message
        .attribute(attribute_type)
        .map(|attribute| {
            let value: [u8; 4] = attribute
                .value()
                .try_into()
                .map_err(|_| ServerError::MalformedRequest)?;
            Ok(u32::from_be_bytes(value))
        })
        .transpose()
}

fn channel_number(message: &Message<'_>) -> Result<Option<u16>, ServerError> {
    let Some(attribute) = message.attribute(AttributeType::CHANNEL_NUMBER) else {
        return Ok(None);
    };
    let value: [u8; 4] = attribute
        .value()
        .try_into()
        .map_err(|_| ServerError::MalformedRequest)?;
    if value[2..] != [0, 0] {
        return Err(ServerError::MalformedRequest);
    }
    Ok(Some(u16::from_be_bytes([value[0], value[1]])))
}

const fn same_family(left: IpAddr, right: IpAddr) -> bool {
    matches!(
        (left, right),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

fn valid_relay_port_range(minimum: u16, maximum: u16, allocations: usize) -> bool {
    if minimum == 0 || maximum == 0 {
        return minimum == 0 && maximum == 0;
    }
    minimum <= maximum
        && usize::from(maximum.saturating_sub(minimum).saturating_add(1)) >= allocations
}

fn is_multicast(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_multicast() || address == Ipv4Addr::BROADCAST,
        IpAddr::V6(address) => address.is_multicast() || address == Ipv6Addr::UNSPECIFIED,
    }
}

fn is_private_peer(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, ..] = address.octets();
            address.is_loopback()
                || address.is_link_local()
                || address.is_private()
                || first == 0
                || (first == 100 && (64..=127).contains(&second))
                || (first == 192 && second == 0 && third == 0)
                || (first == 192 && second == 0 && third == 2)
                || (first == 192 && second == 88 && third == 99)
                || (first == 198 && (second == 18 || second == 19))
                || (first == 198 && second == 51 && third == 100)
                || (first == 203 && second == 0 && third == 113)
                || first >= 240
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            address.is_loopback()
                || address.is_unicast_link_local()
                || address.is_unique_local()
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || segments[0] & 0xffc0 == 0xfec0
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| is_private_peer(IpAddr::V4(address)))
        }
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Debug)]
pub enum ServerError {
    InvalidConfiguration,
    MalformedRequest,
    Io(io::Error),
    Stun(StunError),
    Turn(TurnError),
    ConnectionIdExhausted,
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => formatter.write_str("invalid TURN configuration"),
            Self::MalformedRequest => formatter.write_str("malformed TURN request"),
            Self::Io(error) => error.fmt(formatter),
            Self::Stun(error) => error.fmt(formatter),
            Self::Turn(error) => error.fmt(formatter),
            Self::ConnectionIdExhausted => {
                formatter.write_str("TURN TCP connection identifier exhausted")
            }
        }
    }
}

impl std::error::Error for ServerError {}

impl From<io::Error> for ServerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StunError> for ServerError {
    fn from(error: StunError) -> Self {
        Self::Stun(error)
    }
}

impl From<TurnError> for ServerError {
    fn from(error: TurnError) -> Self {
        Self::Turn(error)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use fluvora_stun::{
        AttributeType, Message, MessageBuilder, MessageClass, MessageType, Method, TransactionId,
    };
    use fluvora_turn::{ChannelData, long_term_key};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpStream, UdpSocket};
    use tokio::task::JoinHandle;
    use tokio_rustls::TlsConnector;

    use super::{
        BandwidthLimiter, ServerConfig, TlsConfiguration, TurnServer, channel_number,
        is_private_peer, read_turn_tcp_frame, requested_transport,
    };

    struct TestAllocation {
        server: Arc<TurnServer>,
        task: JoinHandle<Result<(), super::ServerError>>,
        client: UdpSocket,
        server_address: SocketAddr,
        relayed_address: SocketAddr,
        nonce: String,
        key: [u8; 16],
    }

    async fn round_trip(socket: &UdpSocket, server: SocketAddr, request: &[u8]) -> Vec<u8> {
        socket.send_to(request, server).await.expect("send request");
        let mut buffer = vec![0_u8; 65_535];
        let length = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buffer))
            .await
            .expect("response timeout")
            .expect("receive response");
        buffer.truncate(length);
        buffer
    }

    async fn test_allocation() -> TestAllocation {
        let configuration = ServerConfig {
            control_bind: "127.0.0.1:0".parse().expect("control"),
            status_bind: "127.0.0.1:0".parse().expect("status"),
            advertised_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_port_min: 0,
            relay_port_max: 0,
            allow_private_peers: true,
            maximum_relay_bytes_per_second: 5_000_000,
            username: "user".to_owned(),
            password: "password".to_owned(),
            realm: "turn.test".to_owned(),
            nonce_secret: vec![9; 32],
            rest_secret: vec![8; 32],
            maximum_allocations: 8,
            maximum_allocations_per_ip: 8,
            tls: None,
        };
        let server = Arc::new(TurnServer::bind(configuration).await.expect("server"));
        let server_address = server.control_socket.local_addr().expect("server address");
        let task = tokio::spawn(Arc::clone(&server).control_loop());
        let client = UdpSocket::bind("127.0.0.1:0").await.expect("client");
        let challenge_request = MessageBuilder::new(
            MessageType::new(Method::ALLOCATE, MessageClass::Request),
            TransactionId::new([1; 12]),
        )
        .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
        .fingerprint()
        .build()
        .expect("challenge request");
        let challenge = round_trip(&client, server_address, &challenge_request).await;
        let challenge = Message::parse(&challenge).expect("challenge");
        assert_eq!(
            challenge.error_code().expect("error").expect("code").code(),
            401
        );
        let nonce = challenge.nonce().expect("nonce").expect("nonce").to_owned();
        let key = long_term_key("user", "turn.test", "password");
        let allocate = authenticated_builder(Method::ALLOCATE, [2; 12], &nonce)
            .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
            .message_integrity_sha1(key.to_vec())
            .fingerprint()
            .build()
            .expect("allocate");
        let response = round_trip(&client, server_address, &allocate).await;
        let response = Message::parse(&response).expect("allocate response");
        let relayed_address = response
            .xor_address(AttributeType::XOR_RELAYED_ADDRESS)
            .expect("relay")
            .expect("relay");
        TestAllocation {
            server,
            task,
            client,
            server_address,
            relayed_address,
            nonce,
            key,
        }
    }

    fn authenticated_builder(method: Method, transaction: [u8; 12], nonce: &str) -> MessageBuilder {
        MessageBuilder::new(
            MessageType::new(method, MessageClass::Request),
            TransactionId::new(transaction),
        )
        .username("user")
        .raw_attribute(AttributeType::REALM, b"turn.test".to_vec())
        .raw_attribute(AttributeType::NONCE, nonce.as_bytes().to_vec())
    }

    fn tls_test_configuration() -> (
        ServerConfig,
        tokio_rustls::rustls::pki_types::CertificateDer<'static>,
        tempfile::TempDir,
    ) {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("generate certificate");
        let certificate_der = certified.cert.der().clone();
        let directory = tempfile::tempdir().expect("temporary certificate directory");
        let certificate_path = directory.path().join("certificate.pem");
        let private_key_path = directory.path().join("private-key.pem");
        std::fs::write(&certificate_path, certified.cert.pem()).expect("write certificate");
        std::fs::write(&private_key_path, certified.signing_key.serialize_pem())
            .expect("write private key");
        let configuration = ServerConfig {
            control_bind: "127.0.0.1:0".parse().expect("control"),
            status_bind: "127.0.0.1:0".parse().expect("status"),
            advertised_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_port_min: 0,
            relay_port_max: 0,
            allow_private_peers: true,
            maximum_relay_bytes_per_second: 5_000_000,
            username: "user".to_owned(),
            password: "password".to_owned(),
            realm: "turn.test".to_owned(),
            nonce_secret: vec![9; 32],
            rest_secret: vec![8; 32],
            maximum_allocations: 8,
            maximum_allocations_per_ip: 8,
            tls: Some(TlsConfiguration {
                bind: "127.0.0.1:0".parse().expect("TLS bind"),
                certificate_pem: certificate_path,
                private_key_pem: private_key_path,
            }),
        };
        (configuration, certificate_der, directory)
    }

    #[test]
    fn decodes_turn_control_attributes() {
        let bytes = MessageBuilder::new(
            MessageType::new(Method::ALLOCATE, MessageClass::Request),
            TransactionId::new([1; 12]),
        )
        .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
        .raw_attribute(AttributeType::CHANNEL_NUMBER, [0x40, 0x01, 0, 0].to_vec())
        .build()
        .expect("message");
        let message = Message::parse(&bytes).expect("parse");
        assert_eq!(requested_transport(&message).expect("transport"), Some(17));
        assert_eq!(channel_number(&message).expect("channel"), Some(0x4001));
    }

    #[test]
    fn classifies_non_public_peer_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                is_private_peer(address.parse().expect("address")),
                "{address} should not be treated as public"
            );
        }
        assert!(!is_private_peer("8.8.8.8".parse().expect("IPv4")));
        assert!(!is_private_peer(
            "2606:4700:4700::1111".parse().expect("public IPv6")
        ));
    }

    #[test]
    fn allocation_bandwidth_has_a_bounded_burst_and_refills() {
        let mut limiter = BandwidthLimiter::new(65_000, Duration::ZERO);
        assert!(limiter.consume(Duration::ZERO, 65_000, 65_000));
        assert!(limiter.consume(Duration::ZERO, 65_000, 65_000));
        assert!(!limiter.consume(Duration::ZERO, 1, 65_000));
        assert!(limiter.consume(Duration::from_secs(1), 65_000, 65_000));
        assert!(!limiter.consume(Duration::from_secs(1), 1, 65_000));
    }

    #[tokio::test]
    async fn relays_send_data_indications_and_channel_data() {
        let allocation = test_allocation().await;
        let peer = UdpSocket::bind("127.0.0.1:0").await.expect("peer");
        let peer_address = peer.local_addr().expect("peer address");
        let permission =
            authenticated_builder(Method::CREATE_PERMISSION, [3; 12], &allocation.nonce)
                .xor_address(AttributeType::XOR_PEER_ADDRESS, peer_address)
                .message_integrity_sha1(allocation.key.to_vec())
                .fingerprint()
                .build()
                .expect("permission");
        let response = round_trip(&allocation.client, allocation.server_address, &permission).await;
        assert_eq!(
            Message::parse(&response)
                .expect("permission")
                .message_type()
                .class(),
            MessageClass::SuccessResponse
        );
        send_indication(&allocation, peer_address, b"outbound").await;
        let (payload, relay_source) = receive_peer(&peer).await;
        assert_eq!(payload, b"outbound");
        assert_eq!(relay_source, allocation.relayed_address);
        peer.send_to(b"inbound", relay_source)
            .await
            .expect("peer reply");
        let inbound = receive_client(&allocation.client).await;
        let indication = Message::parse(&inbound).expect("data indication");
        assert_eq!(indication.message_type().method(), Method::DATA);
        assert_eq!(
            indication
                .attribute(AttributeType::DATA)
                .expect("data")
                .value(),
            b"inbound"
        );
        bind_channel(&allocation, peer_address).await;
        let channel = ChannelData::encode(0x4000, b"channel-out").expect("channel");
        allocation
            .client
            .send_to(&channel, allocation.server_address)
            .await
            .expect("channel send");
        assert_eq!(receive_peer(&peer).await.0, b"channel-out");
        peer.send_to(b"channel-in", relay_source)
            .await
            .expect("channel reply");
        let inbound = receive_client(&allocation.client).await;
        assert_eq!(
            ChannelData::parse(&inbound).expect("channel inbound").data,
            b"channel-in"
        );
        allocation.task.abort();
        assert_eq!(allocation.server.allocations.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn accepts_framed_turn_over_tcp_and_cleans_disconnect() {
        let configuration = ServerConfig {
            control_bind: "127.0.0.1:0".parse().expect("control"),
            status_bind: "127.0.0.1:0".parse().expect("status"),
            advertised_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            relay_port_min: 0,
            relay_port_max: 0,
            allow_private_peers: true,
            maximum_relay_bytes_per_second: 5_000_000,
            username: "user".to_owned(),
            password: "password".to_owned(),
            realm: "turn.test".to_owned(),
            nonce_secret: vec![9; 32],
            rest_secret: vec![8; 32],
            maximum_allocations: 8,
            maximum_allocations_per_ip: 8,
            tls: None,
        };
        let server = Arc::new(TurnServer::bind(configuration).await.expect("server"));
        let address = server.tcp_listener.local_addr().expect("TCP address");
        let task = tokio::spawn(Arc::clone(&server).tcp_control_loop());
        let mut client = TcpStream::connect(address).await.expect("connect");
        let challenge_request = MessageBuilder::new(
            MessageType::new(Method::ALLOCATE, MessageClass::Request),
            TransactionId::new([11; 12]),
        )
        .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
        .fingerprint()
        .build()
        .expect("challenge");
        client
            .write_all(&challenge_request)
            .await
            .expect("write challenge");
        let challenge = read_turn_tcp_frame(&mut client)
            .await
            .expect("read challenge")
            .expect("challenge frame");
        let challenge = Message::parse(&challenge).expect("parse challenge");
        assert_eq!(
            challenge.error_code().expect("error").expect("code").code(),
            401
        );
        let nonce = challenge.nonce().expect("nonce").expect("nonce");
        let key = long_term_key("user", "turn.test", "password");
        let allocate = authenticated_builder(Method::ALLOCATE, [12; 12], nonce)
            .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
            .message_integrity_sha1(key.to_vec())
            .fingerprint()
            .build()
            .expect("allocate");
        client.write_all(&allocate).await.expect("write allocate");
        let response = read_turn_tcp_frame(&mut client)
            .await
            .expect("read allocation")
            .expect("allocation frame");
        let response = Message::parse(&response).expect("parse allocation");
        assert_eq!(
            response.message_type().class(),
            MessageClass::SuccessResponse
        );
        assert!(
            response
                .xor_address(AttributeType::XOR_RELAYED_ADDRESS)
                .expect("relayed address")
                .is_some()
        );
        drop(client);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(server.allocations.lock().await.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn accepts_authenticated_turn_over_tls_and_cleans_disconnect() {
        let (configuration, certificate_der, _directory) = tls_test_configuration();
        let server = Arc::new(TurnServer::bind(configuration).await.expect("server"));
        let address = server
            .tls_listener
            .as_ref()
            .expect("TLS listener")
            .local_addr()
            .expect("TLS address");
        let task = tokio::spawn(Arc::clone(&server).tls_control_loop());

        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots.add(certificate_der).expect("trust test certificate");
        let client_configuration = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_configuration));
        let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from("localhost")
            .expect("server name")
            .to_owned();
        let tcp = TcpStream::connect(address).await.expect("connect");
        let mut client = connector
            .connect(server_name, tcp)
            .await
            .expect("TLS handshake");

        let challenge_request = MessageBuilder::new(
            MessageType::new(Method::ALLOCATE, MessageClass::Request),
            TransactionId::new([21; 12]),
        )
        .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
        .fingerprint()
        .build()
        .expect("challenge");
        client
            .write_all(&challenge_request)
            .await
            .expect("write challenge");
        let challenge = read_turn_tcp_frame(&mut client)
            .await
            .expect("read challenge")
            .expect("challenge frame");
        let challenge = Message::parse(&challenge).expect("parse challenge");
        assert_eq!(
            challenge.error_code().expect("error").expect("code").code(),
            401
        );
        let nonce = challenge.nonce().expect("nonce").expect("nonce");
        let key = long_term_key("user", "turn.test", "password");
        let allocate = authenticated_builder(Method::ALLOCATE, [22; 12], nonce)
            .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
            .message_integrity_sha1(key.to_vec())
            .fingerprint()
            .build()
            .expect("allocate");
        client.write_all(&allocate).await.expect("write allocate");
        let response = read_turn_tcp_frame(&mut client)
            .await
            .expect("read allocation")
            .expect("allocation frame");
        let response = Message::parse(&response).expect("parse allocation");
        assert_eq!(
            response.message_type().class(),
            MessageClass::SuccessResponse
        );
        assert!(
            response
                .xor_address(AttributeType::XOR_RELAYED_ADDRESS)
                .expect("relayed address")
                .is_some()
        );
        drop(client);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(server.allocations.lock().await.is_empty());
        assert_eq!(
            server
                .metrics
                .tls_connections_active
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        task.abort();
    }

    async fn send_indication(allocation: &TestAllocation, peer_address: SocketAddr, data: &[u8]) {
        let bytes = MessageBuilder::new(
            MessageType::new(Method::SEND, MessageClass::Indication),
            TransactionId::new([4; 12]),
        )
        .xor_address(AttributeType::XOR_PEER_ADDRESS, peer_address)
        .raw_attribute(AttributeType::DATA, data.to_vec())
        .fingerprint()
        .build()
        .expect("send indication");
        allocation
            .client
            .send_to(&bytes, allocation.server_address)
            .await
            .expect("send indication");
    }

    async fn bind_channel(allocation: &TestAllocation, peer_address: SocketAddr) {
        let request = authenticated_builder(Method::CHANNEL_BIND, [5; 12], &allocation.nonce)
            .raw_attribute(AttributeType::CHANNEL_NUMBER, [0x40, 0x00, 0, 0].to_vec())
            .xor_address(AttributeType::XOR_PEER_ADDRESS, peer_address)
            .message_integrity_sha1(allocation.key.to_vec())
            .fingerprint()
            .build()
            .expect("channel bind");
        let response = round_trip(&allocation.client, allocation.server_address, &request).await;
        assert_eq!(
            Message::parse(&response)
                .expect("bind")
                .message_type()
                .class(),
            MessageClass::SuccessResponse
        );
    }

    async fn receive_peer(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
        let mut buffer = vec![0_u8; 65_535];
        let (length, source) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buffer))
                .await
                .expect("peer timeout")
                .expect("peer receive");
        buffer.truncate(length);
        (buffer, source)
    }

    async fn receive_client(socket: &UdpSocket) -> Vec<u8> {
        let mut buffer = vec![0_u8; 65_535];
        let length = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buffer))
            .await
            .expect("client timeout")
            .expect("client receive");
        buffer.truncate(length);
        buffer
    }
}
