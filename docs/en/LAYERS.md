# Code layers and dependency rules

[简体中文](../LAYERS.md) | [English](LAYERS.md)

## 1. Physical layout

```text
crates/
├── foundation/       domain, protocol, bounded codecs, observability
├── webrtc/           STUN/ICE/SDP/DTLS/SRTP/RTP/RTCP/SCTP/SFU
├── media/            storage and FFmpeg/HLS pipelines
├── control-plane/    auth, persistence, eventing, service status
├── services/         deployable process composition
└── tools/            administration and performance tools
```

SDKs, deployment manifests, examples, scripts, browser tests, and documentation live in their own
top-level directories. A deployable service may compose lower layers; protocol and domain crates
must not import service entry points.

## 2. Dependency levels

| Level | Packages | May depend on |
|---|---|---|
| 0 | bounded byte primitives, protocol, domain | Standard library and focused external primitives |
| 1 | STUN, RTP/RTCP, SRTP, media codec, control-store types | Level 0 |
| 2 | ICE-lite, SDP, SCTP/DataChannel, SFU core, media pipelines, auth/status | Levels 0-1 |
| 3 | RTC session, media-node library, API services | Levels 0-2 |
| 4 | binaries, admin/perf tools, integration tests | Levels 0-3 |

Dependencies point inward and downward. Cycles, sibling reach-through, and importing a binary crate
from a library are rejected in review and by `scripts/check-architecture.ps1`.

## 3. Internal service layers

Deployable services use the same structure:

```text
transport/routes
      ↓
application/services
      ↓
domain/contracts
      ↓
infrastructure adapters
```

Routes parse transport details and map errors. Application services authorize and orchestrate use
cases. Domain code owns invariants. Adapters perform persistence, HTTP, process, socket, or file I/O.
`main.rs` only loads configuration, builds dependencies, starts listeners, and coordinates shutdown.

## 4. State and data ownership

- PostgreSQL/control store owns durable room state, idempotency, events, outbox records, and leases.
- API memory caches are bounded optimizations and must be reconstructable.
- Media nodes own ephemeral ICE/DTLS/SRTP/SCTP and SFU forwarding state.
- Workers own child-process lifecycle and generated media artifacts.
- Large media bytes never pass through durable room snapshots.
- Locks have narrow ownership; no network or process wait is allowed while holding a synchronous
  state lock.

## 5. Automated gates

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-architecture.ps1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The architecture gate validates registered crates, level ordering, service composition, and size
limits for files that should remain focused. When a module approaches its limit, split by domain or
use case rather than creating generic `utils` or `common` dumping grounds.
