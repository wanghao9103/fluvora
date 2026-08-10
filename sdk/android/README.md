# Fluvora Android SDK

Complete Gradle, authentication, coroutine, native WebRTC adapter, lifecycle, and troubleshooting
guidance: [`docs/SDK_INTEGRATION.md`](../../docs/SDK_INTEGRATION.md#7-androidkotlin).

The Android library handles authenticated room operations and SDP signaling. Applications implement
`WebRtcPeer` with their standards-compatible native WebRTC peer connection, which avoids forcing a
specific binary distribution or ABI into the host app.

Override `prepareRoomDataChannel` to create a reliable ordered `fluvora.room.v1` DataChannel on the
platform peer. `connectSfu` invokes it before creating the offer.

`CallbackWebRtcPeer` is a ready-made coroutine bridge for the peer connection already owned by the
application. Supply callbacks for DataChannel creation, local offer creation/application and
remote-answer application; no Fluvora-specific WebRTC binary is required.

P2P applications exchange arbitrary JSON signaling with `postSignal` and `pollSignals`; the media
remains end to end between the platform peer connections and falls back to the returned TURN
configuration when direct ICE cannot connect.

`createLiveAbrOutputFromTracks` binds published WebRTC tracks to a multi-rendition live HLS output
without requiring the application to run its own packager.

The build pins Android Gradle Plugin 9.2.0, Kotlin 2.3.21, coroutines 1.11.0, and serialization
1.11.0. Use the checked-in Gradle 9.4.1 wrapper (`./gradlew`) so local and CI builds use the same
toolchain.

The HTTP client rejects credential-bearing or ambiguous base URLs and access tokens containing
control characters. Requests do not follow redirects. JSON responses are streamed into bounded
buffers: 32 MiB for successful responses and 64 KiB for API error bodies.
