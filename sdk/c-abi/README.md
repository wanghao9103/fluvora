# Fluvora C ABI

Complete build, linking, ownership, threading, signaling, retry, and troubleshooting guidance:
[`docs/SDK_INTEGRATION.md`](../../docs/SDK_INTEGRATION.md#9-cc-abi).

The C ABI provides blocking control/signaling functions for C, C++, Unreal, Unity native plugins,
and other FFI consumers. The application creates its native standards-compatible WebRTC offer,
calls `fluvora_exchange_offer`, and applies the returned answer.

P2P engines use `fluvora_get_ice_configuration`, `fluvora_post_signal`, and
`fluvora_poll_signals`; `recipient_id` may be null for a broadcast. Chat, join, and leave commands
return the same JSON contracts as the native SDKs.

Every returned JSON string belongs to Fluvora and must be released with `fluvora_string_free`.
Network calls are blocking; invoke them off the UI/render thread.

All inputs must be valid NUL-terminated UTF-8. Base URLs are limited to 2,048 bytes, access tokens
to 4,096 bytes, identifiers to 32 bytes, names to 128 bytes, and chat/JSON/SDP inputs to 1 MiB.
Oversized or malformed inputs return `FLUVORA_INVALID_ARGUMENT` (or a null handle during creation).

Windows static-library consumers define `FLUVORA_STATIC` before including `fluvora.h`; DLL
consumers use the default import declaration.
