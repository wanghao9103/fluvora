use fluvora_rtcp::{
    GenericNack, NackEntry, Packet, PictureLossIndication, ReceiverReport, ReportBlock, SdesChunk,
    SdesItem, SenderReport, SourceDescription, TransportWideFeedback, TwccStatus, encode_compound,
    parse_compound,
};

fn report_block() -> ReportBlock {
    ReportBlock {
        ssrc: 0x0102_0304,
        fraction_lost: 7,
        cumulative_lost: -2,
        extended_highest_sequence: 70_000,
        jitter: 44,
        last_sender_report: 55,
        delay_since_last_sender_report: 66,
    }
}

#[test]
fn round_trips_reports_sdes_and_feedback() {
    let packets = vec![
        Packet::SenderReport(SenderReport {
            sender_ssrc: 1,
            ntp_timestamp: 0x1234_5678_90ab_cdef,
            rtp_timestamp: 90_000,
            sender_packet_count: 10,
            sender_octet_count: 1_000,
            reports: vec![report_block()],
        }),
        Packet::ReceiverReport(ReceiverReport {
            sender_ssrc: 2,
            reports: vec![report_block()],
        }),
        Packet::SourceDescription(SourceDescription {
            chunks: vec![SdesChunk {
                ssrc: 1,
                items: vec![SdesItem {
                    item_type: 1,
                    value: b"fluvora@example".to_vec(),
                }],
            }],
        }),
        Packet::GenericNack(GenericNack {
            sender_ssrc: 2,
            media_ssrc: 1,
            entries: vec![NackEntry {
                packet_id: 100,
                lost_packet_bitmask: 0b101,
            }],
        }),
        Packet::PictureLossIndication(PictureLossIndication {
            sender_ssrc: 2,
            media_ssrc: 1,
        }),
        Packet::TransportWideFeedback(TransportWideFeedback {
            sender_ssrc: 2,
            media_ssrc: 0,
            base_sequence_number: 65_530,
            reference_time: -7,
            feedback_packet_count: 9,
            statuses: vec![
                TwccStatus::NotReceived,
                TwccStatus::ReceivedSmallDelta(4),
                TwccStatus::ReceivedLargeDelta(-300),
                TwccStatus::ReceivedSmallDelta(255),
                TwccStatus::NotReceived,
                TwccStatus::ReceivedLargeDelta(500),
                TwccStatus::NotReceived,
                TwccStatus::ReceivedSmallDelta(1),
            ],
        }),
    ];

    let encoded = encode_compound(&packets).expect("valid compound packet");
    assert_eq!(
        parse_compound(&encoded).expect("encoded compound packet must parse"),
        packets
    );
}

#[test]
fn parses_run_length_twcc_chunk() {
    let packet = [
        0x8f, 205, 0, 6, // common header: FMT 15, 28 bytes
        0, 0, 0, 1, // sender SSRC
        0, 0, 0, 0, // media SSRC
        0, 10, 0, 4, // base sequence, status count
        0, 0, 1, 2, // reference time, feedback count
        0x20, 0x04, // run: small delta, four packets
        1, 2, 3, 4, // four small deltas
        0, 0, // RTCP word alignment
    ];
    let parsed = parse_compound(&packet).expect("valid TWCC feedback");
    let Packet::TransportWideFeedback(feedback) = &parsed[0] else {
        panic!("expected TWCC feedback");
    };
    assert_eq!(
        feedback.statuses,
        [
            TwccStatus::ReceivedSmallDelta(1),
            TwccStatus::ReceivedSmallDelta(2),
            TwccStatus::ReceivedSmallDelta(3),
            TwccStatus::ReceivedSmallDelta(4)
        ]
    );
}

#[test]
fn rejects_declared_length_beyond_datagram() {
    let packet = [0x80, 201, 0, 2, 0, 0, 0, 1];
    assert!(parse_compound(&packet).is_err());
}
