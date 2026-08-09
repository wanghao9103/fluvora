use std::collections::VecDeque;
use std::time::Duration;

use fluvora_rtcp::{TransportWideFeedback, TwccStatus};

/// One transport-wide sequence number recorded at socket transmission time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentPacket {
    /// Transport-wide sequence number written to the RTP header extension.
    pub sequence_number: u16,
    /// Monotonic send timestamp.
    pub sent_at: Duration,
    /// Complete UDP payload bytes.
    pub size_bytes: usize,
}

/// Delay/loss interpretation of the newest feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthUsage {
    /// Low loss and delay permit probing upward.
    Underusing,
    /// No strong congestion or spare-capacity signal.
    Normal,
    /// Loss or queuing delay requires multiplicative reduction.
    Overusing,
}

/// One estimator output suitable for pacing and layer allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    /// AIMD target bitrate after this update.
    pub target_bitrate_bps: u64,
    /// Delivery rate observed within matching received packets.
    pub delivery_rate_bps: u64,
    /// Lost share in per-mille units.
    pub loss_per_mille: u16,
    /// Smoothed receive-minus-send inter-packet delta.
    pub delay_trend_micros: i64,
    /// Current bandwidth usage classification.
    pub usage: BandwidthUsage,
    /// Feedback statuses that matched retained send history.
    pub matched_packets: usize,
}

/// Resource bounds and AIMD limits.
#[derive(Debug, Clone)]
pub struct BandwidthEstimatorConfig {
    /// Initial target before useful feedback.
    pub initial_bitrate_bps: u64,
    /// Absolute target floor.
    pub minimum_bitrate_bps: u64,
    /// Absolute target ceiling.
    pub maximum_bitrate_bps: u64,
    /// Maximum retained transport packets.
    pub history_packets: usize,
    /// Maximum send-history age.
    pub history_age: Duration,
}

impl Default for BandwidthEstimatorConfig {
    fn default() -> Self {
        Self {
            initial_bitrate_bps: 800_000,
            minimum_bitrate_bps: 50_000,
            maximum_bitrate_bps: 20_000_000,
            history_packets: 16_384,
            history_age: Duration::from_secs(10),
        }
    }
}

/// Bounded TWCC send history and conservative AIMD controller.
#[derive(Debug)]
pub struct BandwidthEstimator {
    configuration: BandwidthEstimatorConfig,
    history: VecDeque<SentPacket>,
    target_bitrate_bps: u64,
    delay_trend_micros: i64,
    last_update: Option<Duration>,
}

impl BandwidthEstimator {
    /// Creates an estimator at its configured initial rate.
    #[must_use]
    pub fn new(configuration: BandwidthEstimatorConfig) -> Self {
        let target_bitrate_bps = configuration.initial_bitrate_bps.clamp(
            configuration.minimum_bitrate_bps,
            configuration.maximum_bitrate_bps,
        );
        Self {
            configuration,
            history: VecDeque::new(),
            target_bitrate_bps,
            delay_trend_micros: 0,
            last_update: None,
        }
    }

    /// Records a packet after the runtime has handed it to the UDP socket.
    pub fn register_sent(&mut self, packet: SentPacket) {
        self.expire_history(packet.sent_at);
        while self.history.len() >= self.configuration.history_packets {
            self.history.pop_front();
        }
        if self.configuration.history_packets > 0 {
            self.history.push_back(packet);
        }
    }

    /// Applies one decoded transport-wide feedback report.
    #[must_use]
    pub fn process_feedback(
        &mut self,
        now: Duration,
        feedback: &TransportWideFeedback,
    ) -> Estimate {
        self.expire_history(now);
        let observations = self.match_feedback(feedback);
        let metrics = calculate_metrics(&observations);
        if let Some(sample) = metrics.delay_sample_micros {
            self.delay_trend_micros = (self.delay_trend_micros * 7 + sample) / 8;
        }
        let usage = classify(metrics.loss_per_mille, self.delay_trend_micros);
        self.update_target(now, metrics.delivery_rate_bps, usage);
        self.last_update = Some(now);
        Estimate {
            target_bitrate_bps: self.target_bitrate_bps,
            delivery_rate_bps: metrics.delivery_rate_bps,
            loss_per_mille: metrics.loss_per_mille,
            delay_trend_micros: self.delay_trend_micros,
            usage,
            matched_packets: observations.len(),
        }
    }

    /// Returns the current pacing/allocation target.
    #[must_use]
    pub const fn target_bitrate_bps(&self) -> u64 {
        self.target_bitrate_bps
    }

    fn match_feedback(&self, feedback: &TransportWideFeedback) -> Vec<Observation> {
        let mut observations = Vec::new();
        let mut receive_time_ticks = 0_i64;
        for (offset, status) in feedback.statuses.iter().enumerate() {
            let sequence_number = feedback
                .base_sequence_number
                .wrapping_add(u16::try_from(offset).unwrap_or_default());
            let sent = self
                .history
                .iter()
                .rev()
                .find(|packet| packet.sequence_number == sequence_number);
            let received_at_ticks = match status {
                TwccStatus::NotReceived => None,
                TwccStatus::ReceivedSmallDelta(delta) => {
                    receive_time_ticks += i64::from(*delta);
                    Some(receive_time_ticks)
                }
                TwccStatus::ReceivedLargeDelta(delta) => {
                    receive_time_ticks += i64::from(*delta);
                    Some(receive_time_ticks)
                }
            };
            if let Some(sent) = sent {
                observations.push(Observation {
                    sent_at: sent.sent_at,
                    size_bytes: sent.size_bytes,
                    received_at_micros: received_at_ticks.map(|ticks| ticks * 250),
                });
            }
        }
        observations
    }

    fn update_target(&mut self, now: Duration, delivery_rate_bps: u64, usage: BandwidthUsage) {
        match usage {
            BandwidthUsage::Overusing => {
                let baseline = if delivery_rate_bps > 0 {
                    self.target_bitrate_bps.min(delivery_rate_bps)
                } else {
                    self.target_bitrate_bps
                };
                self.target_bitrate_bps = baseline.saturating_mul(85) / 100;
            }
            BandwidthUsage::Underusing => {
                let elapsed_millis = self.last_update.map_or(1_000, |last| {
                    now.saturating_sub(last).as_millis().clamp(1, 5_000)
                });
                let per_second = (self.target_bitrate_bps / 20).max(10_000);
                let increase = u128::from(per_second)
                    .saturating_mul(elapsed_millis)
                    .checked_div(1_000)
                    .and_then(|value| u64::try_from(value).ok())
                    .unwrap_or(u64::MAX);
                self.target_bitrate_bps = self.target_bitrate_bps.saturating_add(increase);
                if delivery_rate_bps > 0 {
                    self.target_bitrate_bps = self
                        .target_bitrate_bps
                        .min(delivery_rate_bps.saturating_mul(11) / 10);
                }
            }
            BandwidthUsage::Normal => {}
        }
        self.target_bitrate_bps = self.target_bitrate_bps.clamp(
            self.configuration.minimum_bitrate_bps,
            self.configuration.maximum_bitrate_bps,
        );
    }

    fn expire_history(&mut self, now: Duration) {
        while self.history.front().is_some_and(|packet| {
            now.saturating_sub(packet.sent_at) > self.configuration.history_age
        }) {
            self.history.pop_front();
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    sent_at: Duration,
    size_bytes: usize,
    received_at_micros: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct Metrics {
    delivery_rate_bps: u64,
    loss_per_mille: u16,
    delay_sample_micros: Option<i64>,
}

fn calculate_metrics(observations: &[Observation]) -> Metrics {
    if observations.is_empty() {
        return Metrics {
            delivery_rate_bps: 0,
            loss_per_mille: 0,
            delay_sample_micros: None,
        };
    }
    let received: Vec<_> = observations
        .iter()
        .filter_map(|observation| {
            observation
                .received_at_micros
                .map(|received| (*observation, received))
        })
        .collect();
    let lost = observations.len() - received.len();
    let loss_per_mille =
        u16::try_from(lost.saturating_mul(1_000) / observations.len()).unwrap_or(u16::MAX);
    let delivery_rate_bps = delivery_rate(&received);
    let delay_sample_micros = delay_sample(&received);
    Metrics {
        delivery_rate_bps,
        loss_per_mille,
        delay_sample_micros,
    }
}

fn delivery_rate(received: &[(Observation, i64)]) -> u64 {
    let (Some(first), Some(last)) = (received.first(), received.last()) else {
        return 0;
    };
    let receive_span = last.1.saturating_sub(first.1);
    let send_span_u128 = last.0.sent_at.saturating_sub(first.0.sent_at).as_micros();
    let send_span = i64::try_from(send_span_u128).unwrap_or(i64::MAX);
    let span_micros = receive_span.max(send_span).max(1);
    let bytes: u128 = received
        .iter()
        .map(|(observation, _)| observation.size_bytes as u128)
        .sum();
    bytes
        .saturating_mul(8_000_000)
        .checked_div(u128::try_from(span_micros).unwrap_or(u128::MAX))
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(u64::MAX)
}

fn delay_sample(received: &[(Observation, i64)]) -> Option<i64> {
    let mut samples = 0_i64;
    let mut sum = 0_i64;
    for pair in received.windows(2) {
        let send_delta_u128 = pair[1]
            .0
            .sent_at
            .saturating_sub(pair[0].0.sent_at)
            .as_micros();
        let send_delta = i64::try_from(send_delta_u128).unwrap_or(i64::MAX);
        let receive_delta = pair[1].1.saturating_sub(pair[0].1);
        sum = sum.saturating_add(receive_delta.saturating_sub(send_delta));
        samples += 1;
    }
    (samples > 0).then(|| sum / samples)
}

const fn classify(loss_per_mille: u16, delay_trend_micros: i64) -> BandwidthUsage {
    if loss_per_mille > 100 || delay_trend_micros > 15_000 {
        BandwidthUsage::Overusing
    } else if loss_per_mille < 20 && delay_trend_micros < 5_000 {
        BandwidthUsage::Underusing
    } else {
        BandwidthUsage::Normal
    }
}
