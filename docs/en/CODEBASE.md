# Fluvora codebase guide

[简体中文](../CODEBASE.md) | [English](CODEBASE.md)

This guide explains where code lives, how the major paths connect, and where new functionality
belongs. Dependency rules are authoritative in [Layering rules](LAYERS.md).

## 1. Top-level directories

| Path | Responsibility |
|---|---|
| `crates/` | Rust workspace, organized by architectural layer |
| `sdk/` | Web, Rust, C ABI, Android/Kotlin, and iOS/Swift clients |
| `examples/` | Runnable cross-platform integration examples |
| `tests/browser/` | Playwright SFU, media, DataChannel, and P2P interoperability |
| `deploy/` | Containers, Compose, Kubernetes, monitoring, and alert rules |
| `migrations/` | PostgreSQL schema evolution |
| `scripts/` | Release gates, load, smoke, and acceptance workflows |
| `fuzz/` | Bounded parser/protocol fuzz targets |
| `docs/` | Chinese design and integration documentation |
| `docs/en/` | English mirrors of core documentation |

Generated outputs belong under `target/`, `artifacts/`, or guarded temporary directories and are not
source files.

## 2. Rust partitions

### 2.1 `foundation`: shared foundations

- `domain`: room/member/role/media value objects and invariants.
- `protocol`: common client/media envelopes and limits.
- `bytes`: bounded cursor and encoding primitives.
- `observability`: metrics used by deployable services.

These crates do not know about HTTP routes, database drivers, or service processes.

### 2.2 `webrtc`: real-time communication

Focused crates implement STUN, ICE-lite, SDP, DTLS adaptation, SRTP, RTP, RTCP, congestion
control, SCTP, DataChannel, SFU routing, and composite RTC sessions. Parsers are Sans-I/O and
bounded. Network sockets and task orchestration stay in service crates.

### 2.3 `media`: media processing

Media crates define codecs, storage contracts, FFmpeg process boundaries, HLS manifests, probe
results, transcoding decisions, and task state. They do not own HTTP authorization or room policy.

### 2.4 `control-plane`: durable coordination

Authentication, PostgreSQL/in-memory stores, event/outbox models, status services, placement data,
and service clients live here. Optimistic room revisions, idempotency, outbox writes, and fenced
leases are implemented as transactional store operations.

### 2.5 `services`: deployable processes

- `api-server`: public control API, signaling, room orchestration, and media control clients.
- `media-node`: shared UDP WebRTC/SFU runtime.
- `media-worker`: FFmpeg-backed asynchronous and live media work.
- `media-gateway`: bounded media/HLS delivery.
- `turn-server`: TURN UDP/TCP/TLS runtime.
- `status-service` and `event-dispatcher`: fleet state and outbox delivery.

Each service keeps `main.rs` as composition root and places reusable logic in `lib.rs` or focused
modules.

### 2.6 `tools`: operator and quality tools

`fluvora-admin` issues development tokens and performs administration. `fluvora-perf-lab` provides
repeatable hot-path capacity gates. TURN probes validate real external relay paths.

## 3. Runtime paths

### Real-time SFU

```text
SDK → API authorization/offer → media-node provision
    → ICE + DTLS-SRTP → SFU route/rewrite → subscriber
```

Control requests never carry protected RTP payloads. The media node routes packets by authenticated
tuple after ICE nomination.

### P2P and TURN

```text
Peer A ↔ API ordered signaling ↔ Peer B
Peer A ↔ direct ICE path or TURN relay ↔ Peer B
```

### Live and VOD

```text
API command → durable task/outbox → worker → shared storage → media gateway/CDN
```

### Control-plane consistency

```text
authenticated command
  → validate expected revision/idempotency
  → transaction: snapshot + event + optional ledger + outbox
  → commit
  → dispatcher claims with fenced lease
```

## 4. SDK boundary

The Web SDK owns browser `RTCPeerConnection` orchestration. Rust, Android, and Swift expose a
`WebRtcPeer`/`WebRTCPeer` adapter contract so applications can supply their chosen native WebRTC
engine. The C ABI exposes a stable blocking control/signaling subset; capture, rendering, and peer
connections remain host responsibilities.

All clients share the limits and operations described by
[`sdk-contract-v1.json`](../sdk-contract-v1.json). Runnable coverage is described by
[`sdk-demo-contract-v1.json`](../sdk-demo-contract-v1.json).

## 5. Placing new code

- Add protocol parsing to the narrow protocol crate, not an HTTP route.
- Add business invariants to domain/application code, not transport adapters.
- Add persistence as a store operation with both in-memory and PostgreSQL behavior.
- Add external I/O behind a focused client/adapter.
- Add service wiring only in the service composition root.
- Add public capabilities together with contract, SDK, example, and bilingual documentation updates.
- Split modules by responsibility before they become multi-domain files.

Avoid generic `helpers`, `misc`, or `common` modules. Name code after the invariant, protocol, or use
case it owns.

## 6. Quality gates

Run `scripts/run-release-gates.ps1`. The quick profile covers architecture, documentation, Rust,
SDK contracts, Web SDK, and TURN. The full profile additionally covers production OpenSSL, live
transcoding, HLS, capacity, and browser/control-plane soak. CI adds PostgreSQL and native platform
SDK runners.
