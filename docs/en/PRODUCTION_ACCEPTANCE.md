# Production v1 acceptance gates

[简体中文](../PRODUCTION_ACCEPTANCE.md) | [English](PRODUCTION_ACCEPTANCE.md)

Fluvora may be released only when both automated gates and environment certification pass. Code
existence is not production certification.

## Automated gates

| Gate | Where | Pass condition |
|---|---|---|
| Architecture/docs | CI + Release | Inward dependencies, module budgets, bilingual pairs, and local links pass |
| Rust format/Clippy/tests | CI + Release | Entire workspace green with `-D warnings` |
| Supply chain | CI + Release | cargo-deny, SBOM, provenance, and Cosign signature |
| SDK contract | CI + Release | Public operations and constants agree across Web/Rust/C/Android/Swift |
| TURN data plane | CI + Release | UDP/TCP/TLS authentication, allocation, permission, data/channel, and release pass |
| Browser SFU | CI + Release | Chromium/Firefox/WebKit ICE/DTLS/SCTP/DataChannel and VP8 SRTP forwarding pass |
| Browser P2P | CI + Release | Two-party signaling, direct transport, DataChannel, and end-to-end video pass |
| Impaired network | CI + Release | Chromium SFU/P2P pass at 80±20 ms, 5% loss, 1% reordering |
| Live/VOD pipeline | CI + Release | Real FFmpeg produces readable two-rendition relative-URI fMP4/HLS |
| Media hot path | CI + nightly | Release packet rate and p99 thresholds pass |
| Control plane | Browser CI + nightly | Transaction flow has zero errors and p95 is within threshold |
| Protocol fuzz | Nightly | Timed STUN/RTP/DataChannel targets have no crash |
| PostgreSQL/NATS | CI | Transactions, outbox, idempotency, leases/fencing, and JetStream pass |
| Android/Swift | CI + Release | Unit tests and release builds pass |

`scripts/run-browser-interop.sh` starts the real Rust API and media node, runs a quick control load,
and executes all three Playwright engines. Windows uses `scripts/run-browser-interop.ps1`. Linux can
set `FLUVORA_NETEM=true`; long control tests use `FLUVORA_SOAK_SECONDS`,
`FLUVORA_SOAK_CONCURRENCY`, and `FLUVORA_SKIP_BROWSER=true`. Soak tokens rotate through an atomic
token file without weakening the 24-hour token limit.

`scripts/smoke-hls-pipelines.ps1` verifies both VOD and live VP8/RTP inputs through the real worker,
including manifests, renditions, init/media segments, FFprobe readability, terminal task state, and
absence of host-absolute paths. `scripts/smoke-turn.ps1` and `fluvora-turn-probe` verify real TURN
transports and emit secret-free JSON evidence.

`scripts/run-release-gates.ps1 -Profile quick|full` writes versioned JSON and per-gate logs under
`artifacts/`. Failed gates are still recorded and can never be replaced by missing evidence.

## Environment certification

The following require real infrastructure and cannot be replaced by a single-machine test:

- public NAT/firewall/enterprise-proxy TURN interoperability with UDP/TCP/TLS JSON evidence;
- 1/10/50/200/1000 participant capacity and audio-priority SLO validation;
- a 48-hour mixed live/VOD/P2P/SFU/chat/gift soak;
- PostgreSQL PITR, object-storage version restore, and regional disaster recovery;
- worker/media-node crash, partition, disk-full, certificate-expiry, and alert drills;
- 1% → 10% → 50% → 100% rollout and immutable-digest rollback;
- third-party security audit with no blockers and signed owners, SLO, RTO/RPO, and support process.

Every Production v1 release links current certification evidence from the release record. Any stale
or failed item blocks release.
