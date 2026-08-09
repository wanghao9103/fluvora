use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use fluvora_dtls_adapter::{DtlsRole, DtlsSrtpProfile, split_srtp_exporter};
use fluvora_ice_lite::{Agent, Configuration, Credentials};
use fluvora_rtc_session::{Session, SessionAction, SessionError, SessionState};
use fluvora_rtp::{Packet, PacketBuilder};
use fluvora_srtp::SrtpContext;
use fluvora_stun::{MessageBuilder, MessageClass, MessageType, Method, TransactionId};

const LOCAL_PASSWORD: &[u8] = b"local-password-is-long-enough";

fn session() -> Session {
    let local = Credentials::new("server", LOCAL_PASSWORD).expect("credentials");
    let remote =
        Credentials::new("browser", b"browser-password-is-long-enough").expect("credentials");
    Session::new(Agent::new(Configuration::new(local, remote, 55)))
}

fn addresses() -> (SocketAddr, SocketAddr) {
    (
        SocketAddr::from((Ipv4Addr::LOCALHOST, 3_478)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 50_000)),
    )
}

fn nominate() -> Vec<u8> {
    MessageBuilder::new(
        MessageType::new(Method::BINDING, MessageClass::Request),
        TransactionId::new([1; 12]),
    )
    .username("server:browser")
    .priority(1_000)
    .ice_controlling(99)
    .use_candidate()
    .message_integrity_sha1(LOCAL_PASSWORD.to_vec())
    .fingerprint()
    .build()
    .expect("valid connectivity check")
}

#[test]
fn pins_tuple_and_gates_dtls_on_nomination() {
    let mut session = session();
    let (local, remote) = addresses();
    let actions = session
        .handle_datagram(Duration::ZERO, local, remote, &nominate())
        .expect("valid nomination");
    assert_eq!(session.state(), SessionState::DtlsHandshaking);
    assert!(actions.iter().any(|action| matches!(
        action,
        SessionAction::StateChanged {
            from: SessionState::New,
            to: SessionState::DtlsHandshaking
        }
    )));
    assert_eq!(
        session.handle_datagram(
            Duration::ZERO,
            local,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 50_001)),
            &[22, 0xfe, 0xfd]
        ),
        Err(SessionError::TupleMismatch)
    );
    assert_eq!(
        session
            .handle_datagram(Duration::ZERO, local, remote, &[22, 0xfe, 0xfd])
            .expect("selected DTLS record"),
        vec![SessionAction::DtlsInput(vec![22, 0xfe, 0xfd])]
    );
}

#[test]
fn decrypts_only_after_dtls_keys_and_protects_return_path() {
    let mut session = session();
    let (local, remote) = addresses();
    session
        .handle_datagram(Duration::ZERO, local, remote, &nominate())
        .expect("valid nomination");
    let exported: Vec<u8> = (0..60).collect();
    let server_keys = split_srtp_exporter(
        DtlsSrtpProfile::Aes128CmSha1_80,
        DtlsRole::Server,
        &exported,
    )
    .expect("server keys");
    let client_keys = split_srtp_exporter(
        DtlsSrtpProfile::Aes128CmSha1_80,
        DtlsRole::Client,
        &exported,
    )
    .expect("client keys");
    session
        .install_dtls_keying_material(&server_keys)
        .expect("install verified keys");
    assert_eq!(session.state(), SessionState::Connected);

    let clear = PacketBuilder::new(111, 1, 48_000, 7, b"audio")
        .build()
        .expect("valid RTP");
    let mut client_context = SrtpContext::new(
        client_keys.profile,
        &client_keys.outbound,
        &client_keys.inbound,
    );
    let mut protected = clear.clone();
    client_context
        .protect_rtp(&mut protected)
        .expect("client protect");
    let actions = session
        .handle_datagram(Duration::ZERO, local, remote, &protected)
        .expect("server unprotect");
    assert_eq!(actions, vec![SessionAction::InboundRtp(clear.clone())]);

    let transmit = session.protect_rtp(clear).expect("server protect");
    let plaintext = client_context
        .unprotect_rtp(&transmit.payload)
        .expect("client unprotect");
    assert_eq!(
        Packet::parse(&plaintext).expect("valid RTP").payload(),
        b"audio"
    );
}
