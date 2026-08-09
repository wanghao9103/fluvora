use std::time::Duration;

use fluvora_congestion_control::{
    BandwidthEstimator, BandwidthEstimatorConfig, BandwidthUsage, LayerOption, LayerSelector,
    SentPacket,
};
use fluvora_rtcp::{TransportWideFeedback, TwccStatus};
use fluvora_sfu_core::Layer;

#[test]
fn reduces_rate_on_loss_and_recovers_additively() {
    let mut estimator = BandwidthEstimator::new(BandwidthEstimatorConfig::default());
    for sequence in 100..120 {
        estimator.register_sent(SentPacket {
            sequence_number: sequence,
            sent_at: Duration::from_millis(u64::from(sequence - 100) * 10),
            size_bytes: 1_200,
        });
    }
    let lossy = TransportWideFeedback {
        sender_ssrc: 1,
        media_ssrc: 0,
        base_sequence_number: 100,
        reference_time: 0,
        feedback_packet_count: 1,
        statuses: (0..20)
            .map(|index| {
                if index % 4 == 0 {
                    TwccStatus::NotReceived
                } else {
                    TwccStatus::ReceivedSmallDelta(40)
                }
            })
            .collect(),
    };
    let reduced = estimator.process_feedback(Duration::from_secs(1), &lossy);
    assert_eq!(reduced.usage, BandwidthUsage::Overusing);
    assert_eq!(reduced.loss_per_mille, 250);
    assert!(reduced.target_bitrate_bps < 800_000);

    for sequence in 120..140 {
        estimator.register_sent(SentPacket {
            sequence_number: sequence,
            sent_at: Duration::from_millis(1_000 + u64::from(sequence - 120) * 10),
            size_bytes: 1_200,
        });
    }
    let healthy = TransportWideFeedback {
        sender_ssrc: 1,
        media_ssrc: 0,
        base_sequence_number: 120,
        reference_time: 1,
        feedback_packet_count: 2,
        statuses: vec![TwccStatus::ReceivedSmallDelta(40); 20],
    };
    let recovered = estimator.process_feedback(Duration::from_secs(2), &healthy);
    assert_eq!(recovered.usage, BandwidthUsage::Underusing);
    assert!(recovered.target_bitrate_bps > reduced.target_bitrate_bps);
}

#[test]
fn downgrades_immediately_and_delays_upgrade() {
    let options = [
        LayerOption {
            layer: Layer {
                spatial: 0,
                temporal: 0,
            },
            minimum_bitrate_bps: 150_000,
        },
        LayerOption {
            layer: Layer {
                spatial: 1,
                temporal: 1,
            },
            minimum_bitrate_bps: 600_000,
        },
        LayerOption {
            layer: Layer {
                spatial: 2,
                temporal: 2,
            },
            minimum_bitrate_bps: 1_500_000,
        },
    ];
    let mut selector = LayerSelector::default();
    assert_eq!(
        selector.select(Duration::ZERO, &options, 1_800_000),
        Some(options[2].layer)
    );
    assert_eq!(
        selector.select(Duration::from_millis(100), &options, 500_000),
        Some(options[0].layer)
    );
    assert_eq!(
        selector.select(Duration::from_secs(1), &options, 2_000_000),
        Some(options[0].layer)
    );
    assert_eq!(
        selector.select(Duration::from_secs(3), &options, 2_000_000),
        Some(options[2].layer)
    );
}
