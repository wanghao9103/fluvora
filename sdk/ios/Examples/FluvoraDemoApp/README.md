# Fluvora iOS Demo App

Open `FluvoraDemoApp.xcodeproj` or build the unsigned Simulator app:

```bash
xcodebuild \
  -project sdk/ios/Examples/FluvoraDemoApp/FluvoraDemoApp.xcodeproj \
  -scheme FluvoraDemoApp \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build
```

The project consumes the local `sdk/ios` Swift package. Room creation, join/leave, ICE, durable chat
and custom data run immediately. To enable camera/microphone SFU media, construct `DemoModel` with
the host application's `NativeWebRTCEngineFactory`; the engine receives Fluvora's room-scoped ICE
servers and owns capture, rendering, PeerConnection and cleanup.
