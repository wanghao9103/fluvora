//! Low-cardinality media-node metrics and component health snapshots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::RwLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Stable prefix used by every exported metric.
pub const METRIC_PREFIX: &str = "fluvora";

/// Monotonic saturating process counter.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Adds `value` without wrapping at `u64::MAX`.
    pub fn add(&self, value: u64) {
        saturating_add_u64(&self.0, value);
    }

    /// Increments by one.
    pub fn increment(&self) {
        self.add(1);
    }

    /// Reads the current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Signed saturating process gauge.
#[derive(Debug, Default)]
pub struct Gauge(AtomicI64);

impl Gauge {
    /// Sets the exact current value.
    pub fn set(&self, value: i64) {
        self.0.store(value, Ordering::Relaxed);
    }

    /// Adds a signed delta without wrapping.
    pub fn add(&self, delta: i64) {
        let mut current = self.0.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(delta);
            match self
                .0
                .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    /// Reads the current value.
    #[must_use]
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Fixed microsecond buckets with sum and count.
#[derive(Debug)]
pub struct DurationHistogram {
    upper_bounds_micros: &'static [u64],
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum_micros: AtomicU64,
}

impl DurationHistogram {
    /// Creates cumulative buckets. Bounds must be strictly increasing.
    #[must_use]
    pub fn new(upper_bounds_micros: &'static [u64]) -> Self {
        debug_assert!(
            upper_bounds_micros
                .windows(2)
                .all(|window| window[0] < window[1])
        );
        Self {
            upper_bounds_micros,
            buckets: (0..upper_bounds_micros.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }

    /// Records one duration in microseconds.
    pub fn observe_micros(&self, micros: u64) {
        for (bound, bucket) in self.upper_bounds_micros.iter().zip(&self.buckets) {
            if micros <= *bound {
                saturating_add_u64(bucket, 1);
            }
        }
        saturating_add_u64(&self.count, 1);
        saturating_add_u64(&self.sum_micros, micros);
    }

    fn render(&self, output: &mut String, name: &str, help: &str) {
        metric_header(output, name, help, "histogram");
        for (bound, bucket) in self.upper_bounds_micros.iter().zip(&self.buckets) {
            let _ = writeln!(
                output,
                "{name}_bucket{{le=\"{bound}\"}} {}",
                bucket.load(Ordering::Relaxed)
            );
        }
        let _ = writeln!(
            output,
            "{name}_bucket{{le=\"+Inf\"}} {}",
            self.count.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            output,
            "{name}_sum {}",
            self.sum_micros.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            output,
            "{name}_count {}",
            self.count.load(Ordering::Relaxed)
        );
    }
}

const PACKET_LATENCY_BUCKETS: &[u64] = &[50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000];
const CONTROL_LATENCY_BUCKETS: &[u64] = &[
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000,
];

/// Complete low-cardinality process metrics for one media node.
#[derive(Debug)]
pub struct MediaNodeMetrics {
    /// Current rooms assigned to this node.
    pub active_rooms: Gauge,
    /// Current WebRTC sessions.
    pub active_sessions: Gauge,
    /// Current publisher tracks.
    pub publisher_tracks: Gauge,
    /// Current subscriber down-tracks.
    pub subscriber_tracks: Gauge,
    /// Current transcoder jobs.
    pub transcoder_jobs: Gauge,
    /// Current authenticated SCTP associations.
    pub active_data_channel_associations: Gauge,
    /// Current negotiated WebRTC data channels.
    pub active_data_channels: Gauge,
    /// Authenticated ingress RTP packets.
    pub rtp_packets_received: Counter,
    /// Protected egress RTP packets.
    pub rtp_packets_sent: Counter,
    /// Ingress media bytes.
    pub media_bytes_received: Counter,
    /// Egress media bytes.
    pub media_bytes_sent: Counter,
    /// Packets dropped by parsing, policy, queue, or adaptation.
    pub packets_dropped: Counter,
    /// STUN/SRTP/DTLS authentication failures.
    pub authentication_failures: Counter,
    /// Generic NACK requests.
    pub nack_requests: Counter,
    /// PLI requests emitted upstream.
    pub pli_requests: Counter,
    /// Committed adaptive layer changes.
    pub layer_switches: Counter,
    /// HLS/DASH/CMAF media segments successfully persisted.
    pub segments_written: Counter,
    /// Accepted WebRTC data-channel messages.
    pub data_channel_messages_received: Counter,
    /// WebRTC data-channel messages queued to room recipients.
    pub data_channel_messages_sent: Counter,
    /// Rejected malformed or unauthorized data-channel packets/messages.
    pub data_channel_rejections: Counter,
    /// Reliable SCTP DATA chunks retransmitted.
    pub data_channel_retransmissions: Counter,
    /// SCTP associations terminated after exhausting retransmissions.
    pub data_channel_delivery_failures: Counter,
    /// Partially reliable SCTP messages abandoned by negotiated policy.
    pub data_channel_messages_abandoned: Counter,
    /// Media packet processing latency.
    pub packet_processing_micros: DurationHistogram,
    /// Signaling/control command latency.
    pub control_processing_micros: DurationHistogram,
}

impl Default for MediaNodeMetrics {
    fn default() -> Self {
        Self {
            active_rooms: Gauge::default(),
            active_sessions: Gauge::default(),
            publisher_tracks: Gauge::default(),
            subscriber_tracks: Gauge::default(),
            transcoder_jobs: Gauge::default(),
            active_data_channel_associations: Gauge::default(),
            active_data_channels: Gauge::default(),
            rtp_packets_received: Counter::default(),
            rtp_packets_sent: Counter::default(),
            media_bytes_received: Counter::default(),
            media_bytes_sent: Counter::default(),
            packets_dropped: Counter::default(),
            authentication_failures: Counter::default(),
            nack_requests: Counter::default(),
            pli_requests: Counter::default(),
            layer_switches: Counter::default(),
            segments_written: Counter::default(),
            data_channel_messages_received: Counter::default(),
            data_channel_messages_sent: Counter::default(),
            data_channel_rejections: Counter::default(),
            data_channel_retransmissions: Counter::default(),
            data_channel_delivery_failures: Counter::default(),
            data_channel_messages_abandoned: Counter::default(),
            packet_processing_micros: DurationHistogram::new(PACKET_LATENCY_BUCKETS),
            control_processing_micros: DurationHistogram::new(CONTROL_LATENCY_BUCKETS),
        }
    }
}

impl MediaNodeMetrics {
    /// Renders Prometheus text exposition without user, room, track, or SSRC labels.
    #[must_use]
    pub fn render_prometheus(&self) -> String {
        let mut output = String::with_capacity(4_096);
        self.render_gauges(&mut output);
        self.render_counters(&mut output);
        self.packet_processing_micros.render(
            &mut output,
            "fluvora_packet_processing_micros",
            "Media packet processing latency in microseconds.",
        );
        self.control_processing_micros.render(
            &mut output,
            "fluvora_control_processing_micros",
            "Control command latency in microseconds.",
        );
        output
    }

    fn render_gauges(&self, output: &mut String) {
        let metrics = [
            (
                "fluvora_active_rooms",
                "Rooms assigned to this media node.",
                self.active_rooms.get(),
            ),
            (
                "fluvora_active_sessions",
                "Active WebRTC sessions.",
                self.active_sessions.get(),
            ),
            (
                "fluvora_publisher_tracks",
                "Active publisher tracks.",
                self.publisher_tracks.get(),
            ),
            (
                "fluvora_subscriber_tracks",
                "Active subscriber down-tracks.",
                self.subscriber_tracks.get(),
            ),
            (
                "fluvora_transcoder_jobs",
                "Active media transcoder jobs.",
                self.transcoder_jobs.get(),
            ),
            (
                "fluvora_active_data_channel_associations",
                "Active authenticated SCTP associations.",
                self.active_data_channel_associations.get(),
            ),
            (
                "fluvora_active_data_channels",
                "Active negotiated WebRTC data channels.",
                self.active_data_channels.get(),
            ),
        ];
        for (name, help, value) in metrics {
            render_gauge(output, name, help, value);
        }
    }

    fn render_counters(&self, output: &mut String) {
        let metrics = [
            (
                "fluvora_rtp_packets_received_total",
                "Authenticated ingress RTP packets.",
                self.rtp_packets_received.get(),
            ),
            (
                "fluvora_rtp_packets_sent_total",
                "Protected egress RTP packets.",
                self.rtp_packets_sent.get(),
            ),
            (
                "fluvora_media_bytes_received_total",
                "Ingress media bytes.",
                self.media_bytes_received.get(),
            ),
            (
                "fluvora_media_bytes_sent_total",
                "Egress media bytes.",
                self.media_bytes_sent.get(),
            ),
            (
                "fluvora_packets_dropped_total",
                "Packets dropped before forwarding.",
                self.packets_dropped.get(),
            ),
            (
                "fluvora_authentication_failures_total",
                "Transport authentication failures.",
                self.authentication_failures.get(),
            ),
            (
                "fluvora_nack_requests_total",
                "Generic NACK requests processed.",
                self.nack_requests.get(),
            ),
            (
                "fluvora_pli_requests_total",
                "Picture-loss indications emitted.",
                self.pli_requests.get(),
            ),
            (
                "fluvora_layer_switches_total",
                "Committed adaptive layer switches.",
                self.layer_switches.get(),
            ),
            (
                "fluvora_segments_written_total",
                "Persisted media segments.",
                self.segments_written.get(),
            ),
            (
                "fluvora_data_channel_messages_received_total",
                "Accepted WebRTC data-channel messages.",
                self.data_channel_messages_received.get(),
            ),
            (
                "fluvora_data_channel_messages_sent_total",
                "WebRTC data-channel messages queued to room recipients.",
                self.data_channel_messages_sent.get(),
            ),
            (
                "fluvora_data_channel_rejections_total",
                "Rejected data-channel packets or messages.",
                self.data_channel_rejections.get(),
            ),
            (
                "fluvora_data_channel_retransmissions_total",
                "Retransmitted reliable SCTP DATA chunks.",
                self.data_channel_retransmissions.get(),
            ),
            (
                "fluvora_data_channel_delivery_failures_total",
                "SCTP associations terminated after retransmission exhaustion.",
                self.data_channel_delivery_failures.get(),
            ),
            (
                "fluvora_data_channel_messages_abandoned_total",
                "Partially reliable SCTP messages abandoned by negotiated policy.",
                self.data_channel_messages_abandoned.get(),
            ),
        ];
        for (name, help, value) in metrics {
            render_counter(output, name, help, value);
        }
    }
}

/// Health of one node dependency or internal subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    /// Component is functioning within policy.
    Healthy,
    /// Component works with reduced capacity or elevated error rate.
    Degraded,
    /// Component is unavailable.
    Unhealthy,
}

/// Point-in-time component observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentHealth {
    /// Health classification.
    pub state: ComponentState,
    /// Stable operator-readable detail without secrets.
    pub detail: String,
    /// Monotonic or Unix timestamp supplied by the runtime.
    pub observed_at_millis: u64,
}

/// Aggregate readiness snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// `true` only when no component is unhealthy and the node is not draining.
    pub ready: bool,
    /// Node has stopped accepting new rooms.
    pub draining: bool,
    /// Stable component names and observations.
    pub components: BTreeMap<String, ComponentHealth>,
}

/// Concurrent component health registry used by readiness and status APIs.
#[derive(Debug, Default)]
pub struct HealthRegistry {
    components: RwLock<BTreeMap<String, ComponentHealth>>,
    draining: RwLock<bool>,
}

impl HealthRegistry {
    /// Updates one stable low-cardinality component name.
    pub fn set_component(
        &self,
        name: impl Into<String>,
        state: ComponentState,
        detail: impl Into<String>,
        observed_at_millis: u64,
    ) {
        let mut components = self
            .components
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        components.insert(
            name.into(),
            ComponentHealth {
                state,
                detail: detail.into(),
                observed_at_millis,
            },
        );
    }

    /// Enables or disables node draining.
    pub fn set_draining(&self, draining: bool) {
        *self
            .draining
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = draining;
    }

    /// Captures an internally consistent readiness response.
    #[must_use]
    pub fn snapshot(&self) -> HealthSnapshot {
        let components = self
            .components
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let draining = *self
            .draining
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready = !draining
            && !components.is_empty()
            && components
                .values()
                .all(|component| component.state != ComponentState::Unhealthy);
        HealthSnapshot {
            ready,
            draining,
            components,
        }
    }
}

fn saturating_add_u64(value: &AtomicU64, delta: u64) {
    let mut current = value.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(delta);
        match value.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn metric_header(output: &mut String, name: &str, help: &str, metric_type: &str) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} {metric_type}");
}

fn render_counter(output: &mut String, name: &str, help: &str, value: u64) {
    metric_header(output, name, help, "counter");
    let _ = writeln!(output, "{name} {value}");
}

fn render_gauge(output: &mut String, name: &str, help: &str, value: i64) {
    metric_header(output, name, help, "gauge");
    let _ = writeln!(output, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::{ComponentState, HealthRegistry, MediaNodeMetrics};

    #[test]
    fn renders_stable_prometheus_metrics() {
        let metrics = MediaNodeMetrics::default();
        metrics.active_sessions.set(7);
        metrics.rtp_packets_received.add(12);
        metrics.data_channel_messages_abandoned.increment();
        metrics.packet_processing_micros.observe_micros(90);
        let rendered = metrics.render_prometheus();

        assert!(rendered.contains("fluvora_active_sessions 7"));
        assert!(rendered.contains("fluvora_rtp_packets_received_total 12"));
        assert!(rendered.contains("fluvora_data_channel_messages_abandoned_total 1"));
        assert!(rendered.contains("fluvora_packet_processing_micros_bucket{le=\"100\"} 1"));
        assert!(!rendered.contains("room_id"));
        assert!(!rendered.contains("user_id"));
    }

    #[test]
    fn derives_readiness_from_components_and_drain_state() {
        let health = HealthRegistry::default();
        assert!(!health.snapshot().ready);
        health.set_component("udp", ComponentState::Healthy, "bound", 1);
        health.set_component("storage", ComponentState::Degraded, "retrying", 1);
        assert!(health.snapshot().ready);
        health.set_component("storage", ComponentState::Unhealthy, "unreachable", 2);
        assert!(!health.snapshot().ready);
        health.set_component("storage", ComponentState::Healthy, "connected", 3);
        health.set_draining(true);
        let snapshot = health.snapshot();
        assert!(!snapshot.ready);
        assert!(snapshot.draining);
    }
}
