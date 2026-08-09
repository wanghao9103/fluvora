use fluvora_rtp::{
    ExtensionFormat, ExtensionRewrite, OwnedHeaderExtension, Packet, PacketBuilder, Rewrite,
    RtpError, SequenceNumberExtender, TimestampExtender, rewrite_header_extensions,
};

#[test]
fn round_trips_header_extensions_payload_and_padding() {
    let extensions = vec![
        OwnedHeaderExtension {
            id: 1,
            value: vec![0x42],
        },
        OwnedHeaderExtension {
            id: 3,
            value: vec![1, 2, 3],
        },
    ];
    let bytes = PacketBuilder::new(111, 65_535, 0x1234_5678, 0x90ab_cdef, b"opus")
        .marker(true)
        .csrcs(vec![7, 8])
        .extensions(ExtensionFormat::OneByte, extensions)
        .padding(4)
        .build()
        .expect("valid RTP packet");

    let packet = Packet::parse(&bytes).expect("builder output must parse");
    assert!(packet.header().marker);
    assert_eq!(packet.header().payload_type, 111);
    assert_eq!(packet.header().sequence_number, 65_535);
    assert_eq!(packet.header().timestamp, 0x1234_5678);
    assert_eq!(packet.header().ssrc, 0x90ab_cdef);
    assert_eq!(packet.header().csrcs, [7, 8]);
    assert_eq!(packet.extension_format(), Some(ExtensionFormat::OneByte));
    assert_eq!(packet.extensions()[0].id, 1);
    assert_eq!(packet.extensions()[0].value, [0x42]);
    assert_eq!(packet.extensions()[1].value, [1, 2, 3]);
    assert_eq!(packet.payload(), b"opus");
    assert_eq!(packet.padding_len(), 4);
}

#[test]
fn parses_two_byte_and_opaque_profiles() {
    let bytes = PacketBuilder::new(96, 1, 2, 3, b"x")
        .extensions(
            ExtensionFormat::TwoByte,
            vec![OwnedHeaderExtension {
                id: 200,
                value: vec![1; 17],
            }],
        )
        .build()
        .expect("valid two-byte extensions");
    let packet = Packet::parse(&bytes).expect("valid RTP packet");
    assert_eq!(packet.extensions()[0].id, 200);
    assert_eq!(packet.extensions()[0].value, [1; 17]);

    let opaque = [
        0x90, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0xab, 0xcd, 0, 1, 1, 2, 3, 4, 9,
    ];
    let packet = Packet::parse(&opaque).expect("valid opaque extension");
    assert_eq!(
        packet.extension_format(),
        Some(ExtensionFormat::Opaque(0xabcd))
    );
    assert_eq!(packet.extension_data(), Some([1, 2, 3, 4].as_slice()));
    assert!(packet.extensions().is_empty());
    assert_eq!(packet.payload(), [9]);
}

#[test]
fn rejects_truncation_and_invalid_padding() {
    assert_eq!(
        Packet::parse(&[0x80; 11]),
        Err(RtpError::PacketTooShort(11))
    );
    let truncated_extension = [0x90, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0xbe, 0xde, 0, 2];
    assert_eq!(
        Packet::parse(&truncated_extension),
        Err(RtpError::TruncatedHeader)
    );
    let invalid_padding = [0xa0, 96, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0];
    assert_eq!(
        Packet::parse(&invalid_padding),
        Err(RtpError::InvalidPadding(0))
    );
}

#[test]
fn rewrites_only_fixed_sfu_fields() {
    let mut bytes = PacketBuilder::new(96, 10, 20, 30, b"video")
        .build()
        .expect("valid RTP packet");
    Rewrite {
        marker: Some(true),
        payload_type: Some(100),
        sequence_number: Some(500),
        timestamp: Some(600),
        ssrc: Some(700),
    }
    .apply(&mut bytes)
    .expect("valid rewrite");
    let packet = Packet::parse(&bytes).expect("rewritten packet remains valid");
    assert!(packet.header().marker);
    assert_eq!(packet.header().payload_type, 100);
    assert_eq!(packet.header().sequence_number, 500);
    assert_eq!(packet.header().timestamp, 600);
    assert_eq!(packet.header().ssrc, 700);
    assert_eq!(packet.payload(), b"video");
}

#[test]
fn extends_sequence_and_timestamp_wraps_with_reordering() {
    let mut sequence = SequenceNumberExtender::new();
    assert_eq!(sequence.extend(65_534), 65_534);
    assert_eq!(sequence.extend(65_535), 65_535);
    assert_eq!(sequence.extend(0), 65_536);
    assert_eq!(sequence.extend(65_535), 65_535);
    assert_eq!(sequence.extend(1), 65_537);
    assert_eq!(sequence.highest(), Some(65_537));

    let mut timestamp = TimestampExtender::new();
    assert_eq!(timestamp.extend(u32::MAX - 1), u64::from(u32::MAX) - 1);
    assert_eq!(timestamp.extend(1), (1_u64 << 32) + 1);
    assert_eq!(timestamp.extend(u32::MAX), u64::from(u32::MAX));
}

#[test]
fn remaps_replaces_and_removes_negotiated_extensions() {
    let input = PacketBuilder::new(96, 4, 5, 6, b"frame")
        .marker(true)
        .extensions(
            ExtensionFormat::OneByte,
            vec![
                OwnedHeaderExtension {
                    id: 1,
                    value: b"publisher-mid".to_vec(),
                },
                OwnedHeaderExtension {
                    id: 2,
                    value: b"rid".to_vec(),
                },
                OwnedHeaderExtension {
                    id: 3,
                    value: vec![0, 9],
                },
            ],
        )
        .padding(4)
        .build()
        .expect("valid RTP packet");

    let output = rewrite_header_extensions(
        &input,
        &[
            ExtensionRewrite {
                source_id: 1,
                destination_id: Some(4),
                replacement: Some(b"subscriber".to_vec()),
            },
            ExtensionRewrite {
                source_id: 2,
                destination_id: None,
                replacement: None,
            },
            ExtensionRewrite {
                source_id: 3,
                destination_id: Some(7),
                replacement: None,
            },
        ],
    )
    .expect("negotiated extension rewrite");
    let packet = Packet::parse(&output).expect("rewritten packet");

    assert!(packet.header().marker);
    assert_eq!(packet.payload(), b"frame");
    assert_eq!(packet.padding_len(), 4);
    assert_eq!(packet.extensions().len(), 2);
    assert_eq!(packet.extensions()[0].id, 4);
    assert_eq!(packet.extensions()[0].value, b"subscriber");
    assert_eq!(packet.extensions()[1].id, 7);
    assert_eq!(packet.extensions()[1].value, [0, 9]);
}
