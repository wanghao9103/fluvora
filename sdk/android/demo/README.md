# Android SDK demo

The `:demo` application compiles the full Fluvora control/signaling flow and shows the exact native
WebRTC injection point.

```bash
cd sdk/android
gradle :demo:assembleDebug
```

The emulator reaches a host API at `http://10.0.2.2:8080`. Paste a short-lived token and use the
room/data buttons immediately.

To enable media, implement `NativeWebRtcEngine` with the WebRTC PeerConnection already selected by
the host application, then install it before opening the activity:

```kotlin
WebRtcEngineProvider.factory = { iceServers ->
    ApplicationWebRtcEngine(iceServers, /* camera, microphone, renderer */)
}
```

The engine must create an ordered/reliable `fluvora.room.v1` DataChannel, add local tracks, return a
complete ICE-gathered offer, apply the server answer, surface remote tracks to the UI, and release
tracks/PeerConnection in `close()`. This is intentionally an application dependency: the Fluvora
SDK remains compatible with different libwebrtc distributions and does not impose their ABI.

On Windows, AGP can compile through the repository's non-ASCII parent path, but the JUnit worker may
still lose Kotlin test classes when Java canonicalizes that path. Run Gradle through an ASCII
directory junction in that environment; Linux/macOS and CI do not require the workaround.
