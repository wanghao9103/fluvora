//! Blocking connected-UDP OpenSSL DTLS backend used by the media-node runtime.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::UdpSocket;
use std::sync::Arc;

use openssl::pkey::{PKey, Private};
use openssl::ssl::{
    ErrorCode, HandshakeError, MidHandshakeSslStream, Ssl, SslContext, SslContextBuilder,
    SslMethod, SslOptions, SslStream, SslVerifyMode, SslVersion,
};
use openssl::x509::X509;

use crate::{
    DirectionalKeyingMaterial, DtlsError, DtlsRole, DtlsSrtpProfile, Sha256Fingerprint,
    split_srtp_exporter,
};

const EXPORTER_LABEL: &str = "EXTRACTOR-dtls_srtp";
const EXPORTER_LEN: usize = 60;
const MAX_DATAGRAM_BYTES: usize = 65_535;

/// In-memory datagram BIO used to multiplex many DTLS sessions over one runtime UDP socket.
#[derive(Debug, Default)]
struct DatagramIo {
    inbound: VecDeque<Vec<u8>>,
    outbound: VecDeque<Vec<u8>>,
}

impl DatagramIo {
    fn with_input(input: &[u8]) -> Self {
        let mut io = Self::default();
        io.push_input(input);
        io
    }

    fn push_input(&mut self, input: &[u8]) {
        self.inbound.push_back(input.to_vec());
    }

    fn take_output(&mut self) -> Vec<Vec<u8>> {
        self.outbound.drain(..).collect()
    }
}

impl Read for DatagramIo {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let Some(datagram) = self.inbound.pop_front() else {
            return Err(std::io::Error::from(ErrorKind::WouldBlock));
        };
        let length = datagram.len().min(buffer.len());
        buffer[..length].copy_from_slice(&datagram[..length]);
        Ok(length)
    }
}

impl Write for DatagramIo {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.outbound.push_back(buffer.to_vec());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Connected datagram socket adapted to OpenSSL's `Read + Write` BIO.
#[derive(Debug)]
pub struct ConnectedUdp(UdpSocket);

impl ConnectedUdp {
    /// Wraps a UDP socket that has already been connected to its authenticated ICE pair.
    #[must_use]
    pub const fn new(socket: UdpSocket) -> Self {
        Self(socket)
    }

    /// Returns the socket for timeout and address inspection.
    #[must_use]
    pub const fn socket(&self) -> &UdpSocket {
        &self.0
    }
}

impl Read for ConnectedUdp {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.recv(buffer)
    }
}

impl Write for ConnectedUdp {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.send(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Server certificate and ECDSA private key loaded from PEM.
#[derive(Clone)]
pub struct Identity {
    certificate: X509,
    private_key: PKey<Private>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Identity([REDACTED PRIVATE KEY])")
    }
}

impl Identity {
    /// Loads a leaf certificate and matching private key.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] for malformed PEM or a mismatched key.
    pub fn from_pem(certificate_pem: &[u8], private_key_pem: &[u8]) -> Result<Self, DtlsError> {
        let certificate = X509::from_pem(certificate_pem)?;
        let private_key = PKey::private_key_from_pem(private_key_pem)?;
        if !certificate.public_key()?.public_eq(&private_key) {
            return Err(DtlsError::Handshake(
                "certificate does not match private key".to_owned(),
            ));
        }
        Ok(Self {
            certificate,
            private_key,
        })
    }

    /// Returns the SDP SHA-256 fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] if OpenSSL cannot encode the certificate.
    pub fn fingerprint(&self) -> Result<Sha256Fingerprint, DtlsError> {
        Ok(Sha256Fingerprint::from_certificate_der(
            &self.certificate.to_der()?,
        ))
    }
}

/// Immutable OpenSSL DTLS 1.2 server configuration.
#[derive(Debug, Clone)]
pub struct DtlsServer {
    context: Arc<SslContext>,
}

impl DtlsServer {
    /// Configures DTLS 1.2, ECDHE-ECDSA, peer certificates, and supported SRTP profiles.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] if OpenSSL rejects any security setting.
    pub fn new(identity: &Identity) -> Result<Self, DtlsError> {
        let mut builder = SslContextBuilder::new(SslMethod::dtls_server())?;
        builder.set_certificate(&identity.certificate)?;
        builder.set_private_key(&identity.private_key)?;
        builder.check_private_key()?;
        builder.set_min_proto_version(Some(SslVersion::DTLS1_2))?;
        builder.set_max_proto_version(Some(SslVersion::DTLS1_2))?;
        builder.set_cipher_list("ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES128-SHA256")?;
        builder.set_tlsext_use_srtp("SRTP_AES128_CM_SHA1_80:SRTP_AES128_CM_SHA1_32")?;
        builder.set_read_ahead(true);
        builder.set_options(
            SslOptions::NO_QUERY_MTU | SslOptions::NO_COMPRESSION | SslOptions::NO_RENEGOTIATION,
        );
        builder.set_verify_callback(
            SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT,
            |_preverified, _context| true,
        );
        Ok(Self {
            context: Arc::new(builder.build()),
        })
    }

    /// Completes a blocking DTLS handshake on an ICE-authenticated connected UDP socket.
    ///
    /// The peer's self-signed certificate is accepted by X.509 verification only so it can be
    /// compared against the authenticated SDP fingerprint immediately after the handshake.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] for handshake, fingerprint, profile, or exporter failures.
    pub fn accept(
        &self,
        transport: ConnectedUdp,
        expected_peer_fingerprint: Sha256Fingerprint,
    ) -> Result<EstablishedSession, DtlsError> {
        let mut ssl = Ssl::new(&self.context)?;
        ssl.set_accept_state();
        ssl.set_mtu(1_200)?;
        let stream = ssl
            .accept(transport)
            .map_err(|error| DtlsError::Handshake(error.to_string()))?;
        EstablishedSession::finish(stream, expected_peer_fingerprint)
    }

    /// Creates a nonblocking DTLS session for a shared UDP runtime.
    #[must_use]
    pub fn datagram_session(
        &self,
        expected_peer_fingerprint: Sha256Fingerprint,
    ) -> DatagramDtlsSession {
        DatagramDtlsSession {
            context: Arc::clone(&self.context),
            expected_peer_fingerprint,
            state: DatagramState::New,
        }
    }
}

#[derive(Debug)]
enum DatagramState {
    New,
    Handshaking(MidHandshakeSslStream<DatagramIo>),
    Established(SslStream<DatagramIo>),
    Failed,
}

/// Nonblocking OpenSSL DTLS state machine driven by authenticated UDP datagrams.
#[derive(Debug)]
pub struct DatagramDtlsSession {
    context: Arc<SslContext>,
    expected_peer_fingerprint: Sha256Fingerprint,
    state: DatagramState,
}

/// Output from one nonblocking DTLS step.
#[derive(Debug, Clone)]
pub struct DatagramProgress {
    /// DTLS records to send to the authenticated ICE tuple.
    pub outbound_datagrams: Vec<Vec<u8>>,
    /// Set exactly once after fingerprint verification and exporter completion.
    pub established_keying_material: Option<DirectionalKeyingMaterial>,
    /// Decrypted DTLS application bytes, such as SCTP packets.
    pub application_data: Vec<Vec<u8>>,
}

impl DatagramDtlsSession {
    /// Processes one authenticated DTLS datagram.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] for malformed records, handshake failure, certificate mismatch, an
    /// unsupported SRTP profile, or an exporter failure.
    pub fn handle_datagram(&mut self, input: &[u8]) -> Result<DatagramProgress, DtlsError> {
        if input.is_empty() || input.len() > MAX_DATAGRAM_BYTES {
            return Err(DtlsError::Handshake(
                "invalid DTLS datagram length".to_owned(),
            ));
        }
        self.step(Some(input))
    }

    /// Polls handshake retransmission/output without injecting a datagram.
    ///
    /// # Errors
    ///
    /// Returns [`DtlsError`] if OpenSSL reports a terminal handshake failure.
    pub fn poll(&mut self) -> Result<DatagramProgress, DtlsError> {
        self.step(None)
    }

    /// Returns whether the SRTP exporter has completed.
    #[must_use]
    pub const fn is_established(&self) -> bool {
        matches!(self.state, DatagramState::Established(_))
    }

    /// Encrypts one complete application message and returns generated DTLS datagrams.
    ///
    /// # Errors
    ///
    /// Returns an error before handshake completion, for an empty/oversized message, a partial
    /// write, or an OpenSSL write failure.
    pub fn write_application_data(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>, DtlsError> {
        if payload.is_empty() || payload.len() > MAX_DATAGRAM_BYTES {
            return Err(DtlsError::Handshake(
                "invalid DTLS application message length".to_owned(),
            ));
        }
        let state = std::mem::replace(&mut self.state, DatagramState::Failed);
        let DatagramState::Established(mut stream) = state else {
            self.state = state;
            return Err(DtlsError::Handshake(
                "DTLS session is not established".to_owned(),
            ));
        };
        let result = stream.ssl_write(payload);
        let outbound = stream.get_mut().take_output();
        self.state = DatagramState::Established(stream);
        match result {
            Ok(length) if length == payload.len() => Ok(outbound),
            Ok(_) => Err(DtlsError::Handshake(
                "partial DTLS application write".to_owned(),
            )),
            Err(error) => Err(DtlsError::Handshake(error.to_string())),
        }
    }

    fn step(&mut self, input: Option<&[u8]>) -> Result<DatagramProgress, DtlsError> {
        let state = std::mem::replace(&mut self.state, DatagramState::Failed);
        match state {
            DatagramState::New => {
                let Some(input) = input else {
                    self.state = DatagramState::New;
                    return Ok(empty_progress());
                };
                let mut ssl = Ssl::new(&self.context)?;
                ssl.set_accept_state();
                ssl.set_mtu(1_200)?;
                let io = DatagramIo::with_input(input);
                self.handle_handshake_result(ssl.accept(io))
            }
            DatagramState::Handshaking(mut stream) => {
                if let Some(input) = input {
                    stream.get_mut().push_input(input);
                }
                self.handle_handshake_result(stream.handshake())
            }
            DatagramState::Established(mut stream) => {
                if let Some(input) = input {
                    stream.get_mut().push_input(input);
                }
                let mut application_data = Vec::new();
                loop {
                    let mut plaintext = vec![0_u8; 65_535];
                    match stream.ssl_read(&mut plaintext) {
                        Ok(length) => {
                            plaintext.truncate(length);
                            application_data.push(plaintext);
                        }
                        Err(error)
                            if matches!(
                                error.code(),
                                ErrorCode::WANT_READ | ErrorCode::WANT_WRITE
                            ) =>
                        {
                            break;
                        }
                        Err(error) => {
                            return Err(DtlsError::Handshake(error.to_string()));
                        }
                    }
                }
                let outbound_datagrams = stream.get_mut().take_output();
                self.state = DatagramState::Established(stream);
                Ok(DatagramProgress {
                    outbound_datagrams,
                    established_keying_material: None,
                    application_data,
                })
            }
            DatagramState::Failed => Err(DtlsError::Handshake(
                "DTLS session is in a terminal failed state".to_owned(),
            )),
        }
    }

    fn handle_handshake_result(
        &mut self,
        result: Result<SslStream<DatagramIo>, HandshakeError<DatagramIo>>,
    ) -> Result<DatagramProgress, DtlsError> {
        match result {
            Ok(mut stream) => {
                let outbound_datagrams = stream.get_mut().take_output();
                let (keying, _) = verify_peer_and_export(&stream, self.expected_peer_fingerprint)?;
                self.state = DatagramState::Established(stream);
                Ok(DatagramProgress {
                    outbound_datagrams,
                    established_keying_material: Some(keying),
                    application_data: Vec::new(),
                })
            }
            Err(HandshakeError::WouldBlock(mut stream)) => {
                let outbound_datagrams = stream.get_mut().take_output();
                self.state = DatagramState::Handshaking(stream);
                Ok(DatagramProgress {
                    outbound_datagrams,
                    established_keying_material: None,
                    application_data: Vec::new(),
                })
            }
            Err(HandshakeError::Failure(stream)) => {
                Err(DtlsError::Handshake(stream.error().to_string()))
            }
            Err(HandshakeError::SetupFailure(error)) => Err(DtlsError::OpenSsl(error)),
        }
    }
}

fn empty_progress() -> DatagramProgress {
    DatagramProgress {
        outbound_datagrams: Vec::new(),
        established_keying_material: None,
        application_data: Vec::new(),
    }
}

/// Handshake-complete session and exported SRTP keys.
#[derive(Debug)]
pub struct EstablishedSession {
    stream: SslStream<ConnectedUdp>,
    keying_material: DirectionalKeyingMaterial,
    peer_fingerprint: Sha256Fingerprint,
}

impl EstablishedSession {
    fn finish(
        stream: SslStream<ConnectedUdp>,
        expected_peer_fingerprint: Sha256Fingerprint,
    ) -> Result<Self, DtlsError> {
        let (keying_material, peer_fingerprint) =
            verify_peer_and_export(&stream, expected_peer_fingerprint)?;
        Ok(Self {
            stream,
            keying_material,
            peer_fingerprint,
        })
    }

    /// Returns directional SRTP material.
    #[must_use]
    pub const fn keying_material(&self) -> &DirectionalKeyingMaterial {
        &self.keying_material
    }

    /// Returns the verified peer fingerprint.
    #[must_use]
    pub const fn peer_fingerprint(&self) -> Sha256Fingerprint {
        self.peer_fingerprint
    }

    /// Returns the underlying session for DTLS application data such as SCTP.
    #[must_use]
    pub const fn stream(&self) -> &SslStream<ConnectedUdp> {
        &self.stream
    }
}

fn verify_peer_and_export<S: Read + Write>(
    stream: &SslStream<S>,
    expected_peer_fingerprint: Sha256Fingerprint,
) -> Result<(DirectionalKeyingMaterial, Sha256Fingerprint), DtlsError> {
    let certificate = stream
        .ssl()
        .peer_certificate()
        .ok_or(DtlsError::MissingPeerCertificate)?;
    let peer_fingerprint = Sha256Fingerprint::from_certificate_der(&certificate.to_der()?);
    if peer_fingerprint != expected_peer_fingerprint {
        return Err(DtlsError::FingerprintMismatch);
    }
    let selected = stream
        .ssl()
        .selected_srtp_profile()
        .ok_or_else(|| DtlsError::UnsupportedSrtpProfile("none".to_owned()))?;
    let profile = DtlsSrtpProfile::parse_name(selected.name())?;
    let mut exported = [0_u8; EXPORTER_LEN];
    stream
        .ssl()
        .export_keying_material(&mut exported, EXPORTER_LABEL, None)?;
    let keying_material = split_srtp_exporter(profile, DtlsRole::Server, &exported)?;
    Ok((keying_material, peer_fingerprint))
}
