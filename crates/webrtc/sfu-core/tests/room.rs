use std::time::Duration;

use fluvora_media_codec::Codec;
use fluvora_rtcp::{GenericNack, NackEntry, Packet as RtcpPacket, encode_compound};
use fluvora_rtp::{Packet, PacketBuilder};
use fluvora_sfu_core::{
    Encoding, Layer, MediaKind, ParticipantId, PublishedTrack, Room, RoomConfig, SfuError,
    SfuEvent, SubscriptionConfig, SubscriptionId, TrackId,
};

const PUBLISHER: ParticipantId = ParticipantId(1);
const SUBSCRIBER: ParticipantId = ParticipantId(2);
const TRACK: TrackId = TrackId(10);
const SUBSCRIPTION: SubscriptionId = SubscriptionId(20);

fn room() -> Room {
    let mut room = Room::new(RoomConfig::default());
    room.publish(PublishedTrack {
        id: TRACK,
        owner: PUBLISHER,
        kind: MediaKind::Video,
        codec: Codec::Vp8,
        clock_rate: 90_000,
        payload_type: 96,
        encodings: vec![
            Encoding {
                ssrc: 100,
                rid: Some("low".to_owned()),
                spatial_layer: 0,
                max_bitrate_bps: 250_000,
            },
            Encoding {
                ssrc: 200,
                rid: Some("high".to_owned()),
                spatial_layer: 1,
                max_bitrate_bps: 1_500_000,
            },
        ],
    })
    .expect("publish track");
    room.subscribe(SubscriptionConfig {
        id: SUBSCRIPTION,
        subscriber: SUBSCRIBER,
        track_id: TRACK,
        output_ssrc: 900,
        output_payload_type: 120,
        initial_layer: Layer {
            spatial: 0,
            temporal: 2,
        },
        initial_sequence_number: 1_000,
        initial_timestamp: 5_000,
        extension_rewrites: Vec::new(),
    })
    .expect("subscribe");
    room
}

fn vp8_packet(ssrc: u32, sequence: u16, timestamp: u32, keyframe: bool) -> Vec<u8> {
    let frame_tag = u8::from(!keyframe);
    PacketBuilder::new(96, sequence, timestamp, ssrc, &[0x10, frame_tag])
        .marker(true)
        .build()
        .expect("valid RTP packet")
}

#[test]
fn switches_simulcast_only_on_target_keyframe_with_continuity() {
    let mut room = room();
    let low_key = room
        .handle_rtp(Duration::ZERO, PUBLISHER, &vp8_packet(100, 1, 10_000, true))
        .expect("route low keyframe");
    assert_eq!(low_key.packets.len(), 1);
    let first = Packet::parse(&low_key.packets[0].packet).expect("valid output");
    assert_eq!(first.header().sequence_number, 1_000);
    assert_eq!(first.header().timestamp, 5_000);
    assert_eq!(first.header().ssrc, 900);

    let events = room
        .set_target_layer(
            Duration::from_secs(1),
            SUBSCRIBER,
            SUBSCRIPTION,
            Layer {
                spatial: 1,
                temporal: 2,
            },
        )
        .expect("set target");
    assert!(matches!(
        events.as_slice(),
        [SfuEvent::PictureLossIndication {
            track_id: TRACK,
            media_ssrc: 200
        }]
    ));

    let old_layer = room
        .handle_rtp(
            Duration::from_millis(1_100),
            PUBLISHER,
            &vp8_packet(100, 2, 13_000, false),
        )
        .expect("continue old layer");
    assert_eq!(old_layer.packets.len(), 1);
    let high_inter = room
        .handle_rtp(
            Duration::from_millis(1_200),
            PUBLISHER,
            &vp8_packet(200, 10, 20_000, false),
        )
        .expect("drop target inter frame");
    assert!(high_inter.packets.is_empty());

    let switched = room
        .handle_rtp(
            Duration::from_millis(1_600),
            PUBLISHER,
            &vp8_packet(200, 11, 23_000, true),
        )
        .expect("switch on keyframe");
    assert_eq!(switched.packets.len(), 1);
    assert!(switched.events.iter().any(|event| matches!(
        event,
        SfuEvent::LayerSwitched {
            subscription_id: SUBSCRIPTION,
            from: Some(0),
            to: 1
        }
    )));
    let second = Packet::parse(&old_layer.packets[0].packet).expect("valid output");
    let third = Packet::parse(&switched.packets[0].packet).expect("valid output");
    assert_eq!(second.header().sequence_number, 1_001);
    assert_eq!(third.header().sequence_number, 1_002);
    assert!(
        third
            .header()
            .timestamp
            .wrapping_sub(second.header().timestamp)
            > 0
    );
}

#[test]
fn serves_bounded_nack_retransmissions_and_checks_subscriber() {
    let mut room = room();
    room.handle_rtp(Duration::ZERO, PUBLISHER, &vp8_packet(100, 1, 10_000, true))
        .expect("route packet");
    let nack = encode_compound(&[RtcpPacket::GenericNack(GenericNack {
        sender_ssrc: 700,
        media_ssrc: 900,
        entries: vec![NackEntry {
            packet_id: 1_000,
            lost_packet_bitmask: 0,
        }],
    })])
    .expect("encode NACK");
    let output = room
        .handle_rtcp(Duration::from_millis(100), SUBSCRIBER, &nack)
        .expect("handle NACK");
    assert_eq!(output.retransmissions.len(), 1);
    assert!(output.retransmissions[0].retransmission);
    let unauthorized = room
        .handle_rtcp(Duration::from_millis(100), ParticipantId(99), &nack)
        .expect("ignore unauthorized feedback");
    assert!(unauthorized.retransmissions.is_empty());
}

#[test]
fn rejects_spoofed_publishers() {
    let mut room = room();
    assert_eq!(
        room.handle_rtp(
            Duration::ZERO,
            ParticipantId(99),
            &vp8_packet(100, 1, 10_000, true)
        ),
        Err(SfuError::UnauthorizedParticipant(ParticipantId(99)))
    );
}
