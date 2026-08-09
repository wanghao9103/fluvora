# Rust SDK command-line demo

The `room_client` example is a buildable reference for every control/signaling path and the
dependency-neutral `CallbackWebRtcPeer` bridge.

```bash
cargo build -p fluvora-sdk --example room_client
export FLUVORA_BASE_URL=http://127.0.0.1:8080
export FLUVORA_ACCESS_TOKEN='<short-lived-token>'
cargo run -p fluvora-sdk --example room_client -- create sfu
cargo run -p fluvora-sdk --example room_client -- join <room-id>
cargo run -p fluvora-sdk --example room_client -- chat <room-id> hello
```

For SFU media, let the host WebRTC implementation gather ICE and write its complete offer to a
file. The example exchanges it and writes the answer:

```bash
cargo run -p fluvora-sdk --example room_client -- \
  sfu-offer <room-id> local-offer.sdp remote-answer.sdp
```

Production adapters replace the three `CallbackWebRtcPeer` closures with calls to the application's
PeerConnection and create a reliable, ordered `fluvora.room.v1` DataChannel before the offer.
