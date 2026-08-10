# Fluvora

[简体中文](README.md) | [English](README.en.md)

Fluvora is a reference implementation of a streaming media platform written in Rust. Its real-time
media core is implemented in this repository instead of embedding an existing WebRTC or SFU server.
General-purpose cryptographic primitives are delegated to OpenSSL and RustCrypto, while FFmpeg is
used for audio/video codecs and container processing.

Implemented capabilities include:

- WebRTC ICE-lite, STUN, SDP, DTLS-SRTP, RTP, and RTCP;
- P2P signaling and TURN relay over UDP, TCP, and TLS;
- a single-node SFU, Simulcast/SVC layer selection, NACK, PLI, and Transport-CC;
- WebRTC DataChannel with in-tree SCTP, DCEP, CRC32C, SACK, PR-SCTP, FORWARD-TSN,
  fragmentation, and stream reset support;
- WHIP/WHEP, Trickle ICE, and in-session ICE restart;
- real-time transcoding, automatic pipeline reconstruction, and WebRTC-to-HLS conversion;
- multi-bitrate CMAF/HLS for live and VOD, recording, upload, probing, transcoding, and playback;
- chat, verified gifts, P2P signaling, and typed room data;
- Web, Rust, C ABI, Android/Kotlin, and iOS/Swift SDKs;
- service heartbeats, capacity-aware placement, graceful draining, Prometheus, Grafana, Compose,
  Kubernetes, and CI.

## Quick start

Docker Compose is required. Copy and customize the development environment file:

```powershell
Copy-Item deploy/compose/.env.example deploy/compose/.env
docker compose --env-file deploy/compose/.env -f deploy/compose/compose.yaml up --build -d
```

Local endpoints:

- API: `http://127.0.0.1:8080`
- media files and HLS: `http://127.0.0.1:8093`
- platform status: `http://127.0.0.1:8090/v1/status`
- Prometheus: `http://127.0.0.1:9090`
- Grafana: `http://127.0.0.1:3000`
- Alertmanager: `http://127.0.0.1:9093`
- TURN: UDP/TCP `3478`, TLS `5349`, relay UDP `49152-49251`

Issue a one-hour development token:

```powershell
docker compose --env-file deploy/compose/.env -f deploy/compose/compose.yaml run --rm api `
  fluvora-admin token --subject 1 --room * --ttl 3600 --scopes all
```

Replace every example secret, domain, certificate, and public IP before production use. The TURN
relay range must be opened consistently in the firewall, orchestration layer, and
`FLUVORA_TURN_RELAY_PORT_MIN/MAX`.

## Local verification

The unified release gates generate machine-readable
`artifacts/release-gates-*/release-gates.json` evidence and a log for every check:

```powershell
./scripts/run-release-gates.ps1 -Profile quick
./scripts/run-release-gates.ps1 -Profile full
```

The `quick` profile covers the complete Rust workspace, SDK contracts, Web SDK, and a real TURN
UDP/TCP/TLS relay. The `full` profile additionally covers production DTLS, real-time transcoding,
live/VOD HLS, capacity, and short browser/control-plane soak tests. Individual checks can also be
run directly:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-docs.ps1
cargo run --release --locked -p fluvora-perf-lab -- --quick --assert
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-hls-pipelines.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-turn.ps1

cd sdk/web
npm ci
npm run check
npm run build
```

Runnable examples for all five SDK targets are indexed in
[`examples/README.md`](examples/README.md). They cover room creation and joining, SFU, P2P
signaling, ICE, chat, custom data, and resource cleanup. See the
[`SDK demo specification`](docs/en/SDK_DEMOS.md) for the capability matrix and native WebRTC
engine boundary.

CI and release gates run real browsers, P2P/SFU paths, impaired-network tests, and control-plane
load through `scripts/run-browser-interop.sh`. On Windows,
`scripts/run-browser-interop.ps1` starts the real Rust API and media node and runs Chromium SFU,
reliable/partially reliable DataChannel, media forwarding, and P2P tests.

Run capacity or soak load directly with a short-lived token:

```powershell
$env:FLUVORA_LOAD_TOKEN = "<short-lived token>"
node scripts/load-control-plane.mjs --profile capacity
$env:FLUVORA_LOAD_TOKEN = $null
node scripts/load-control-plane.mjs --profile soak --token-file C:\secure\fluvora-soak.token
```

During long-running tests, a controlled issuer should atomically replace the token file. The load
runner reloads it every 30 seconds and immediately after a `401` response.

For public TURN acceptance, run a UDP echo endpoint on a second public host and probe UDP, TCP, and
TLS from the client network:

```bash
fluvora-turn-probe echo --bind 0.0.0.0:3479
fluvora-turn-probe probe --transport tls --server turn.example.com:5349 \
  --server-name turn.example.com --username "$TURN_USERNAME" \
  --password-file /run/secrets/turn-password --peer echo.example.net:3479 \
  --evidence artifacts/turn-tls.json
```

`FLUVORA_TURN_PROBE_PASSWORD` may instead be injected by a secret manager to keep the password out
of command lines and process listings.

Use the following packages for a complete Linux DTLS build:

```bash
sudo apt-get install libssl-dev pkg-config ffmpeg
cargo clippy -p fluvora-media-node --features openssl-backend --all-targets -- -D warnings
```

Windows environments without a system OpenSSL installation can use `openssl-vendored`, which also
requires a complete Perl and MSVC toolchain. Source paths containing non-ASCII characters should
use the ASCII Cargo cache configured by this script:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-openssl-vendored.ps1
```

CI and container images always compile the production DTLS feature.

## Repository layout

- `crates/foundation/`: domain models, shared protocols, bounded byte codecs, and observability;
- `crates/webrtc/`: STUN, ICE, DTLS, SRTP, RTP/RTCP, DataChannel, and SFU;
- `crates/media/`: media storage, FFmpeg/HLS pipelines, and transcoding decisions;
- `crates/control-plane/`: authentication, persistence, events, and service status;
- `crates/services/`: deployable API, media node, worker, gateway, and TURN processes;
- `crates/tools/`: administration CLI and performance-gate tools;
- `sdk/`: Web, Rust, C, Android, and iOS SDKs;
- `deploy/`: containers, Compose, Kubernetes, monitoring, and alerts;
- `fuzz/`: STUN, RTP, SCTP, and DataChannel fuzz targets;
- `scripts/`: unified release gates and real TURN, transcoding, VOD, and live-pipeline smoke tests;
- `tests/browser/`: real browser DataChannel, VP8-over-SFU, P2P, and Playwright matrix tests;
- `docs/`: architecture, security boundaries, acceptance gates, operations, and development plans.

Start with the [English documentation index](docs/en/README.md). New contributors should then read
the [codebase guide](docs/en/CODEBASE.md), [architecture](docs/en/ARCHITECTURE.md),
[API service design](docs/en/API_SERVER_STRUCTURE.md), [public API](docs/en/API.md),
[SDK integration guide](docs/en/SDK_INTEGRATION.md),
[production acceptance gates](docs/en/PRODUCTION_ACCEPTANCE.md), and
[operations runbook](docs/en/RUNBOOK.md).
