# Fluvora Swift SDK

Complete Swift Package, authentication, actor, native WebRTC adapter, lifecycle, and troubleshooting
guidance: [`docs/SDK_INTEGRATION.md`](../../docs/SDK_INTEGRATION.md#8-iosswift).

The Swift Package supports iOS 16+ and macOS 13+. Implement `WebRTCPeer` using the native WebRTC
binary already selected by the host application; Fluvora handles authenticated room commands and
offer/answer exchange without locking the app to a particular client distribution.

Override `prepareRoomDataChannel` to create a reliable ordered `fluvora.room.v1` DataChannel.
`connectSFU` invokes it before creating the offer.

`CallbackWebRTCPeer` provides the corresponding `@Sendable` async closure bridge for the native
peer connection selected by the host application.

`createLiveAbrOutputFromTracks` binds published WebRTC tracks to a multi-rendition live HLS output
and returns the master-playlist URL.

P2P applications exchange `JSONValue` signaling with `postSignal` and `pollSignals`; media remains
end to end between the platform peer connections and uses the returned TURN configuration as an ICE
fallback.

The default HTTP session rejects redirects, credential-bearing or ambiguous base URLs, and access
tokens containing control characters. JSON responses are consumed as bounded byte streams: 32 MiB
for successful responses and 64 KiB for API error bodies. A caller-supplied `URLSession` is also
checked for a changed final URL.
