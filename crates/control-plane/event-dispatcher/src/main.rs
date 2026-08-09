use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_nats::jetstream;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use fluvora_control_store::{OutboxMessage, PostgresStore};
use fluvora_event_dispatcher::{EventEnvelope, event_subject, retry_delay};
use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};

mod outbox_cleanup;

#[derive(Debug, Clone)]
struct Config {
    bind: SocketAddr,
    database_url: String,
    database_connections: u32,
    nats_url: String,
    nats_token: String,
    stream_name: String,
    subject_root: String,
    batch_size: u32,
    claim_ttl: Duration,
    poll_interval: Duration,
    outbox_retention: Duration,
    outbox_cleanup_batch: u32,
    instance_id: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        let bind = env::var("FLUVORA_EVENT_DISPATCHER_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8095".to_owned())
            .parse()
            .context("FLUVORA_EVENT_DISPATCHER_BIND must be host:port")?;
        let database_url =
            env::var("FLUVORA_DATABASE_URL").context("FLUVORA_DATABASE_URL is required")?;
        let database_connections = parse_bounded("FLUVORA_DATABASE_MAX_CONNECTIONS", 8, 1, 64)?;
        let nats_url =
            env::var("FLUVORA_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned());
        if !nats_url.starts_with("nats://") && !nats_url.starts_with("tls://") {
            return Err(anyhow!("FLUVORA_NATS_URL must use nats:// or tls://"));
        }
        let nats_token =
            env::var("FLUVORA_NATS_TOKEN").context("FLUVORA_NATS_TOKEN is required")?;
        if !(24..=4_096).contains(&nats_token.len())
            || nats_token.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(anyhow!(
                "FLUVORA_NATS_TOKEN must contain 24..=4096 non-control bytes"
            ));
        }
        let stream_name =
            env::var("FLUVORA_EVENT_STREAM").unwrap_or_else(|_| "FLUVORA_EVENTS".to_owned());
        validate_identifier("FLUVORA_EVENT_STREAM", &stream_name, 64)?;
        let subject_root =
            env::var("FLUVORA_EVENT_SUBJECT").unwrap_or_else(|_| "fluvora.events".to_owned());
        validate_subject_root(&subject_root)?;
        let batch_size = parse_bounded("FLUVORA_EVENT_BATCH_SIZE", 100, 1, 1_000)?;
        let claim_ttl = Duration::from_millis(parse_bounded(
            "FLUVORA_EVENT_CLAIM_TTL_MILLIS",
            30_000,
            1_000,
            300_000,
        )?);
        let poll_interval =
            Duration::from_millis(parse_bounded("FLUVORA_EVENT_POLL_MILLIS", 100, 10, 60_000)?);
        let outbox_retention = Duration::from_secs(
            parse_bounded("FLUVORA_OUTBOX_RETENTION_HOURS", 168, 1, 8_760)? * 3_600,
        );
        let outbox_cleanup_batch =
            parse_bounded("FLUVORA_OUTBOX_CLEANUP_BATCH", 10_000, 1, 10_000)?;
        let instance_id = env::var("FLUVORA_NODE_ID")
            .unwrap_or_else(|_| format!("event-dispatcher-{}", std::process::id()));
        validate_identifier("FLUVORA_NODE_ID", &instance_id, 128)?;
        Ok(Self {
            bind,
            database_url,
            database_connections,
            nats_url,
            nats_token,
            stream_name,
            subject_root,
            batch_size,
            claim_ttl,
            poll_interval,
            outbox_retention,
            outbox_cleanup_batch,
            instance_id,
        })
    }
}

#[derive(Debug, Default)]
struct Metrics {
    ready: AtomicBool,
    claimed: AtomicU64,
    published: AtomicU64,
    retried: AtomicU64,
    acknowledgement_conflicts: AtomicU64,
    pruned: AtomicU64,
    last_publish_millis: AtomicU64,
}

#[derive(Clone)]
struct AppState {
    store: PostgresStore,
    metrics: Arc<Metrics>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let store = PostgresStore::connect(&config.database_url, config.database_connections)
        .await
        .context("connect PostgreSQL")?;
    store
        .migrate()
        .await
        .context("apply PostgreSQL migrations")?;

    let client = async_nats::ConnectOptions::with_token(config.nats_token.clone())
        .name(config.instance_id.clone())
        .connect(&config.nats_url)
        .await
        .context("connect NATS")?;
    let context = jetstream::new(client);
    context
        .get_or_create_stream(jetstream::stream::Config {
            name: config.stream_name.clone(),
            subjects: vec![format!("{}.>", config.subject_root)],
            max_age: Duration::from_hours(168),
            duplicate_window: Duration::from_mins(2),
            ..Default::default()
        })
        .await
        .context("create or load JetStream event stream")?;

    let metrics = Arc::new(Metrics::default());
    metrics.ready.store(true, Ordering::Release);
    let state = AppState {
        store: store.clone(),
        metrics: Arc::clone(&metrics),
    };
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(render_metrics))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .context("bind event-dispatcher HTTP listener")?;

    if let Some(heartbeat) = HeartbeatClient::from_env(ServiceKind::EventDispatcher)
        .context("configure status heartbeat")?
    {
        let heartbeat_metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            heartbeat
                .run(|| {
                    let metrics = Arc::clone(&heartbeat_metrics);
                    async move {
                        NodeCapacity {
                            jobs_limit: 1,
                            jobs_used: u64::from(!metrics.ready.load(Ordering::Acquire)),
                            memory_bytes: process_memory_bytes(),
                            ..NodeCapacity::default()
                        }
                    }
                })
                .await;
        });
    }

    let dispatch_config = config.clone();
    let dispatch_metrics = Arc::clone(&metrics);
    let cleanup = tokio::spawn(outbox_cleanup::run(
        store.clone(),
        config.outbox_retention,
        config.outbox_cleanup_batch,
        Arc::clone(&metrics),
    ));
    let dispatch = tokio::spawn(async move {
        dispatch_loop(store, context, dispatch_config, dispatch_metrics).await;
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    println!("Fluvora event dispatcher listening on {}", config.bind);
    shutdown_signal().await;
    metrics.ready.store(false, Ordering::Release);
    dispatch.abort();
    cleanup.abort();
    server.abort();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn dispatch_loop(
    store: PostgresStore,
    context: jetstream::Context,
    config: Config,
    metrics: Arc<Metrics>,
) {
    loop {
        match store
            .claim_outbox(&config.instance_id, config.batch_size, config.claim_ttl)
            .await
        {
            Ok(messages) if messages.is_empty() => {
                metrics.ready.store(true, Ordering::Release);
                tokio::time::sleep(config.poll_interval).await;
            }
            Ok(messages) => {
                metrics
                    .claimed
                    .fetch_add(messages.len() as u64, Ordering::Relaxed);
                for message in messages {
                    dispatch_message(&store, &context, &config, &metrics, &message).await;
                }
            }
            Err(error) => {
                metrics.ready.store(false, Ordering::Release);
                eprintln!("outbox claim failed: {error}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn dispatch_message(
    store: &PostgresStore,
    context: &jetstream::Context,
    config: &Config,
    metrics: &Metrics,
    message: &OutboxMessage,
) {
    let envelope = EventEnvelope::from(message);
    let result = async {
        let payload = serde_json::to_vec(&envelope).context("serialize event envelope")?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", envelope.event_id.as_str());
        let acknowledgement = context
            .publish_with_headers(
                event_subject(&config.subject_root, message),
                headers,
                payload.into(),
            )
            .await
            .context("publish event")?;
        acknowledgement
            .await
            .context("await JetStream acknowledgement")?;
        if !store
            .acknowledge_outbox(&config.instance_id, message.id)
            .await
            .context("acknowledge PostgreSQL outbox row")?
        {
            metrics
                .acknowledgement_conflicts
                .fetch_add(1, Ordering::Relaxed);
            return Err(anyhow!("outbox lease ownership was lost"));
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    match result {
        Ok(()) => {
            metrics.published.fetch_add(1, Ordering::Relaxed);
            metrics
                .last_publish_millis
                .store(now_millis(), Ordering::Relaxed);
            metrics.ready.store(true, Ordering::Release);
        }
        Err(error) => {
            metrics.retried.fetch_add(1, Ordering::Relaxed);
            metrics.ready.store(false, Ordering::Release);
            let detail = truncate_error(&error.to_string(), 2_048);
            if let Err(retry_error) = store
                .retry_outbox(
                    &config.instance_id,
                    message.id,
                    retry_delay(message.attempts),
                    &detail,
                )
                .await
            {
                eprintln!(
                    "event {} failed ({detail}); outbox retry release failed: {retry_error}",
                    message.id
                );
            } else {
                eprintln!("event {} delivery deferred: {detail}", message.id);
            }
        }
    }
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    if state.metrics.ready.load(Ordering::Acquire) && state.store.healthcheck().await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn render_metrics(State(state): State<AppState>) -> String {
    let ready = u8::from(state.metrics.ready.load(Ordering::Acquire));
    format!(
        "# TYPE fluvora_event_dispatcher_ready gauge\n\
         fluvora_event_dispatcher_ready {ready}\n\
         # TYPE fluvora_event_dispatcher_claimed_total counter\n\
         fluvora_event_dispatcher_claimed_total {}\n\
         # TYPE fluvora_event_dispatcher_published_total counter\n\
         fluvora_event_dispatcher_published_total {}\n\
         # TYPE fluvora_event_dispatcher_retried_total counter\n\
         fluvora_event_dispatcher_retried_total {}\n\
         # TYPE fluvora_event_dispatcher_acknowledgement_conflicts_total counter\n\
         fluvora_event_dispatcher_acknowledgement_conflicts_total {}\n\
         # TYPE fluvora_event_dispatcher_pruned_total counter\n\
         fluvora_event_dispatcher_pruned_total {}\n\
         # TYPE fluvora_event_dispatcher_last_publish_millis gauge\n\
         fluvora_event_dispatcher_last_publish_millis {}\n",
        state.metrics.claimed.load(Ordering::Relaxed),
        state.metrics.published.load(Ordering::Relaxed),
        state.metrics.retried.load(Ordering::Relaxed),
        state
            .metrics
            .acknowledgement_conflicts
            .load(Ordering::Relaxed),
        state.metrics.pruned.load(Ordering::Relaxed),
        state.metrics.last_publish_millis.load(Ordering::Relaxed)
    )
}

fn parse_bounded<T>(name: &str, default: T, minimum: T, maximum: T) -> Result<T>
where
    T: std::str::FromStr + PartialOrd + Copy + std::fmt::Display,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value = env::var(name).map_or(Ok(default), |raw| {
        raw.parse()
            .with_context(|| format!("{name} must be an integer"))
    })?;
    if value < minimum || value > maximum {
        Err(anyhow!("{name} must be {minimum}..={maximum}"))
    } else {
        Ok(value)
    }
}

fn validate_identifier(name: &str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Err(anyhow!("{name} must be a bounded ASCII identifier"))
    } else {
        Ok(())
    }
}

fn validate_subject_root(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('.')
        || value.ends_with('.')
        || value.split('.').any(|token| {
            token.is_empty()
                || !token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        Err(anyhow!(
            "FLUVORA_EVENT_SUBJECT must contain bounded safe NATS tokens"
        ))
    } else {
        Ok(())
    }
}

fn truncate_error(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
