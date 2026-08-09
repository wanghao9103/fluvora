# C ABI demo

Build the Rust static library, then compile the C client:

```bash
cargo build -p fluvora-c-abi --release
cmake -S sdk/c-abi/examples -B target/c-demo \
  -DFLUVORA_LIBRARY_DIR="$PWD/target/release"
cmake --build target/c-demo
```

Run it with a short-lived token:

```bash
export FLUVORA_ACCESS_TOKEN='<short-lived-token>'
target/c-demo/fluvora-c-demo create sfu
target/c-demo/fluvora-c-demo join <room-id>
target/c-demo/fluvora-c-demo chat <room-id> hello
```

The `sfu-offer` command accepts a complete SDP offer produced by the application's native
PeerConnection. Parse `answer_sdp` from the response and apply it to the same PeerConnection. C++,
Unity native plugins and Unreal modules use the identical ABI and must free every returned string
with `fluvora_string_free`.
