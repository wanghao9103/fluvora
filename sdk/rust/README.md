# fluvora-sdk

Complete installation, authentication, native WebRTC adapter, retry, cleanup, and troubleshooting
guidance: [`docs/SDK_INTEGRATION.md`](../../docs/SDK_INTEGRATION.md#6-rust).

The Rust SDK owns authenticated REST/signaling flows and accepts a `WebRtcPeer` adapter so desktop,
embedded, game-engine, or mobile applications can plug in their chosen standards-compatible WebRTC
implementation without coupling Fluvora's protocol to one client runtime.

`WebRtcPeer::prepare_room_data_channel` is called before offer creation. Adapters that support
standard WebRTC DataChannel should create a reliable ordered `fluvora.room.v1` channel there;
the default implementation keeps media-only adapters source-compatible.

`CallbackWebRtcPeer` is the ready-made dependency-neutral bridge: pass closures that prepare the
DataChannel, create/apply the local offer and apply the remote answer. Those closures can capture
the application's existing native peer connection.

Live WebRTC tracks can be packaged into a bounded adaptive ladder with
`create_live_abr_output_from_tracks`; the response points at the HLS master playlist.

The HTTP client rejects credential-bearing or ambiguous base URLs, does not follow redirects, and
uses bounded connect/request timeouts. JSON responses are streamed into bounded buffers (32 MiB for
successful API payloads and 64 KiB for errors), so a misconfigured or hostile upstream cannot force
unbounded SDK memory growth.
