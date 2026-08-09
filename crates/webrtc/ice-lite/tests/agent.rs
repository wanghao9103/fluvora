use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use fluvora_ice_lite::{
    Agent, Configuration, Credentials, Event, IceError, IceState, IntegrityAlgorithm,
};
use fluvora_stun::{
    AttributeType, ErrorCode, Message, MessageBuilder, MessageClass, MessageType, Method,
    StunError, TransactionId,
};

const TRANSACTION_ID: TransactionId = TransactionId::new([
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
]);
const LOCAL_PASSWORD: &[u8] = b"local-password-is-long-enough";
const REMOTE_PASSWORD: &[u8] = b"remote-password-is-long-enough";

fn credentials(username: &str, password: &[u8]) -> Credentials {
    Credentials::new(username, password).expect("valid test credentials")
}

fn agent() -> Agent {
    Agent::new(Configuration::new(
        credentials("server", LOCAL_PASSWORD),
        credentials("browser", REMOTE_PASSWORD),
        0x1234_5678_90ab_cdef,
    ))
}

fn addresses() -> (SocketAddr, SocketAddr) {
    (
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), 3478)),
        SocketAddr::from((Ipv4Addr::new(198, 51, 100, 20), 50_000)),
    )
}

fn request(nominated: bool, role_controlling: bool, algorithm: IntegrityAlgorithm) -> Vec<u8> {
    let mut builder = MessageBuilder::new(
        MessageType::new(Method::BINDING, MessageClass::Request),
        TRANSACTION_ID,
    )
    .username("server:browser")
    .priority(1_849_562_550);
    builder = if role_controlling {
        builder.ice_controlling(55)
    } else {
        builder.ice_controlled(55)
    };
    if nominated {
        builder = builder.use_candidate();
    }
    builder = match algorithm {
        IntegrityAlgorithm::Sha1 => builder.message_integrity_sha1(LOCAL_PASSWORD.to_vec()),
        IntegrityAlgorithm::Sha256 => builder.message_integrity_sha256(LOCAL_PASSWORD.to_vec()),
    };
    builder
        .fingerprint()
        .build()
        .expect("valid connectivity check")
}

#[test]
fn accepts_and_nominates_peer_reflexive_pair() {
    let mut agent = agent();
    let (local, remote) = addresses();
    let output = agent
        .handle_datagram(
            Duration::from_secs(1),
            local,
            remote,
            &request(true, true, IntegrityAlgorithm::Sha1),
        )
        .expect("valid connectivity check");

    assert_eq!(agent.state(), IceState::Completed);
    assert_eq!(output.transmit.source, local);
    assert_eq!(output.transmit.destination, remote);
    assert!(matches!(
        output.events.as_slice(),
        [
            Event::SelectedPair(_),
            Event::StateChanged {
                from: IceState::New,
                to: IceState::Completed
            }
        ]
    ));
    let response = Message::parse(&output.transmit.payload).expect("valid STUN response");
    assert_eq!(
        response.message_type(),
        MessageType::new(Method::BINDING, MessageClass::SuccessResponse)
    );
    assert_eq!(response.xor_mapped_address(), Ok(Some(remote)));
    assert_eq!(
        response.verify_message_integrity_sha1(LOCAL_PASSWORD),
        Ok(())
    );
    assert_eq!(response.verify_fingerprint(), Ok(()));
}

#[test]
fn supports_sha256_checks_and_connected_state() {
    let mut agent = agent();
    let (local, remote) = addresses();
    let output = agent
        .handle_datagram(
            Duration::from_secs(1),
            local,
            remote,
            &request(false, true, IntegrityAlgorithm::Sha256),
        )
        .expect("valid SHA-256 check");

    assert_eq!(agent.state(), IceState::Connected);
    let response = Message::parse(&output.transmit.payload).expect("valid STUN response");
    assert_eq!(
        response.verify_message_integrity_sha256(LOCAL_PASSWORD),
        Ok(())
    );
}

#[test]
fn rejects_bad_integrity_without_reflection() {
    let mut bytes = request(false, true, IntegrityAlgorithm::Sha1);
    let integrity_header = bytes
        .windows(4)
        .position(|window| window == [0x00, 0x08, 0x00, 0x14])
        .expect("MESSAGE-INTEGRITY is present");
    bytes[integrity_header + 4] ^= 1;
    let mut agent = agent();
    let (local, remote) = addresses();

    assert_eq!(
        agent.handle_datagram(Duration::ZERO, local, remote, &bytes),
        Err(IceError::Stun(StunError::IntegrityMismatch))
    );
    assert_eq!(agent.state(), IceState::New);
}

#[test]
fn signs_role_conflict_and_unknown_attribute_errors() {
    let (local, remote) = addresses();
    let mut agent = agent();
    let role_output = agent
        .handle_datagram(
            Duration::ZERO,
            local,
            remote,
            &request(false, false, IntegrityAlgorithm::Sha1),
        )
        .expect("authenticated semantic error");
    let role_response =
        Message::parse(&role_output.transmit.payload).expect("valid error response");
    assert_eq!(
        role_response
            .error_code()
            .expect("valid error")
            .map(ErrorCode::code),
        Some(487)
    );
    assert!(
        role_response
            .ice_controlled()
            .expect("valid role")
            .is_some()
    );
    assert_eq!(
        role_response.verify_message_integrity_sha1(LOCAL_PASSWORD),
        Ok(())
    );

    let unknown_type = AttributeType::new(0x1234);
    let unknown_request = MessageBuilder::new(
        MessageType::new(Method::BINDING, MessageClass::Request),
        TRANSACTION_ID,
    )
    .username("server:browser")
    .priority(100)
    .ice_controlling(99)
    .raw_attribute(unknown_type, vec![1])
    .message_integrity_sha1(LOCAL_PASSWORD.to_vec())
    .fingerprint()
    .build()
    .expect("valid STUN shape");
    let unknown_output = agent
        .handle_datagram(Duration::ZERO, local, remote, &unknown_request)
        .expect("authenticated semantic error");
    let unknown_response =
        Message::parse(&unknown_output.transmit.payload).expect("valid error response");
    assert_eq!(
        unknown_response
            .error_code()
            .expect("valid error")
            .map(ErrorCode::code),
        Some(420)
    );
    assert_eq!(
        unknown_response.unknown_attributes(),
        Ok(vec![unknown_type])
    );
}

#[test]
fn expires_consent_then_requires_restart() {
    let mut agent = agent();
    let (local, remote) = addresses();
    agent
        .handle_datagram(
            Duration::from_secs(1),
            local,
            remote,
            &request(true, true, IntegrityAlgorithm::Sha1),
        )
        .expect("valid connectivity check");

    assert_eq!(
        agent.tick(Duration::from_secs(31)),
        vec![Event::StateChanged {
            from: IceState::Completed,
            to: IceState::Disconnected
        }]
    );
    assert_eq!(
        agent.tick(Duration::from_secs(61)),
        vec![Event::StateChanged {
            from: IceState::Disconnected,
            to: IceState::Failed
        }]
    );
    assert_eq!(
        agent.handle_datagram(
            Duration::from_secs(62),
            local,
            remote,
            &request(true, true, IntegrityAlgorithm::Sha1)
        ),
        Err(IceError::RestartRequired)
    );
}

#[test]
fn validates_credential_boundaries() {
    assert!(Credentials::new("", LOCAL_PASSWORD).is_err());
    assert!(Credentials::new("bad:name", LOCAL_PASSWORD).is_err());
    assert!(Credentials::new("ok", b"too-short".to_vec()).is_err());
}
