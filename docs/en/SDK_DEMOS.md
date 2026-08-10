# SDK demo delivery specification

[简体中文](../SDK_DEMOS.md) | [English](SDK_DEMOS.md)

See the [SDK integration guide](SDK_INTEGRATION.md) for actual application setup, dependencies,
native WebRTC adapters, retry policy, and cleanup. This document defines runnable-demo coverage.

## Goal

A public SDK release must include a buildable integration example for every client. Shared scenarios
are checked by [`sdk-demo-contract-v1.json`](../sdk-demo-contract-v1.json).

## Five-target capability matrix

| Scenario | Web | Rust | C/C++ | Android | Swift/iOS |
|---|---|---|---|---|---|
| Short-lived token and API configuration | Runnable | CLI | CLI | App | SwiftUI app + CLI |
| Create/join/leave room | Yes | Yes | Yes | Yes | Yes |
| Chat and custom data | REST + DataChannel | REST | REST | REST | REST |
| Room ICE/TURN credentials | Yes | Yes | Yes | Yes | Yes |
| SFU offer/answer | Real browser media | Host peer callbacks | Host SDP | Host peer callbacks | Host peer callbacks |
| P2P offer/answer/ICE | Real browser signaling | Signaling CLI | Signaling CLI | Signaling calls | Signaling CLI |
| Network statistics/adaptation | Full demo | Host reports/applies | Host engine | Host engine | Host engine |
| Live/VOD manifest | Video element | URL/API | URL/API | Host player | AVPlayer/host player |
| Cleanup | Tracks + peer | Room state | Client + strings | Engine + room | Peer + room |

“Host peer callbacks” are not mock WebRTC. Native applications select a WebRTC binary appropriate
for their ABI, hardware codecs, and distribution channel. Fluvora handles ICE credentials,
offer/answer, signaling, and protocol limits; the host handles capture, peer connection, rendering,
and device cleanup. Browsers use the standard `RTCPeerConnection` end to end.

## Acceptance gates

Every CI run checks:

1. `node scripts/check-sdk-contract.mjs` for server and five-SDK API alignment;
2. `node scripts/check-sdk-demos.mjs` for shared scenario coverage;
3. Rust example compilation;
4. actual C example linkage to `fluvora-c-abi`;
5. Android `:demo:assembleDebug`;
6. Swift demo product compilation;
7. three-engine browser WebRTC interoperability.

## Delivery schedule

Repository examples and CI gates are implemented. External GA certification is scheduled
separately:

| Work | Estimate | Prerequisite | Evidence |
|---|---:|---|---|
| Android mainstream libwebrtc device adapter | 3-5 days | Selected AAR/ABI/device matrix | Adapter, device logs, APK |
| iOS WebRTC Framework device adapter | 3-5 days | Selected XCFramework/signing team | Adapter, device logs, IPA |
| Unity/Unreal C ABI plugin example | 5-8 days | Engine versions and targets | Plugin project and editor smoke |
| Public NAT/TURN certification | 2-3 days | Domain, certificate, at least two NAT types | Evidence bundle |
| Linux three-browser impaired-network matrix | 2-3 days | Linux runner/netem permission | Report and screenshots |
| 48-hour multi-node stability/DR drill | 3-4 days | Three nodes and observability | SLO/recovery report |
| Third-party security/compatibility audit | 1-3 weeks | External vendor | Audit report |

These tasks depend on release infrastructure, signing assets, client-binary choices, or external
teams and cannot be replaced by repository unit tests.
