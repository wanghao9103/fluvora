# Rust crates

The Rust workspace is grouped by responsibility. Put a new crate in the
directory that describes what it does, not where it happens to be used.

| Directory | Responsibility | May contain |
|---|---|---|
| `foundation/` | Stable shared vocabulary and low-level utilities | Domain types, wire contracts, bounded codecs, metrics |
| `webrtc/` | Real-time communication protocol and SFU engines | STUN, ICE, DTLS, SRTP, RTP/RTCP, SDP, SCTP, SFU |
| `media/` | Stored and transformed media | Object storage, FFmpeg pipelines, transcoding decisions |
| `control-plane/` | Shared platform coordination | Authentication, PostgreSQL state, events, service status |
| `services/` | Deployable production processes | API, media node, worker, gateway, TURN server |
| `tools/` | Operator and development executables | Admin CLI, performance gates |

Dependencies point toward stable capabilities:

```text
services/tools -> control-plane/media/webrtc -> foundation
```

Run `pwsh ./scripts/check-architecture.ps1` after adding or moving a crate.
