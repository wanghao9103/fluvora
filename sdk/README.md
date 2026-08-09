# Fluvora SDKs

Fluvora ships five client surfaces over one REST/signaling and room-data contract:

- `web/`: TypeScript browser SDK with native `RTCPeerConnection`, SFU, P2P, DataChannel and
  weak-network adaptation;
- `rust/`: asynchronous Rust control/signaling SDK and dependency-neutral WebRTC adapter;
- `c-abi/`: stable blocking C ABI for C/C++, desktop and game-engine plugins;
- `android/`: Kotlin control-plane SDK with a callback adapter for the host WebRTC engine;
- `ios/`: Swift concurrency SDK with a callback adapter for the host WebRTC engine.

All clients use short-lived access tokens and the reliable ordered room DataChannel label
`fluvora.room.v1` with protocol `fluvora.v1`. Native SDKs accept SDP from the application's
standards-compatible WebRTC implementation instead of forcing a binary distribution or ABI.
Authoritative Envelope v1 DataChannel messages are capped at 16 KiB including the 60-byte header,
leaving 16,324 bytes for application payloads.

Runnable/buildable examples and their verification status are indexed in
[`../examples/README.md`](../examples/README.md). The common example contract is checked with:

```bash
node scripts/check-sdk-demos.mjs
```
