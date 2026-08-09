use std::collections::HashSet;
use std::env;
use std::time::Duration;

const DEFAULT_TRANSCODE_GLOBAL_JOBS: usize = 128;
const DEFAULT_TRANSCODE_TENANT_JOBS: usize = 16;
const DEFAULT_ICE_URLS: &str = "stun:127.0.0.1:3478,\
                                turn:127.0.0.1:3478?transport=udp,\
                                turn:127.0.0.1:3478?transport=tcp";

#[derive(Debug)]
pub(super) struct ApiConfig {
    pub(super) token_keys: Vec<Vec<u8>>,
    pub(super) media_control_token: String,
    pub(super) gateway_control_token: String,
    pub(super) worker_control_token: String,
    pub(super) turn_rest_secret: Vec<u8>,
    pub(super) gift_webhook_secret: Vec<u8>,
    pub(super) dtls_fingerprint: String,
    pub(super) region: String,
    pub(super) candidate: Option<String>,
    pub(super) media_control_url: String,
    pub(super) gateway_control_url: String,
    pub(super) worker_control_url: String,
    pub(super) ice_urls: Vec<String>,
    pub(super) placement_stale_after: Duration,
    pub(super) transcode_global_jobs: usize,
    pub(super) transcode_tenant_jobs: usize,
}

impl ApiConfig {
    pub(super) fn from_env() -> Self {
        let (transcode_global_jobs, transcode_tenant_jobs) = load_transcode_quotas();
        Self {
            token_keys: load_token_keys(),
            media_control_token: required_control_token(
                "FLUVORA_MEDIA_CONTROL_TOKEN",
                "FLUVORA_MEDIA_CONTROL_TOKEN must match media-node",
            ),
            gateway_control_token: required_control_token(
                "FLUVORA_GATEWAY_TOKEN",
                "FLUVORA_GATEWAY_TOKEN must match media-gateway",
            ),
            worker_control_token: required_control_token(
                "FLUVORA_WORKER_TOKEN",
                "FLUVORA_WORKER_TOKEN must match media-worker",
            ),
            turn_rest_secret: required_secret("FLUVORA_TURN_REST_SECRET"),
            gift_webhook_secret: required_secret("FLUVORA_GIFT_WEBHOOK_SECRET"),
            dtls_fingerprint: load_dtls_fingerprint(),
            region: env::var("FLUVORA_REGION").unwrap_or_else(|_| "local".to_owned()),
            candidate: env::var("FLUVORA_ICE_CANDIDATE").ok(),
            media_control_url: control_url("FLUVORA_MEDIA_CONTROL_URL", "http://127.0.0.1:8092"),
            gateway_control_url: control_url("FLUVORA_GATEWAY_URL", "http://127.0.0.1:8093"),
            worker_control_url: control_url("FLUVORA_WORKER_URL", "http://127.0.0.1:8091"),
            ice_urls: parse_ice_urls(
                &env::var("FLUVORA_ICE_URLS").unwrap_or_else(|_| DEFAULT_ICE_URLS.to_owned()),
            ),
            placement_stale_after: placement_stale_after(),
            transcode_global_jobs,
            transcode_tenant_jobs,
        }
    }
}

fn control_url(name: &str, default: &str) -> String {
    let value = env::var(name).unwrap_or_else(|_| default.to_owned());
    normalize_control_url(&value).unwrap_or_else(|error| panic!("{name} {error}"))
}

pub(super) fn normalize_control_url(value: &str) -> Result<String, &'static str> {
    if value.is_empty() || value.len() > 2_048 || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("must contain 1..=2048 non-control bytes");
    }
    let parsed = reqwest::Url::parse(value).map_err(|_| "must be a valid absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("must be an HTTP(S) origin without credentials, path, query, or fragment");
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

fn required_control_token(name: &str, missing_message: &str) -> String {
    let token = env::var(name).unwrap_or_else(|_| panic!("{missing_message}"));
    assert!(
        (16..=4_096).contains(&token.len()) && !token.bytes().any(|byte| byte.is_ascii_control()),
        "{name} must contain 16..=4096 non-control bytes"
    );
    token
}

fn required_secret(name: &str) -> Vec<u8> {
    let value = env::var(name)
        .unwrap_or_else(|_| panic!("{name} is required"))
        .into_bytes();
    assert!(
        (32..=4_096).contains(&value.len()) && !value.iter().any(u8::is_ascii_control),
        "{name} must contain 32..=4096 non-control bytes"
    );
    value
}

fn load_token_keys() -> Vec<Vec<u8>> {
    let values = env::var("FLUVORA_TOKEN_SECRETS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            env::var("FLUVORA_TOKEN_SECRET")
                .expect("FLUVORA_TOKEN_SECRETS or FLUVORA_TOKEN_SECRET is required")
        });
    let keys = values
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.as_bytes().to_vec())
        .collect::<Vec<_>>();
    assert!(
        !keys.is_empty()
            && keys.iter().all(|key| {
                (32..=4_096).contains(&key.len()) && !key.iter().any(u8::is_ascii_control)
            }),
        "FLUVORA_TOKEN_SECRETS must contain bounded strong keys"
    );
    assert!(
        keys.iter().collect::<HashSet<_>>().len() == keys.len(),
        "FLUVORA_TOKEN_SECRETS must not contain duplicate keys"
    );
    keys
}

fn load_transcode_quotas() -> (usize, usize) {
    let global = env::var("FLUVORA_TRANSCODE_GLOBAL_JOBS")
        .map_or(Ok(DEFAULT_TRANSCODE_GLOBAL_JOBS), |value| value.parse())
        .expect("FLUVORA_TRANSCODE_GLOBAL_JOBS must be an integer");
    let tenant = env::var("FLUVORA_TRANSCODE_TENANT_JOBS")
        .map_or(Ok(DEFAULT_TRANSCODE_TENANT_JOBS), |value| value.parse())
        .expect("FLUVORA_TRANSCODE_TENANT_JOBS must be an integer");
    assert!(
        (1..=10_000).contains(&global) && (1..=1_000).contains(&tenant) && tenant <= global,
        "transcode job quotas are invalid"
    );
    (global, tenant)
}

fn placement_stale_after() -> Duration {
    Duration::from_millis(
        env::var("FLUVORA_PLACEMENT_STALE_MILLIS")
            .map_or(Ok(15_000), |value| value.parse::<u64>())
            .expect("FLUVORA_PLACEMENT_STALE_MILLIS must be an integer")
            .clamp(1_000, 300_000),
    )
}

fn load_dtls_fingerprint() -> String {
    let fingerprint = env::var("FLUVORA_DTLS_FINGERPRINT").unwrap_or_else(|_| {
        let path = env::var("FLUVORA_DTLS_FINGERPRINT_FILE")
            .expect("FLUVORA_DTLS_FINGERPRINT or FLUVORA_DTLS_FINGERPRINT_FILE is required");
        std::fs::read_to_string(path)
            .expect("read FLUVORA_DTLS_FINGERPRINT_FILE")
            .trim()
            .to_owned()
    });
    fluvora_dtls_adapter::Sha256Fingerprint::parse("sha-256", &fingerprint)
        .expect("valid SHA-256 DTLS fingerprint");
    fingerprint
}

fn parse_ice_urls(value: &str) -> Vec<String> {
    let urls = value
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert!(
        !urls.is_empty()
            && urls.len() <= 8
            && urls.iter().all(|url| {
                url.len() <= 512
                    && url.bytes().all(|byte| byte.is_ascii_graphic())
                    && matches!(
                        url.split_once(':').map(|(scheme, _)| scheme),
                        Some("stun" | "stuns" | "turn" | "turns")
                    )
            })
            && urls
                .iter()
                .any(|url| url.starts_with("turn:") || url.starts_with("turns:")),
        "FLUVORA_ICE_URLS must contain 1..=8 valid STUN/TURN URLs and at least one TURN URL"
    );
    urls
}

#[cfg(test)]
mod tests {
    use super::{normalize_control_url, parse_ice_urls};

    #[test]
    fn accepts_bounded_stun_and_turn_urls() {
        let urls = parse_ice_urls("stun:stun.example,turn:relay.example?transport=udp");
        assert_eq!(urls.len(), 2);
    }

    #[test]
    #[should_panic(expected = "at least one TURN URL")]
    fn rejects_configuration_without_turn_fallback() {
        let _ = parse_ice_urls("stun:stun.example");
    }

    #[test]
    fn normalizes_bounded_control_origins() {
        assert_eq!(
            normalize_control_url("HTTPS://media.example:8443/").expect("control origin"),
            "https://media.example:8443"
        );
        assert!(normalize_control_url("ftp://media.example").is_err());
        assert!(normalize_control_url("http://token@media.example").is_err());
        assert!(normalize_control_url("http://media.example/internal").is_err());
        assert!(normalize_control_url("http://media.example?redirect=attacker").is_err());
    }
}
