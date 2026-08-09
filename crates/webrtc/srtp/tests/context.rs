use fluvora_rtcp::{Packet as RtcpPacket, PictureLossIndication, encode_compound, parse_compound};
use fluvora_rtp::{Packet, PacketBuilder};
use fluvora_srtp::{KeyingMaterial, ProtectionProfile, SrtpContext, SrtpError};

fn material(seed: u8) -> KeyingMaterial {
    let key: Vec<u8> = (seed..seed + 16).collect();
    let salt: Vec<u8> = (seed + 20..seed + 34).collect();
    KeyingMaterial::new(&key, &salt).expect("valid key material")
}

fn peers(profile: ProtectionProfile) -> (SrtpContext, SrtpContext) {
    let client = material(1);
    let server = material(50);
    (
        SrtpContext::new(profile, &client, &server),
        SrtpContext::new(profile, &server, &client),
    )
}

#[test]
fn protects_rtp_and_rejects_tampering_and_replay() {
    let (mut sender, mut receiver) = peers(ProtectionProfile::Aes128CmSha1_80);
    let clear = PacketBuilder::new(111, 65_535, 48_000, 0x1234_5678, b"secret audio")
        .build()
        .expect("valid RTP");
    let mut protected = clear.clone();
    sender.protect_rtp(&mut protected).expect("protect RTP");
    assert_ne!(protected, clear);
    assert_eq!(
        receiver.unprotect_rtp(&protected).expect("unprotect RTP"),
        clear
    );
    assert_eq!(
        receiver.unprotect_rtp(&protected),
        Err(SrtpError::ReplayDetected)
    );

    let mut next = PacketBuilder::new(111, 0, 48_960, 0x1234_5678, b"next")
        .build()
        .expect("valid RTP");
    sender.protect_rtp(&mut next).expect("rollover is valid");
    let mut tampered = next.clone();
    tampered[12] ^= 1;
    assert_eq!(
        receiver.unprotect_rtp(&tampered),
        Err(SrtpError::AuthenticationFailed)
    );
    let plaintext = receiver
        .unprotect_rtp(&next)
        .expect("valid packet after tamper");
    assert_eq!(
        Packet::parse(&plaintext).expect("valid RTP").payload(),
        b"next"
    );
}

#[test]
fn supports_32_bit_rtp_authentication_profile() {
    let (mut sender, mut receiver) = peers(ProtectionProfile::Aes128CmSha1_32);
    let mut packet = PacketBuilder::new(96, 1, 90_000, 77, b"video")
        .build()
        .expect("valid RTP");
    let clear_len = packet.len();
    sender.protect_rtp(&mut packet).expect("protect RTP");
    assert_eq!(packet.len(), clear_len + 4);
    receiver.unprotect_rtp(&packet).expect("unprotect RTP");
}

#[test]
fn protects_srtcp_and_enforces_replay_window() {
    let (mut sender, mut receiver) = peers(ProtectionProfile::Aes128CmSha1_80);
    let clear = encode_compound(&[RtcpPacket::PictureLossIndication(PictureLossIndication {
        sender_ssrc: 8,
        media_ssrc: 9,
    })])
    .expect("valid RTCP");
    let mut protected = clear.clone();
    sender.protect_rtcp(&mut protected).expect("protect RTCP");
    assert_ne!(protected, clear);
    let plaintext = receiver.unprotect_rtcp(&protected).expect("unprotect RTCP");
    assert_eq!(plaintext, clear);
    assert_eq!(
        parse_compound(&plaintext).expect("valid RTCP")[0],
        RtcpPacket::PictureLossIndication(PictureLossIndication {
            sender_ssrc: 8,
            media_ssrc: 9
        })
    );
    assert_eq!(
        receiver.unprotect_rtcp(&protected),
        Err(SrtpError::ReplayDetected)
    );
}

#[test]
fn rejects_non_monotonic_outbound_sequence() {
    let (mut sender, _) = peers(ProtectionProfile::Aes128CmSha1_80);
    let mut first = PacketBuilder::new(96, 10, 1, 7, b"a")
        .build()
        .expect("valid RTP");
    sender.protect_rtp(&mut first).expect("first packet");
    let mut duplicate = PacketBuilder::new(96, 10, 2, 7, b"b")
        .build()
        .expect("valid RTP");
    assert_eq!(
        sender.protect_rtp(&mut duplicate),
        Err(SrtpError::NonMonotonicSequence)
    );
}
