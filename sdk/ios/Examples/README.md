# Swift/iOS SDK demo

`fluvora-swift-demo` is a buildable Swift client covering the control API, room data, ICE, P2P
signaling and the `CallbackWebRTCPeer` integration boundary.

```bash
swift build --package-path sdk/ios --product fluvora-swift-demo
export FLUVORA_ACCESS_TOKEN='<short-lived-token>'
swift run --package-path sdk/ios fluvora-swift-demo create sfu
swift run --package-path sdk/ios fluvora-swift-demo join <room-id>
swift run --package-path sdk/ios fluvora-swift-demo chat <room-id> hello
```

For iOS media, the same adapter closures call the host application's standards-compatible native
PeerConnection:

- create the ordered/reliable `fluvora.room.v1` DataChannel and local tracks;
- create/set a local offer and wait for ICE gathering;
- apply `answerSDP` and attach remote video/audio renderers;
- stop capture, tracks, DataChannels and the PeerConnection when leaving.

The file-based `sfu-offer` command makes that exchange observable and testable without forcing a
particular WebRTC binary into the Swift package:

```bash
swift run --package-path sdk/ios fluvora-swift-demo \
  sfu-offer <room-id> local-offer.sdp remote-answer.sdp
```

The runnable SwiftUI client project is in [`FluvoraDemoApp/`](FluvoraDemoApp/). CI builds it for an
unsigned iOS Simulator destination on every change.
