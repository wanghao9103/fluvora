# Fluvora production development plan

[简体中文](../DEVELOPMENT_PLAN.md) | [English](DEVELOPMENT_PLAN.md)

Plan start: 2026-07-27  
Production v1 target: 2027-07-25  
Duration: 52 weeks, 26 two-week sprints  
Recommended core team: 11

The repository is a functionally complete Production Candidate v1. Protocols, transactional control
plane, distributed leases/placement, object storage, services, five SDKs, deployment, monitoring,
security, and automated release gates are implemented and tested. The 52-week plan certifies
long-term cross-browser/device compatibility, real public and multi-zone capacity, third-party
security, a 48-hour mixed-media soak, and operations sign-off before external SLO commitments.

## 1. Milestones

| Milestone | Period | Date | Exit condition |
|---|---:|---:|---|
| Technical prototype certification | Weeks 1-14 | 2026-11-01 | Three-browser ICE/DTLS/SRTP/SCTP interoperability and crash-free fuzzing |
| Real-time MVP | Weeks 15-26 | 2027-01-24 | P2P, 50-person SFU, adaptation, TURN, Web SDK, basic SLO |
| Feature beta | Weeks 27-38 | 2027-04-18 | Live, VOD, real-time transcode, five SDKs, gifts/custom data |
| Release Candidate | Weeks 39-46 | 2027-06-13 | Distributed state, placement, failover, audit, capacity benchmark |
| Production v1 | Weeks 47-52 | 2027-07-25 | 48-hour soak, rollout/rollback, alert drills, operations handoff |

## 2. Team

| Role | Count | Scope |
|---|---:|---|
| Architecture/technical lead | 1 | Protocol boundaries, ADRs, gates, coordination |
| Rust RTC engineers | 3 | ICE, DTLS-SRTP, RTP/RTCP, SCTP, SFU, congestion |
| Rust platform engineers | 2 | API, rooms, authorization, state, placement, storage |
| Media pipeline engineer | 1 | FFmpeg, CMAF/HLS, recording, VOD, transcode |
| SDK engineers | 2 | Web, Rust/C, Android, iOS, compatibility |
| QA/performance engineer | 1 | Browser E2E, impairment, capacity, regression, fuzz |
| SRE/security engineer | 1 | CI/CD, monitoring, certificates, alerts, vulnerabilities, release |

With fewer than seven people, extend Production v1 to 16-18 months. A 1-3 person team should treat
the project as research or an internal reference and not promise a public large-scale SLA.

## 3. Sprint plan

### Sprints 1-3: protocol baseline

Freeze wire corpora; establish current Chrome/Firefox/Safari nightly matrices; certify certificate
rotation and fingerprint overlap; continuously fuzz STUN, RTP/RTCP, SCTP, and room envelopes; define
protocol compatibility and deprecation policy. Exit after seven consecutive green interoperability
days and one billion crash-free fuzz inputs.

### Sprints 4-7: real-time data-plane certification

Load multi-publisher/subscriber SFU and Simulcast/SVC; calibrate NACK, PLI, Transport-CC, and receiver
reports under 0-20% loss; certify audio priority and layer hysteresis; stress SCTP retransmission and
reset; exhaust TURN transports/NAT/relay pools. Exit with a 50-person, three-layer 720p room whose
audio p99 remains healthy under video congestion.

### Sprints 8-10: control-plane consistency

Certify PostgreSQL transactions/migrations, Redis/NATS events, task ownership/leader election,
enterprise identity key rotation/revocation, and gift reconciliation/audit outbox. Exit when the
control plane scales horizontally, survives restart without state loss, and returns identical
results for duplicates.

### Sprints 11-13: real-time MVP

Certify P2P negotiation, TURN fallback, ICE restart, Web SDK reconnect/token refresh, DataChannel/WSS
fallback, placement/draining/admission, initial SLI/capacity model/runbook, and a 24-hour real-time
soak. Freeze core interface compatibility.

### Sprints 14-16: live

Certify WebRTC/WHIP-to-multi-rendition CMAF/HLS, atomic segment publication, discontinuity/restart,
object storage/CDN adapters, recording/retention, and first-frame/stall/transcode SLI. Exit after a
six-hour stream with no manifest break and automatic worker recovery.

### Sprints 17-19: VOD and SDKs

Certify large resumable uploads, checksums/dedup/cancel, probe/transcode/thumbnail/Range/HLS,
Rust/C ABI and symbol stability, Kotlin/Swift native adapters with lifecycle recovery, and the five-
SDK conformance suite. Exit with consistent SDK behavior and playable common MP4/WebM inputs.

### Sprints 20-23: Release Candidate

Certify multi-node placement, affinity/backpressure, failure reconstruction, rolling drain, multi-zone
replication/DR, third-party security, SBOM/signing, and 1/10/50/200/1000 participant cost curves.
Exit when RTO/RPO pass, no security blocker remains, and release/rollback is automated.

### Sprints 24-26: Production v1

Run the 48-hour mixed-media soak, partition/disk/certificate/worker drills, staged 1% → 10% → 50% →
100% rollout, on-call/support handoff, and final compatibility/API/SDK/capacity documentation. Exit
only with signed SLO, owners, rollback point, and data-recovery process.

## 4. Quality gates

Every PR runs Rust format, Clippy with warnings denied, full workspace tests, production OpenSSL DTLS
compilation/tests, Web SDK checks, documentation checks, and new vectors/corpus for protocol-input
changes.

Every Release Candidate covers Chrome/Firefox/Safari and Android/iOS, TURN through enterprise
firewalls, 2/5/10/20% loss and 100-1500 ms RTT, CPU/memory/bandwidth/relay/worker capacity,
vulnerability scans, key rotation, restore, and rollback drills.

## 5. Major risks

| Risk | Early signal | Response |
|---|---|---|
| Browser protocol divergence | Nightly interop fluctuation | Fixed corpora, three-browser gate, staged versions |
| In-tree SCTP/SRTP defect | Fuzz crash or retransmission exhaustion | Bounded state, differential tests, security audit |
| Transcode resource growth | Worker queue/CPU continuously grows | Quotas, admission, isolated pools, HLS fallback |
| TURN port exhaustion | Allocations exceed 80% | Fixed pools, alerts, scale/shard |
| File state prevents scale | Replica conflict | Complete transactional persistence certification |
| Gift financial risk | Duplicate receipt or ledger mismatch | Trusted verification, idempotent ledger, outbox, reconciliation |
| Schedule optimism | Progress despite failed interop/soak | Honor exit conditions; never remove security gates for a date |
