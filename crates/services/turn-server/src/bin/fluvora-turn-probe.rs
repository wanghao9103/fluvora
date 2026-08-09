use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluvora_stun::{
    AttributeType, Message, MessageBuilder, MessageClass, MessageType, Method, TransactionId,
};
use fluvora_turn::{ChannelData, long_term_key};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject as _;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};

const CHANNEL_NUMBER: u16 = 0x4000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME_BYTES: usize = 65_559;

type AnyError = Box<dyn Error + Send + Sync>;
type AnyResult<T> = Result<T, AnyError>;

trait AsyncControlStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncControlStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Transport {
    Udp,
    Tcp,
    Tls,
}

impl Transport {
    fn parse(value: &str) -> AnyResult<Self> {
        match value {
            "udp" => Ok(Self::Udp),
            "tcp" => Ok(Self::Tcp),
            "tls" => Ok(Self::Tls),
            _ => Err(input_error("--transport must be udp, tcp, or tls")),
        }
    }
}

#[derive(Debug)]
enum Command {
    Probe(ProbeConfig),
    Echo(EchoConfig),
}

#[derive(Debug)]
struct ProbeConfig {
    transport: Transport,
    server: String,
    username: String,
    password: Secret,
    expected_realm: Option<String>,
    peer: PeerTarget,
    timeout: Duration,
    server_name: Option<String>,
    ca_pem: Option<PathBuf>,
    evidence: Option<PathBuf>,
}

struct Secret(String);

impl Secret {
    fn new(value: String) -> AnyResult<Self> {
        if value.is_empty() || value.len() > 512 || value.contains('\0') {
            return Err(input_error(
                "TURN password must be 1..=512 bytes without NUL",
            ));
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[derive(Debug)]
enum PeerTarget {
    SelfEcho,
    External(String),
}

#[derive(Debug)]
struct EchoConfig {
    bind: SocketAddr,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeEvidence {
    schema_version: u8,
    status: &'static str,
    observed_at_unix_seconds: u64,
    transport: Transport,
    server: String,
    realm: String,
    relayed_address: String,
    mapped_address: String,
    peer_address: String,
    allocation_millis: u128,
    send_indication_millis: u128,
    channel_data_millis: u128,
    total_millis: u128,
}

#[derive(Debug)]
struct Challenge {
    realm: String,
    nonce: String,
}

#[derive(Debug)]
struct AllocationContext {
    realm: String,
    nonce: String,
    key: [u8; 16],
    relayed_address: SocketAddr,
    mapped_address: SocketAddr,
    allocation_millis: u128,
}

#[derive(Debug)]
struct ProbeFailure(String);

impl fmt::Display for ProbeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ProbeFailure {}

enum Control {
    Udp {
        socket: UdpSocket,
        timeout: Duration,
    },
    Stream {
        stream: Box<dyn AsyncControlStream>,
        timeout: Duration,
    },
}

impl fmt::Debug for Control {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Udp { .. } => formatter.write_str("Control::Udp"),
            Self::Stream { .. } => formatter.write_str("Control::Stream"),
        }
    }
}

impl Control {
    async fn connect(configuration: &ProbeConfig, server: SocketAddr) -> AnyResult<Self> {
        match configuration.transport {
            Transport::Udp => {
                let bind = unspecified_address(server.ip());
                let socket = UdpSocket::bind(bind).await?;
                socket.connect(server).await?;
                Ok(Self::Udp {
                    socket,
                    timeout: configuration.timeout,
                })
            }
            Transport::Tcp => {
                let stream = timeout(
                    configuration.timeout,
                    TcpStream::connect(server),
                    "TURN/TCP connect",
                )
                .await?;
                Ok(Self::Stream {
                    stream: Box::new(stream),
                    timeout: configuration.timeout,
                })
            }
            Transport::Tls => {
                let server_name = configuration
                    .server_name
                    .as_ref()
                    .ok_or_else(|| input_error("--server-name is required with --transport tls"))?;
                let stream = connect_tls(configuration, server, server_name).await?;
                Ok(Self::Stream {
                    stream,
                    timeout: configuration.timeout,
                })
            }
        }
    }

    async fn request(&mut self, request: &[u8], operation: &str) -> AnyResult<Vec<u8>> {
        self.send(request, operation).await?;
        self.receive(operation).await
    }

    async fn send(&mut self, frame: &[u8], operation: &str) -> AnyResult<()> {
        match self {
            Self::Udp {
                socket,
                timeout: wait,
            } => {
                timeout(*wait, socket.send(frame), operation).await?;
            }
            Self::Stream {
                stream,
                timeout: wait,
            } => {
                let mut framed = frame.to_vec();
                if is_channel_data(&framed) {
                    let padding = (4 - framed.len() % 4) % 4;
                    framed.resize(framed.len() + padding, 0);
                }
                timeout(*wait, stream.write_all(&framed), operation).await?;
            }
        }
        Ok(())
    }

    async fn receive(&mut self, operation: &str) -> AnyResult<Vec<u8>> {
        match self {
            Self::Udp {
                socket,
                timeout: wait,
            } => {
                let mut buffer = vec![0_u8; MAX_FRAME_BYTES];
                let length = timeout(*wait, socket.recv(&mut buffer), operation).await?;
                buffer.truncate(length);
                Ok(buffer)
            }
            Self::Stream {
                stream,
                timeout: wait,
            } => Ok(timeout(*wait, read_stream_frame(stream), operation).await?),
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("TURN probe failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> AnyResult<()> {
    match parse_command(env::args().skip(1))? {
        Command::Probe(configuration) => run_probe(configuration).await,
        Command::Echo(configuration) => run_echo(configuration).await,
    }
}

async fn run_echo(configuration: EchoConfig) -> AnyResult<()> {
    let socket = UdpSocket::bind(configuration.bind).await?;
    println!(
        "Fluvora TURN probe echo listening on {}",
        socket.local_addr()?
    );
    let mut buffer = vec![0_u8; 65_535];
    loop {
        let (length, source) = socket.recv_from(&mut buffer).await?;
        socket.send_to(&buffer[..length], source).await?;
    }
}

async fn run_probe(configuration: ProbeConfig) -> AnyResult<()> {
    let started = Instant::now();
    let server = resolve_address(&configuration.server).await?;
    let (peer, echo_task) = prepare_peer(&configuration.peer, server.ip()).await?;
    let mut control = Control::connect(&configuration, server).await?;
    let allocation = allocate(&configuration, &mut control).await?;
    create_permission(&configuration, &allocation, &mut control, peer).await?;
    let send_indication_millis = verify_send_indication(&mut control, peer).await?;
    let channel_data_millis =
        verify_channel_data(&configuration, &allocation, &mut control, peer).await?;
    delete_allocation(&configuration, &allocation, &mut control).await?;

    if let Some(task) = echo_task {
        task.abort();
    }
    let evidence = ProbeEvidence {
        schema_version: 1,
        status: "pass",
        observed_at_unix_seconds: unix_seconds(),
        transport: configuration.transport,
        server: configuration.server,
        realm: allocation.realm,
        relayed_address: allocation.relayed_address.to_string(),
        mapped_address: allocation.mapped_address.to_string(),
        peer_address: peer.to_string(),
        allocation_millis: allocation.allocation_millis,
        send_indication_millis,
        channel_data_millis,
        total_millis: started.elapsed().as_millis(),
    };
    let rendered = serde_json::to_string_pretty(&evidence)?;
    if let Some(path) = configuration.evidence {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, format!("{rendered}\n"))?;
    }
    println!("{rendered}");
    Ok(())
}

async fn allocate(
    configuration: &ProbeConfig,
    control: &mut Control,
) -> AnyResult<AllocationContext> {
    let started = Instant::now();
    let challenge = challenge(control).await?;
    if configuration
        .expected_realm
        .as_ref()
        .is_some_and(|expected| expected != &challenge.realm)
    {
        return Err(failure(format!(
            "TURN realm mismatch: expected {}, received {}",
            configuration.expected_realm.as_deref().unwrap_or_default(),
            challenge.realm
        )));
    }
    let key = long_term_key(
        &configuration.username,
        &challenge.realm,
        configuration.password.expose(),
    );
    let request = authenticated_request(
        configuration,
        &challenge.realm,
        &challenge.nonce,
        Method::ALLOCATE,
    )?
    .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
    .message_integrity_sha1(key.to_vec())
    .fingerprint()
    .build()?;
    let response = control.request(&request, "TURN Allocate response").await?;
    let response = parse_success(&response, Method::ALLOCATE, &key)?;
    Ok(AllocationContext {
        realm: challenge.realm,
        nonce: challenge.nonce,
        key,
        relayed_address: required_address(
            response.xor_address(AttributeType::XOR_RELAYED_ADDRESS)?,
            "XOR-RELAYED-ADDRESS",
        )?,
        mapped_address: required_address(
            response.xor_address(AttributeType::XOR_MAPPED_ADDRESS)?,
            "XOR-MAPPED-ADDRESS",
        )?,
        allocation_millis: started.elapsed().as_millis(),
    })
}

async fn create_permission(
    configuration: &ProbeConfig,
    allocation: &AllocationContext,
    control: &mut Control,
    peer: SocketAddr,
) -> AnyResult<()> {
    let request = authenticated_request(
        configuration,
        &allocation.realm,
        &allocation.nonce,
        Method::CREATE_PERMISSION,
    )?
    .xor_address(AttributeType::XOR_PEER_ADDRESS, peer)
    .message_integrity_sha1(allocation.key.to_vec())
    .fingerprint()
    .build()?;
    let response = control
        .request(&request, "TURN CreatePermission response")
        .await?;
    parse_success(&response, Method::CREATE_PERMISSION, &allocation.key)?;
    Ok(())
}

async fn verify_send_indication(control: &mut Control, peer: SocketAddr) -> AnyResult<u128> {
    let started = Instant::now();
    let payload = b"fluvora-turn-probe-send";
    let indication = MessageBuilder::new(
        MessageType::new(Method::SEND, MessageClass::Indication),
        random_transaction()?,
    )
    .xor_address(AttributeType::XOR_PEER_ADDRESS, peer)
    .raw_attribute(AttributeType::DATA, payload.to_vec())
    .fingerprint()
    .build()?;
    control.send(&indication, "TURN Send indication").await?;
    let echoed = control.receive("TURN Data indication").await?;
    verify_data_indication(&echoed, peer, payload)?;
    Ok(started.elapsed().as_millis())
}

async fn verify_channel_data(
    configuration: &ProbeConfig,
    allocation: &AllocationContext,
    control: &mut Control,
    peer: SocketAddr,
) -> AnyResult<u128> {
    let bind = authenticated_request(
        configuration,
        &allocation.realm,
        &allocation.nonce,
        Method::CHANNEL_BIND,
    )?
    .raw_attribute(AttributeType::CHANNEL_NUMBER, [0x40, 0x00, 0, 0].to_vec())
    .xor_address(AttributeType::XOR_PEER_ADDRESS, peer)
    .message_integrity_sha1(allocation.key.to_vec())
    .fingerprint()
    .build()?;
    let response = control.request(&bind, "TURN ChannelBind response").await?;
    parse_success(&response, Method::CHANNEL_BIND, &allocation.key)?;

    let started = Instant::now();
    let payload = b"fluvora-turn-probe-channel";
    control
        .send(
            &ChannelData::encode(CHANNEL_NUMBER, payload)?,
            "TURN ChannelData send",
        )
        .await?;
    let echoed = control.receive("TURN ChannelData receive").await?;
    let echoed = ChannelData::parse(&echoed)?;
    if echoed.channel_number != CHANNEL_NUMBER || echoed.data != payload {
        return Err(failure("TURN ChannelData echo payload mismatch"));
    }
    Ok(started.elapsed().as_millis())
}

async fn delete_allocation(
    configuration: &ProbeConfig,
    allocation: &AllocationContext,
    control: &mut Control,
) -> AnyResult<()> {
    let request = authenticated_request(
        configuration,
        &allocation.realm,
        &allocation.nonce,
        Method::REFRESH,
    )?
    .raw_attribute(AttributeType::LIFETIME, 0_u32.to_be_bytes().to_vec())
    .message_integrity_sha1(allocation.key.to_vec())
    .fingerprint()
    .build()?;
    let response = control
        .request(&request, "TURN allocation deletion response")
        .await?;
    parse_success(&response, Method::REFRESH, &allocation.key)?;
    Ok(())
}

async fn challenge(control: &mut Control) -> AnyResult<Challenge> {
    let request = MessageBuilder::new(
        MessageType::new(Method::ALLOCATE, MessageClass::Request),
        random_transaction()?,
    )
    .raw_attribute(AttributeType::REQUESTED_TRANSPORT, vec![17, 0, 0, 0])
    .fingerprint()
    .build()?;
    let response = control
        .request(&request, "TURN authentication challenge")
        .await?;
    let parsed = Message::parse(&response)?;
    parsed.verify_fingerprint()?;
    let code = parsed
        .error_code()?
        .ok_or_else(|| failure("TURN challenge omitted ERROR-CODE"))?
        .code();
    if parsed.message_type().method() != Method::ALLOCATE
        || parsed.message_type().class() != MessageClass::ErrorResponse
        || code != 401
    {
        return Err(failure(format!(
            "expected TURN Allocate 401 challenge, received {:?} code {code}",
            parsed.message_type()
        )));
    }
    Ok(Challenge {
        realm: required_text(parsed.realm()?, "REALM")?.to_owned(),
        nonce: required_text(parsed.nonce()?, "NONCE")?.to_owned(),
    })
}

fn authenticated_request(
    configuration: &ProbeConfig,
    realm: &str,
    nonce: &str,
    method: Method,
) -> AnyResult<MessageBuilder> {
    Ok(MessageBuilder::new(
        MessageType::new(method, MessageClass::Request),
        random_transaction()?,
    )
    .username(configuration.username.clone())
    .raw_attribute(AttributeType::REALM, realm.as_bytes().to_vec())
    .raw_attribute(AttributeType::NONCE, nonce.as_bytes().to_vec()))
}

fn parse_success<'a>(
    response: &'a [u8],
    expected_method: Method,
    key: &[u8],
) -> AnyResult<Message<'a>> {
    let message = Message::parse(response)?;
    message.verify_fingerprint()?;
    message.verify_message_integrity_sha1(key)?;
    if message.message_type().method() != expected_method
        || message.message_type().class() != MessageClass::SuccessResponse
    {
        let code = message.error_code()?.map(fluvora_stun::ErrorCode::code);
        return Err(failure(format!(
            "expected {:?} success response, received {:?} error {code:?}",
            expected_method,
            message.message_type()
        )));
    }
    Ok(message)
}

fn verify_data_indication(response: &[u8], peer: SocketAddr, payload: &[u8]) -> AnyResult<()> {
    let message = Message::parse(response)?;
    message.verify_fingerprint()?;
    let source = required_address(
        message.xor_address(AttributeType::XOR_PEER_ADDRESS)?,
        "XOR-PEER-ADDRESS",
    )?;
    let data = message
        .attribute(AttributeType::DATA)
        .ok_or_else(|| failure("TURN Data indication omitted DATA"))?;
    if message.message_type() != MessageType::new(Method::DATA, MessageClass::Indication)
        || source != peer
        || data.value() != payload
    {
        return Err(failure("TURN Data indication echo mismatch"));
    }
    Ok(())
}

async fn prepare_peer(
    target: &PeerTarget,
    server_ip: IpAddr,
) -> AnyResult<(SocketAddr, Option<tokio::task::JoinHandle<()>>)> {
    match target {
        PeerTarget::External(address) => Ok((resolve_address(address).await?, None)),
        PeerTarget::SelfEcho => {
            if !server_ip.is_loopback() {
                return Err(input_error(
                    "--peer must identify a reachable external UDP echo server for non-loopback TURN",
                ));
            }
            let socket = Arc::new(
                UdpSocket::bind(loopback_address(server_ip))
                    .await
                    .map_err(|error| failure(format!("bind self echo: {error}")))?,
            );
            let address = socket.local_addr()?;
            let task = tokio::spawn(async move {
                let mut buffer = vec![0_u8; 65_535];
                loop {
                    let Ok((length, source)) = socket.recv_from(&mut buffer).await else {
                        break;
                    };
                    if socket.send_to(&buffer[..length], source).await.is_err() {
                        break;
                    }
                }
            });
            Ok((address, Some(task)))
        }
    }
}

async fn connect_tls(
    configuration: &ProbeConfig,
    server: SocketAddr,
    server_name: &str,
) -> AnyResult<Box<dyn AsyncControlStream>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca_pem) = &configuration.ca_pem {
        let certificates = CertificateDer::pem_file_iter(ca_pem)?.collect::<Result<Vec<_>, _>>()?;
        if certificates.is_empty() {
            return Err(input_error("--ca-pem did not contain a certificate"));
        }
        for certificate in certificates {
            roots.add(certificate)?;
        }
    } else {
        let native = rustls_native_certs::load_native_certs();
        if !native.errors.is_empty() && native.certs.is_empty() {
            return Err(failure(format!(
                "could not load native TLS roots: {:?}",
                native.errors
            )));
        }
        for certificate in native.certs {
            roots.add(certificate)?;
        }
    }
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(tls));
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|_| input_error("--server-name is not a valid TLS DNS name or IP address"))?;
    let tcp = timeout(
        configuration.timeout,
        TcpStream::connect(server),
        "TURN/TLS TCP connect",
    )
    .await?;
    let stream = timeout(
        configuration.timeout,
        connector.connect(name, tcp),
        "TURN/TLS handshake",
    )
    .await?;
    Ok(Box::new(stream))
}

async fn read_stream_frame(stream: &mut Box<dyn AsyncControlStream>) -> AnyResult<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    stream.read_exact(&mut prefix).await?;
    let body_length = usize::from(u16::from_be_bytes([prefix[2], prefix[3]]));
    let total = if prefix[0] & 0xc0 == 0x40 {
        4usize
            .checked_add((body_length.saturating_add(3)) & !3)
            .ok_or_else(|| failure("TURN ChannelData frame length overflow"))?
    } else if prefix[0] & 0xc0 == 0 {
        if !body_length.is_multiple_of(4) {
            return Err(failure("TURN stream STUN length is not aligned"));
        }
        20usize
            .checked_add(body_length)
            .ok_or_else(|| failure("TURN STUN frame length overflow"))?
    } else {
        return Err(failure("TURN stream frame has an invalid prefix"));
    };
    if !(4..=MAX_FRAME_BYTES).contains(&total) {
        return Err(failure("TURN stream frame exceeds the probe limit"));
    }
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&prefix);
    frame.resize(total, 0);
    stream.read_exact(&mut frame[4..]).await?;
    Ok(frame)
}

async fn timeout<F, T, E>(duration: Duration, future: F, operation: &str) -> AnyResult<T>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: Into<AnyError>,
{
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| {
            failure(format!(
                "{operation} timed out after {} ms",
                duration.as_millis()
            ))
        })?
        .map_err(Into::into)
}

async fn resolve_address(value: &str) -> AnyResult<SocketAddr> {
    lookup_host(value)
        .await?
        .next()
        .ok_or_else(|| input_error(format!("could not resolve {value}")))
}

fn parse_command(arguments: impl Iterator<Item = String>) -> AnyResult<Command> {
    let mut arguments = arguments.peekable();
    let mode = arguments.next().ok_or_else(|| input_error(usage()))?;
    let mut values = std::collections::HashMap::<String, String>::new();
    while let Some(name) = arguments.next() {
        if !name.starts_with("--") {
            return Err(input_error(format!(
                "unexpected argument {name}\n{}",
                usage()
            )));
        }
        let value = arguments
            .next()
            .ok_or_else(|| input_error(format!("{name} requires a value")))?;
        if values.insert(name.clone(), value).is_some() {
            return Err(input_error(format!("duplicate option {name}")));
        }
    }
    match mode.as_str() {
        "echo" => {
            reject_unknown(&values, &["--bind"])?;
            let bind = values
                .get("--bind")
                .map_or("0.0.0.0:3479", String::as_str)
                .parse()
                .map_err(|_| input_error("--bind must be an IP socket address"))?;
            Ok(Command::Echo(EchoConfig { bind }))
        }
        "probe" => {
            reject_unknown(
                &values,
                &[
                    "--transport",
                    "--server",
                    "--username",
                    "--password",
                    "--password-file",
                    "--realm",
                    "--peer",
                    "--timeout-ms",
                    "--server-name",
                    "--ca-pem",
                    "--evidence",
                ],
            )?;
            let transport = Transport::parse(required_option(&values, "--transport")?)?;
            let timeout_millis = values
                .get("--timeout-ms")
                .map_or(Ok(DEFAULT_TIMEOUT.as_millis()), |value| {
                    value.parse::<u128>()
                })
                .map_err(|_| input_error("--timeout-ms must be an integer"))?;
            if !(100..=60_000).contains(&timeout_millis) {
                return Err(input_error("--timeout-ms must be 100..=60000"));
            }
            let timeout = Duration::from_millis(
                u64::try_from(timeout_millis)
                    .map_err(|_| input_error("--timeout-ms is too large"))?,
            );
            let peer = values.get("--peer").map_or(PeerTarget::SelfEcho, |value| {
                PeerTarget::External(value.clone())
            });
            let password = probe_password(&values)?;
            Ok(Command::Probe(ProbeConfig {
                transport,
                server: required_option(&values, "--server")?.to_owned(),
                username: required_option(&values, "--username")?.to_owned(),
                password,
                expected_realm: values.get("--realm").cloned(),
                peer,
                timeout,
                server_name: values.get("--server-name").cloned(),
                ca_pem: values.get("--ca-pem").map(PathBuf::from),
                evidence: values.get("--evidence").map(PathBuf::from),
            }))
        }
        _ => Err(input_error(usage())),
    }
}

fn probe_password(values: &std::collections::HashMap<String, String>) -> AnyResult<Secret> {
    let inline = values.get("--password");
    let file = values.get("--password-file");
    if inline.is_some() && file.is_some() {
        return Err(input_error(
            "--password and --password-file are mutually exclusive",
        ));
    }
    let value = if let Some(value) = inline {
        value.clone()
    } else if let Some(path) = file {
        std::fs::read_to_string(path)?
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    } else {
        env::var("FLUVORA_TURN_PROBE_PASSWORD").map_err(|_| {
            input_error("--password-file or FLUVORA_TURN_PROBE_PASSWORD is required")
        })?
    };
    Secret::new(value)
}

fn reject_unknown(
    values: &std::collections::HashMap<String, String>,
    allowed: &[&str],
) -> AnyResult<()> {
    if let Some(name) = values.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(input_error(format!("unknown option {name}\n{}", usage())));
    }
    Ok(())
}

fn required_option<'a>(
    values: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> AnyResult<&'a str> {
    values
        .get(name)
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .ok_or_else(|| input_error(format!("{name} is required")))
}

fn required_text<'a>(value: Option<&'a str>, name: &str) -> AnyResult<&'a str> {
    value.ok_or_else(|| failure(format!("TURN response omitted {name}")))
}

fn required_address(value: Option<SocketAddr>, name: &str) -> AnyResult<SocketAddr> {
    value.ok_or_else(|| failure(format!("TURN response omitted {name}")))
}

fn random_transaction() -> AnyResult<TransactionId> {
    let mut transaction = [0_u8; 12];
    getrandom::fill(&mut transaction)
        .map_err(|_| failure("operating system random source is unavailable"))?;
    Ok(TransactionId::new(transaction))
}

fn unspecified_address(ip: IpAddr) -> SocketAddr {
    match ip {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    }
}

fn loopback_address(ip: IpAddr) -> SocketAddr {
    match ip {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
    }
}

fn is_channel_data(frame: &[u8]) -> bool {
    frame.first().is_some_and(|first| first & 0xc0 == 0x40)
}

fn input_error(message: impl Into<String>) -> AnyError {
    Box::new(io::Error::new(io::ErrorKind::InvalidInput, message.into()))
}

fn failure(message: impl Into<String>) -> AnyError {
    Box::new(ProbeFailure(message.into()))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn usage() -> &'static str {
    "usage:
  fluvora-turn-probe probe --transport udp|tcp|tls --server HOST:PORT \\
    --username USER --password-file FILE [--realm REALM] [--peer HOST:PORT] \\
    [--timeout-ms 5000] [--server-name HOST] [--ca-pem FILE] [--evidence FILE]
  fluvora-turn-probe echo [--bind IP:PORT]

Use FLUVORA_TURN_PROBE_PASSWORD instead of --password-file when a secret injector supplies it.
Omit --peer only for a loopback TURN server; the probe then starts a private self-echo socket."
}

#[cfg(test)]
mod tests {
    use super::{Command, PeerTarget, Transport, parse_command};

    #[test]
    fn parses_a_bounded_probe_configuration_without_exposing_the_password() {
        let command = parse_command(
            [
                "probe",
                "--transport",
                "tls",
                "--server",
                "turn.example.com:5349",
                "--username",
                "user",
                "--password",
                "secret",
                "--server-name",
                "turn.example.com",
                "--peer",
                "echo.example.com:3479",
                "--timeout-ms",
                "1000",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("probe");
        let Command::Probe(configuration) = command else {
            panic!("expected probe");
        };
        assert_eq!(configuration.transport, Transport::Tls);
        assert!(matches!(configuration.peer, PeerTarget::External(_)));
        assert_eq!(configuration.timeout.as_millis(), 1_000);
        assert!(!format!("{configuration:?}").contains("secret"));
    }

    #[test]
    fn rejects_unknown_and_duplicate_options() {
        assert!(
            parse_command(
                ["echo", "--unknown", "value"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
        assert!(
            parse_command(
                ["echo", "--bind", "127.0.0.1:1", "--bind", "127.0.0.1:2"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }
}
