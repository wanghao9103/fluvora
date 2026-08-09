# Fluvora client demos

For installation, authentication, lifecycle, error handling, and production integration guidance,
read [`docs/SDK_INTEGRATION.md`](../docs/SDK_INTEGRATION.md). This page only maps runnable demos.

These examples implement the common scenarios in
[`docs/sdk-demo-contract-v1.json`](../docs/sdk-demo-contract-v1.json):

| Client | Example | Verification |
| --- | --- | --- |
| Browser | [`web/`](web/) | Real browser media, SFU, P2P signaling, DataChannel and adaptive stats |
| Rust | [`../sdk/rust/examples/room_client.rs`](../sdk/rust/examples/room_client.rs) | Buildable control/signaling CLI plus WebRTC callback adapter |
| C/C++ | [`../sdk/c-abi/examples/`](../sdk/c-abi/examples/) | Buildable C ABI CLI |
| Android | [`../sdk/android/demo/`](../sdk/android/demo/) | Buildable app and dependency-neutral native WebRTC bridge |
| iOS/Swift | [`../sdk/ios/Examples/FluvoraDemoApp/`](../sdk/ios/Examples/FluvoraDemoApp/) | Buildable SwiftUI app, CLI and native WebRTC callback bridge |

The browser owns a standard `RTCPeerConnection`, so the Web demo is fully media-capable without an
extra dependency. Native SDK examples deliberately inject the application's WebRTC engine through
`CallbackWebRtcPeer`/`CallbackWebRTCPeer`; Fluvora does not force a particular libwebrtc binary or
ABI on a host application.

Run the static demo contract gate from the repository root:

```bash
node scripts/check-sdk-demos.mjs
```
