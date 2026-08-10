//! Authenticated, failure-tolerant service heartbeat client.

use std::env;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use fluvora_status_service::{NodeCapacity, NodeHeartbeatInput, ServiceKind};

const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// Status-service client configured from the common Fluvora environment.
#[derive(Debug, Clone)]
pub struct HeartbeatClient {
    http: reqwest::Client,
    endpoint: Arc<str>,
    token: Arc<str>,
    node_id: Arc<str>,
    region: Arc<str>,
    version: Arc<str>,
    control_endpoint: Option<Arc<str>>,
    media_candidate: Option<Arc<str>>,
    service: ServiceKind,
    draining: Arc<AtomicBool>,
    successful_reports: Arc<AtomicU64>,
    failed_reports: Arc<AtomicU64>,
}

impl HeartbeatClient {
    /// Creates a client when `FLUVORA_STATUS_URL` is configured.
    ///
    /// # Errors
    ///
    /// Returns an error for incomplete or invalid heartbeat configuration.
    pub fn from_env(service: ServiceKind) -> Result<Option<Self>, ConfigError> {
        let Ok(base_url) = env::var("FLUVORA_STATUS_URL") else {
            return Ok(None);
        };
        let allow_insecure = env::var("FLUVORA_STATUS_ALLOW_INSECURE")
            .is_ok_and(|value| value.eq_ignore_ascii_case("true"));
        let base_url = normalize_status_origin(&base_url, allow_insecure)?;
        let token = env::var("FLUVORA_STATUS_TOKEN").map_err(|_| ConfigError::MissingToken)?;
        if !(16..=4_096).contains(&token.len()) || token.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ConfigError::WeakToken);
        }
        let node_id = env::var("FLUVORA_NODE_ID")
            .unwrap_or_else(|_| default_node_id(service))
            .to_ascii_lowercase();
        if !valid_identifier(&node_id, 128) {
            return Err(ConfigError::InvalidNodeId);
        }
        let region = env::var("FLUVORA_REGION").unwrap_or_else(|_| "local".to_owned());
        if !valid_identifier(&region, 64) {
            return Err(ConfigError::InvalidRegion);
        }
        let version = env::var("FLUVORA_BUILD_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
        if version.is_empty()
            || version.len() > 128
            || version.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ConfigError::InvalidVersion);
        }
        let control_endpoint = env::var("FLUVORA_CONTROL_ADVERTISE_URL")
            .ok()
            .map(|endpoint| {
                normalize_http_origin(&endpoint)
                    .map(Arc::<str>::from)
                    .map_err(|()| ConfigError::InvalidControlEndpoint)
            })
            .transpose()?;
        let media_candidate = env::var("FLUVORA_MEDIA_ADVERTISE_CANDIDATE")
            .ok()
            .map(|candidate| {
                if !candidate.is_empty()
                    && candidate.len() <= 2_048
                    && !candidate.bytes().any(|byte| byte.is_ascii_control())
                {
                    Ok(Arc::<str>::from(candidate))
                } else {
                    Err(ConfigError::InvalidMediaCandidate)
                }
            })
            .transpose()?;
        Ok(Some(Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(3))
                .build()
                .map_err(|_| ConfigError::HttpClient)?,
            endpoint: Arc::from(format!("{base_url}/v1/nodes/{node_id}/heartbeat")),
            token: Arc::from(token),
            node_id: Arc::from(node_id),
            region: Arc::from(region),
            version: Arc::from(version),
            control_endpoint,
            media_candidate,
            service,
            draining: Arc::new(AtomicBool::new(false)),
            successful_reports: Arc::new(AtomicU64::new(0)),
            failed_reports: Arc::new(AtomicU64::new(0)),
        }))
    }

    /// Returns the stable instance identifier.
    #[must_use]
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Marks subsequent reports as draining.
    pub fn mark_draining(&self) {
        self.draining.store(true, Ordering::Release);
    }

    /// Sends one heartbeat.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be delivered or is rejected.
    pub async fn report(&self, capacity: NodeCapacity, healthy: bool) -> Result<(), ReportError> {
        let response = self
            .http
            .post(self.endpoint.as_ref())
            .bearer_auth(self.token.as_ref())
            .json(&NodeHeartbeatInput {
                service: self.service,
                region: self.region.to_string(),
                version: self.version.to_string(),
                control_endpoint: self.control_endpoint.as_deref().map(str::to_owned),
                media_candidate: self.media_candidate.as_deref().map(str::to_owned),
                healthy,
                draining: self.draining.load(Ordering::Acquire),
                capacity,
            })
            .send()
            .await
            .map_err(|_| ReportError::Unavailable)?;
        if response.status().is_success() {
            self.successful_reports.fetch_add(1, Ordering::Relaxed);
            Ok(())
        } else {
            self.failed_reports.fetch_add(1, Ordering::Relaxed);
            Err(ReportError::Rejected(response.status().as_u16()))
        }
    }

    /// Runs periodic heartbeats until the task is cancelled.
    pub async fn run<Sample, SampleFuture>(&self, sample: Sample)
    where
        Sample: Fn() -> SampleFuture,
        SampleFuture: Future<Output = NodeCapacity>,
    {
        let mut interval = tokio::time::interval(DEFAULT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut previously_failed = false;
        loop {
            interval.tick().await;
            match self.report(sample().await, true).await {
                Ok(()) if previously_failed => {
                    eprintln!("status heartbeat restored for {}", self.node_id);
                    previously_failed = false;
                }
                Err(error) if !previously_failed => {
                    eprintln!("status heartbeat failed for {}: {error}", self.node_id);
                    previously_failed = true;
                }
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Total successful heartbeat requests.
    #[must_use]
    pub fn successful_reports(&self) -> u64 {
        self.successful_reports.load(Ordering::Relaxed)
    }

    /// Total failed heartbeat requests.
    #[must_use]
    pub fn failed_reports(&self) -> u64 {
        self.failed_reports.load(Ordering::Relaxed)
    }
}

fn normalize_status_origin(value: &str, allow_insecure: bool) -> Result<String, ConfigError> {
    let normalized = normalize_http_origin(value).map_err(|()| ConfigError::InvalidStatusUrl)?;
    let parsed = reqwest::Url::parse(&normalized).map_err(|_| ConfigError::InvalidStatusUrl)?;
    if parsed.scheme() == "http" && !allow_insecure {
        let host = parsed
            .host_str()
            .ok_or(ConfigError::InvalidStatusUrl)?
            .trim_matches(['[', ']']);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !loopback {
            return Err(ConfigError::InsecureStatusUrl);
        }
    }
    Ok(normalized)
}

fn normalize_http_origin(value: &str) -> Result<String, ()> {
    if value.is_empty() || value.len() > 2_048 || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(());
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| ())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(());
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

fn default_node_id(service: ServiceKind) -> String {
    let host = env::var("HOSTNAME")
        .or_else(|_| env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "local".to_owned())
        .to_ascii_lowercase();
    let filtered = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .take(80)
        .collect::<String>();
    format!("{}-{filtered}", service.as_str())
}

fn valid_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Invalid common heartbeat configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Status URL is not a bounded credential-free HTTP(S) origin.
    InvalidStatusUrl,
    /// Plain HTTP is allowed only on loopback or an explicitly trusted isolated network.
    InsecureStatusUrl,
    /// Status token was not provided.
    MissingToken,
    /// Status token is too short.
    WeakToken,
    /// Node identifier is not a bounded URL-safe identifier.
    InvalidNodeId,
    /// Region is not a bounded identifier.
    InvalidRegion,
    /// Build version is empty or too long.
    InvalidVersion,
    /// Advertised control endpoint is not a bounded HTTP(S) URL.
    InvalidControlEndpoint,
    /// Advertised media candidate is empty or oversized.
    InvalidMediaCandidate,
    /// HTTP client initialization failed.
    HttpClient,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidStatusUrl => formatter.write_str(
                "FLUVORA_STATUS_URL must be a valid HTTP(S) origin without credentials or paths",
            ),
            Self::InsecureStatusUrl => formatter.write_str(
                "FLUVORA_STATUS_URL must use HTTPS or explicitly enable isolated-network HTTP",
            ),
            Self::MissingToken => formatter.write_str("FLUVORA_STATUS_TOKEN is required"),
            Self::WeakToken => {
                formatter.write_str("FLUVORA_STATUS_TOKEN must contain 16..=4096 non-control bytes")
            }
            Self::InvalidNodeId => formatter.write_str("FLUVORA_NODE_ID is invalid"),
            Self::InvalidRegion => formatter.write_str("FLUVORA_REGION is invalid"),
            Self::InvalidVersion => formatter.write_str("FLUVORA_BUILD_VERSION is invalid"),
            Self::InvalidControlEndpoint => {
                formatter.write_str("FLUVORA_CONTROL_ADVERTISE_URL is invalid")
            }
            Self::InvalidMediaCandidate => {
                formatter.write_str("FLUVORA_MEDIA_ADVERTISE_CANDIDATE is invalid")
            }
            Self::HttpClient => formatter.write_str("failed to initialize heartbeat HTTP client"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Heartbeat delivery error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportError {
    /// Status service could not be reached.
    Unavailable,
    /// Status service rejected the heartbeat.
    Rejected(u16),
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("status service unavailable"),
            Self::Rejected(status) => write!(formatter, "status service returned HTTP {status}"),
        }
    }
}

impl std::error::Error for ReportError {}

/// Returns resident process memory on Linux, or zero when the platform does not expose it through
/// a safe standard interface.
#[must_use]
pub fn process_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return 0;
        };
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|kilobytes| kilobytes.checked_mul(1_024))
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, normalize_http_origin, normalize_status_origin, valid_identifier};

    #[test]
    fn accepts_only_url_safe_bounded_identifiers() {
        assert!(valid_identifier("media-node_01.cn", 64));
        assert!(!valid_identifier("", 64));
        assert!(!valid_identifier("node/01", 64));
        assert!(!valid_identifier(&"x".repeat(65), 64));
    }

    #[test]
    fn validates_status_and_advertised_control_origins() {
        assert_eq!(
            normalize_status_origin("http://127.0.0.1:8090/", false).expect("loopback"),
            "http://127.0.0.1:8090"
        );
        assert_eq!(
            normalize_status_origin("http://[::1]:8090", false).expect("IPv6 loopback"),
            "http://[::1]:8090"
        );
        assert_eq!(
            normalize_status_origin("http://127.0.0.1.attacker.example", false),
            Err(ConfigError::InsecureStatusUrl)
        );
        assert_eq!(
            normalize_status_origin("http://status.internal", false),
            Err(ConfigError::InsecureStatusUrl)
        );
        assert!(normalize_status_origin("http://status.internal", true).is_ok());
        assert!(normalize_status_origin("https://token@status.example", false).is_err());
        assert!(normalize_status_origin("https://status.example/base", false).is_err());
        assert!(normalize_http_origin("https://node.example:8092/").is_ok());
        assert!(normalize_http_origin("https://node.example/path").is_err());
    }
}
