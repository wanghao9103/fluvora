mod nonce;
mod server;

use std::env;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use fluvora_status_client::{HeartbeatClient, process_memory_bytes};
use fluvora_status_service::{NodeCapacity, ServiceKind};
use server::{ServerConfig, TlsConfiguration, TurnServer};

#[tokio::main]
async fn main() {
    let configuration = configuration_from_environment();
    let turn_address = configuration.control_bind;
    let status_address = configuration.status_bind;
    let tls_address = configuration.tls.as_ref().map(|tls| tls.bind);
    let server = Arc::new(
        TurnServer::bind(configuration)
            .await
            .expect("TURN server bind"),
    );
    println!(
        "{} TURN UDP/TCP {turn_address}, status {status_address}",
        fluvora_domain::PLATFORM_NAME
    );
    if let Some(tls_address) = tls_address {
        println!("{} TURN/TLS {tls_address}", fluvora_domain::PLATFORM_NAME);
    }
    let (heartbeat, heartbeat_task) = start_heartbeat(Arc::clone(&server));
    let shutdown_heartbeat = heartbeat.clone();
    let shutdown_server = Arc::clone(&server);
    Arc::clone(&server)
        .run(async move {
            server::shutdown_signal().await;
            if let Some(client) = shutdown_heartbeat.as_ref() {
                client.mark_draining();
                let _ = client
                    .report(turn_capacity(&shutdown_server).await, true)
                    .await;
            }
        })
        .await
        .expect("TURN server runtime");
    stop_heartbeat(heartbeat.as_ref(), heartbeat_task, &server).await;
}

fn start_heartbeat(
    server: Arc<TurnServer>,
) -> (Option<HeartbeatClient>, Option<tokio::task::JoinHandle<()>>) {
    let client =
        HeartbeatClient::from_env(ServiceKind::Turn).expect("valid status heartbeat configuration");
    let task = client.as_ref().map(|client| {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .run(|| {
                    let server = Arc::clone(&server);
                    async move { turn_capacity(&server).await }
                })
                .await;
        })
    });
    (client, task)
}

async fn stop_heartbeat(
    client: Option<&HeartbeatClient>,
    task: Option<tokio::task::JoinHandle<()>>,
    server: &TurnServer,
) {
    if let Some(client) = client {
        client.mark_draining();
        if let Err(error) = client.report(turn_capacity(server).await, true).await {
            eprintln!("failed to report draining TURN heartbeat: {error}");
        }
    }
    if let Some(task) = task {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            eprintln!("TURN heartbeat task failed during shutdown: {error}");
        }
    }
}

async fn turn_capacity(server: &TurnServer) -> NodeCapacity {
    NodeCapacity {
        turn_allocations: u64::try_from(server.active_allocations().await).unwrap_or(u64::MAX),
        sessions_limit: u64::try_from(server.allocation_limit()).unwrap_or(u64::MAX),
        sessions_used: u64::try_from(server.active_allocations().await).unwrap_or(u64::MAX),
        memory_bytes: process_memory_bytes(),
        ..NodeCapacity::default()
    }
}

fn configuration_from_environment() -> ServerConfig {
    let control_bind = env::var("FLUVORA_TURN_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3478".to_owned())
        .parse::<SocketAddr>()
        .expect("FLUVORA_TURN_BIND must be host:port");
    let status_bind = env::var("FLUVORA_TURN_STATUS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8094".to_owned())
        .parse::<SocketAddr>()
        .expect("FLUVORA_TURN_STATUS_BIND must be host:port");
    let advertised_ip =
        env::var("FLUVORA_TURN_ADVERTISED_IP")
            .ok()
            .map_or(control_bind.ip(), |value| {
                value
                    .parse::<IpAddr>()
                    .expect("FLUVORA_TURN_ADVERTISED_IP must be an IP address")
            });
    assert!(
        !advertised_ip.is_unspecified(),
        "FLUVORA_TURN_ADVERTISED_IP is required when binding an unspecified address"
    );
    let relay_bind_ip =
        env::var("FLUVORA_TURN_RELAY_BIND_IP")
            .ok()
            .map_or(control_bind.ip(), |value| {
                value
                    .parse::<IpAddr>()
                    .expect("FLUVORA_TURN_RELAY_BIND_IP must be an IP address")
            });
    let relay_port_min = bounded_u16("FLUVORA_TURN_RELAY_PORT_MIN", 49_152);
    let relay_port_max = bounded_u16("FLUVORA_TURN_RELAY_PORT_MAX", 65_535);
    let allow_private_peers = boolean("FLUVORA_TURN_ALLOW_PRIVATE_PEERS", false);
    let maximum_relay_bytes_per_second = bounded_u64(
        "FLUVORA_TURN_MAX_RELAY_BYTES_PER_SECOND",
        5_000_000,
        65_000,
        1_000_000_000,
    );
    let username = required_bounded("FLUVORA_TURN_USERNAME", 128);
    let password = required_bounded("FLUVORA_TURN_PASSWORD", 256);
    let realm = required_bounded("FLUVORA_TURN_REALM", 128);
    let nonce_secret = required_bounded("FLUVORA_TURN_NONCE_SECRET", 4_096).into_bytes();
    assert!(
        nonce_secret.len() >= 32,
        "FLUVORA_TURN_NONCE_SECRET must contain at least 32 bytes"
    );
    let rest_secret = required_bounded("FLUVORA_TURN_REST_SECRET", 4_096).into_bytes();
    assert!(
        rest_secret.len() >= 32,
        "FLUVORA_TURN_REST_SECRET must contain at least 32 bytes"
    );
    let maximum_allocations = bounded_usize("FLUVORA_TURN_MAX_ALLOCATIONS", 10_000, 1, 100_000);
    let maximum_allocations_per_ip =
        bounded_usize("FLUVORA_TURN_MAX_ALLOCATIONS_PER_IP", 16, 1, 1_024);
    let tls = env::var("FLUVORA_TURN_TLS_BIND").ok().map(|bind| {
        let bind = bind
            .parse::<SocketAddr>()
            .expect("FLUVORA_TURN_TLS_BIND must be host:port");
        let certificate_pem = env::var("FLUVORA_TURN_TLS_CERT")
            .expect("FLUVORA_TURN_TLS_CERT is required when TURN/TLS is enabled")
            .into();
        let private_key_pem = env::var("FLUVORA_TURN_TLS_KEY")
            .expect("FLUVORA_TURN_TLS_KEY is required when TURN/TLS is enabled")
            .into();
        TlsConfiguration {
            bind,
            certificate_pem,
            private_key_pem,
        }
    });
    ServerConfig {
        control_bind,
        status_bind,
        advertised_ip,
        relay_bind_ip,
        relay_port_min,
        relay_port_max,
        allow_private_peers,
        maximum_relay_bytes_per_second,
        username,
        password,
        realm,
        nonce_secret,
        rest_secret,
        maximum_allocations,
        maximum_allocations_per_ip,
        tls,
    }
}

fn required_bounded(name: &str, maximum: usize) -> String {
    let value = env::var(name).unwrap_or_else(|_| panic!("{name} is required"));
    assert!(
        !value.is_empty()
            && value.len() <= maximum
            && value
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic()),
        "{name} must be bounded printable ASCII"
    );
    value
}

fn bounded_usize(name: &str, default: usize, minimum: usize, maximum: usize) -> usize {
    let value = env::var(name)
        .map_or(Ok(default), |value| value.parse::<usize>())
        .unwrap_or_else(|_| panic!("{name} must be an integer"));
    assert!(
        (minimum..=maximum).contains(&value),
        "{name} must be {minimum}..={maximum}"
    );
    value
}

fn bounded_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .map_or(Ok(default), |value| value.parse::<u16>())
        .unwrap_or_else(|_| panic!("{name} must be a UDP port"))
}

fn bounded_u64(name: &str, default: u64, minimum: u64, maximum: u64) -> u64 {
    let value = env::var(name)
        .map_or(Ok(default), |value| value.parse::<u64>())
        .unwrap_or_else(|_| panic!("{name} must be an integer"));
    assert!(
        (minimum..=maximum).contains(&value),
        "{name} must be {minimum}..={maximum}"
    );
    value
}

fn boolean(name: &str, default: bool) -> bool {
    env::var(name).map_or(default, |value| match value.as_str() {
        "true" => true,
        "false" => false,
        _ => panic!("{name} must be true or false"),
    })
}
