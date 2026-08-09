//! Media-node heartbeat registry and aggregate platform status.

use std::collections::BTreeMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Stable component role reported by a Fluvora process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// Public control/signaling API.
    Api,
    /// WebRTC media/SFU node.
    MediaNode,
    /// FFmpeg-based bounded media worker.
    MediaWorker,
    /// VOD/live origin gateway.
    MediaGateway,
    /// STUN/TURN relay.
    Turn,
    /// Transactional-outbox to `JetStream` publisher.
    EventDispatcher,
}

impl ServiceKind {
    /// Stable label used by metrics and generated instance identifiers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::MediaNode => "media_node",
            Self::MediaWorker => "media_worker",
            Self::MediaGateway => "media_gateway",
            Self::Turn => "turn",
            Self::EventDispatcher => "event_dispatcher",
        }
    }
}

const fn default_service_kind() -> ServiceKind {
    ServiceKind::MediaNode
}

/// Node capacity and instantaneous allocation reported by a heartbeat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapacity {
    /// Maximum configured rooms.
    pub rooms_limit: u64,
    /// Active rooms.
    pub rooms_used: u64,
    /// Maximum configured RTC sessions.
    pub sessions_limit: u64,
    /// Active RTC sessions.
    pub sessions_used: u64,
    /// Current publisher tracks.
    pub publisher_tracks: u64,
    /// Current subscriber tracks.
    pub subscriber_tracks: u64,
    /// Process CPU in per-mille units.
    pub cpu_per_mille: u16,
    /// Process resident bytes.
    pub memory_bytes: u64,
    /// Maximum concurrent media jobs.
    #[serde(default)]
    pub jobs_limit: u64,
    /// Running or queued media jobs.
    #[serde(default)]
    pub jobs_used: u64,
    /// Retained VOD assets.
    #[serde(default)]
    pub assets: u64,
    /// Active live streams.
    #[serde(default)]
    pub live_streams: u64,
    /// Active TURN allocations.
    #[serde(default)]
    pub turn_allocations: u64,
}

/// Heartbeat body accepted from an authenticated media node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHeartbeatInput {
    /// Component role.
    #[serde(default = "default_service_kind")]
    pub service: ServiceKind,
    /// Deployment region or failure domain.
    pub region: String,
    /// Build version or commit.
    pub version: String,
    /// Internal service control endpoint used for durable discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_endpoint: Option<String>,
    /// Node-specific SDP ICE candidate used for room placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_candidate: Option<String>,
    /// Whether local critical components are healthy.
    pub healthy: bool,
    /// Whether the node is draining.
    pub draining: bool,
    /// Capacity and current use.
    pub capacity: NodeCapacity,
}

/// Stored node status with server-assigned observation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatus {
    /// Stable node identifier.
    pub node_id: String,
    /// Component role.
    pub service: ServiceKind,
    /// Deployment region.
    pub region: String,
    /// Build version.
    pub version: String,
    /// Internal service control endpoint.
    pub control_endpoint: Option<String>,
    /// Node-specific SDP ICE candidate.
    pub media_candidate: Option<String>,
    /// Local health.
    pub healthy: bool,
    /// Draining state.
    pub draining: bool,
    /// Capacity and current use.
    pub capacity: NodeCapacity,
    /// Status-service Unix observation timestamp.
    pub observed_at_millis: u64,
}

/// Aggregated active-node capacity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateCapacity {
    /// Total room limit.
    pub rooms_limit: u64,
    /// Total active rooms.
    pub rooms_used: u64,
    /// Total session limit.
    pub sessions_limit: u64,
    /// Total active sessions.
    pub sessions_used: u64,
    /// Total publisher tracks.
    pub publisher_tracks: u64,
    /// Total subscriber tracks.
    pub subscriber_tracks: u64,
    /// Total media job capacity.
    pub jobs_limit: u64,
    /// Total running or queued media jobs.
    pub jobs_used: u64,
    /// Total retained VOD assets.
    pub assets: u64,
    /// Total active live streams.
    pub live_streams: u64,
    /// Total active TURN allocations.
    pub turn_allocations: u64,
}

/// Counts for one component role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSummary {
    /// Non-expired instances.
    pub total: u64,
    /// Healthy, non-draining instances.
    pub available: u64,
    /// Draining instances.
    pub draining: u64,
}

/// Public status response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformStatus {
    /// `true` when at least one non-draining healthy node is active.
    pub available: bool,
    /// Active nodes keyed by stable identifier.
    pub nodes: BTreeMap<String, NodeStatus>,
    /// Instance counts grouped by component role.
    pub services: BTreeMap<String, ServiceSummary>,
    /// Saturating capacity sum.
    pub capacity: AggregateCapacity,
    /// Snapshot timestamp.
    pub generated_at_millis: u64,
}

/// Concurrent heartbeat registry with deterministic expiry.
#[derive(Debug)]
pub struct StatusRegistry {
    nodes: RwLock<BTreeMap<String, NodeStatus>>,
    heartbeat_ttl_millis: u64,
}

impl StatusRegistry {
    /// Creates a registry with a heartbeat time-to-live.
    #[must_use]
    pub const fn new(heartbeat_ttl_millis: u64) -> Self {
        Self {
            nodes: RwLock::new(BTreeMap::new()),
            heartbeat_ttl_millis,
        }
    }

    /// Validates and records one heartbeat.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for invalid identifiers, regions, versions, or impossible used
    /// capacity values.
    pub fn upsert(
        &self,
        node_id: String,
        input: NodeHeartbeatInput,
        observed_at_millis: u64,
    ) -> Result<(), RegistryError> {
        validate_heartbeat(&node_id, &input)?;
        let status = NodeStatus {
            node_id: node_id.clone(),
            service: input.service,
            region: input.region,
            version: input.version,
            control_endpoint: input.control_endpoint,
            media_candidate: input.media_candidate,
            healthy: input.healthy,
            draining: input.draining,
            capacity: input.capacity,
            observed_at_millis,
        };
        self.nodes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(node_id, status);
        Ok(())
    }

    /// Removes expired nodes and returns an aggregate snapshot.
    #[must_use]
    pub fn snapshot(&self, now_millis: u64) -> PlatformStatus {
        let mut nodes = self
            .nodes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nodes.retain(|_, node| {
            now_millis.saturating_sub(node.observed_at_millis) <= self.heartbeat_ttl_millis
        });
        let nodes = nodes.clone();
        let capacity = nodes
            .values()
            .fold(AggregateCapacity::default(), |mut aggregate, node| {
                aggregate.rooms_limit = aggregate
                    .rooms_limit
                    .saturating_add(node.capacity.rooms_limit);
                aggregate.rooms_used = aggregate
                    .rooms_used
                    .saturating_add(node.capacity.rooms_used);
                aggregate.sessions_limit = aggregate
                    .sessions_limit
                    .saturating_add(node.capacity.sessions_limit);
                aggregate.sessions_used = aggregate
                    .sessions_used
                    .saturating_add(node.capacity.sessions_used);
                aggregate.publisher_tracks = aggregate
                    .publisher_tracks
                    .saturating_add(node.capacity.publisher_tracks);
                aggregate.subscriber_tracks = aggregate
                    .subscriber_tracks
                    .saturating_add(node.capacity.subscriber_tracks);
                aggregate.jobs_limit = aggregate
                    .jobs_limit
                    .saturating_add(node.capacity.jobs_limit);
                aggregate.jobs_used = aggregate.jobs_used.saturating_add(node.capacity.jobs_used);
                aggregate.assets = aggregate.assets.saturating_add(node.capacity.assets);
                aggregate.live_streams = aggregate
                    .live_streams
                    .saturating_add(node.capacity.live_streams);
                aggregate.turn_allocations = aggregate
                    .turn_allocations
                    .saturating_add(node.capacity.turn_allocations);
                aggregate
            });
        let mut services = BTreeMap::<String, ServiceSummary>::new();
        for node in nodes.values() {
            let summary = services
                .entry(node.service.as_str().to_owned())
                .or_default();
            summary.total = summary.total.saturating_add(1);
            if node.healthy && !node.draining {
                summary.available = summary.available.saturating_add(1);
            }
            if node.draining {
                summary.draining = summary.draining.saturating_add(1);
            }
        }
        let available = nodes
            .values()
            .any(|node| node.service == ServiceKind::MediaNode && node.healthy && !node.draining);
        PlatformStatus {
            available,
            nodes,
            services,
            capacity,
            generated_at_millis: now_millis,
        }
    }
}

fn validate_heartbeat(node_id: &str, input: &NodeHeartbeatInput) -> Result<(), RegistryError> {
    if !valid_identifier(node_id, 128) {
        return Err(RegistryError::InvalidNodeId);
    }
    if !valid_identifier(&input.region, 64) {
        return Err(RegistryError::InvalidRegion);
    }
    if input.version.is_empty()
        || input.version.len() > 128
        || input.version.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RegistryError::InvalidVersion);
    }
    if input
        .control_endpoint
        .as_ref()
        .is_some_and(|endpoint| !valid_http_origin(endpoint))
    {
        return Err(RegistryError::InvalidControlEndpoint);
    }
    if input.media_candidate.as_ref().is_some_and(|candidate| {
        candidate.is_empty()
            || candidate.len() > 2_048
            || candidate.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err(RegistryError::InvalidMediaCandidate);
    }
    if input.capacity.rooms_used > input.capacity.rooms_limit
        || input.capacity.sessions_used > input.capacity.sessions_limit
        || input.capacity.jobs_used > input.capacity.jobs_limit
        || input.capacity.cpu_per_mille > 1_000
    {
        return Err(RegistryError::InvalidCapacity);
    }
    Ok(())
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_http_origin(value: &str) -> bool {
    if value.is_empty() || value.len() > 2_048 || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

/// Heartbeat validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// Node identifier is empty or too long.
    InvalidNodeId,
    /// Region is empty or too long.
    InvalidRegion,
    /// Version is empty or too long.
    InvalidVersion,
    /// Internal control endpoint is not a bounded HTTP(S) URL.
    InvalidControlEndpoint,
    /// Media candidate is empty or oversized.
    InvalidMediaCandidate,
    /// Used capacity exceeds limit or CPU exceeds 1000 per-mille.
    InvalidCapacity,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNodeId => formatter.write_str("invalid node id"),
            Self::InvalidRegion => formatter.write_str("invalid node region"),
            Self::InvalidVersion => formatter.write_str("invalid node version"),
            Self::InvalidControlEndpoint => formatter.write_str("invalid node control endpoint"),
            Self::InvalidMediaCandidate => formatter.write_str("invalid node media candidate"),
            Self::InvalidCapacity => formatter.write_str("invalid node capacity"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::{NodeCapacity, NodeHeartbeatInput, ServiceKind, StatusRegistry};

    fn heartbeat(healthy: bool, draining: bool) -> NodeHeartbeatInput {
        NodeHeartbeatInput {
            service: ServiceKind::MediaNode,
            region: "cn-east".to_owned(),
            version: "0.1.0".to_owned(),
            control_endpoint: Some("http://media-node:8092".to_owned()),
            media_candidate: Some("1 1 UDP 2130706431 203.0.113.10 50000 typ host".to_owned()),
            healthy,
            draining,
            capacity: NodeCapacity {
                rooms_limit: 100,
                rooms_used: 10,
                sessions_limit: 1_000,
                sessions_used: 50,
                publisher_tracks: 20,
                subscriber_tracks: 100,
                cpu_per_mille: 250,
                memory_bytes: 1_000_000,
                jobs_limit: 0,
                jobs_used: 0,
                assets: 0,
                live_streams: 0,
                turn_allocations: 0,
            },
        }
    }

    #[test]
    fn aggregates_and_expires_nodes() {
        let registry = StatusRegistry::new(10_000);
        registry
            .upsert("node-a".to_owned(), heartbeat(true, false), 1_000)
            .expect("valid heartbeat");
        registry
            .upsert("node-b".to_owned(), heartbeat(false, false), 2_000)
            .expect("valid heartbeat");
        let status = registry.snapshot(5_000);
        assert!(status.available);
        assert_eq!(status.nodes.len(), 2);
        assert_eq!(status.capacity.sessions_limit, 2_000);
        assert_eq!(status.services["media_node"].available, 1);

        let expired = registry.snapshot(11_001);
        assert!(!expired.available);
        assert_eq!(expired.nodes.len(), 1);
        let all_expired = registry.snapshot(12_002);
        assert!(all_expired.nodes.is_empty());
    }

    #[test]
    fn rejects_impossible_capacity() {
        let registry = StatusRegistry::new(10_000);
        let mut invalid = heartbeat(true, false);
        invalid.capacity.sessions_used = 2_000;
        assert!(registry.upsert("node-a".to_owned(), invalid, 1).is_err());
    }

    #[test]
    fn rejects_endpoint_and_identifier_injection() {
        let registry = StatusRegistry::new(10_000);
        for endpoint in [
            "http://token@media-node:8092",
            "http://media-node:8092/internal",
            "file:///tmp/socket",
            "http://media-node:8092?redirect=true",
        ] {
            let mut invalid = heartbeat(true, false);
            invalid.control_endpoint = Some(endpoint.to_owned());
            assert!(registry.upsert("node-a".to_owned(), invalid, 1).is_err());
        }
        assert!(
            registry
                .upsert("node/a".to_owned(), heartbeat(true, false), 1)
                .is_err()
        );
    }
}
