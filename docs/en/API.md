# Fluvora API and SDK surface

[简体中文](../API.md) | [English](API.md)

This document describes the Production Candidate v1 public control API. The default endpoint is
`http://127.0.0.1:8080`. All `/v1` JSON requests use:

```http
Authorization: Bearer <token>
Content-Type: application/json
```

State-changing business requests also use an `Idempotency-Key` of at most 128 bytes. SDKs generate
one automatically. Errors have a stable bounded shape:

```json
{"code":"machine_readable_code","message":"bounded explanation"}
```

Identifiers are unprefixed hexadecimal. Available scopes are `room_create`, `room_join`,
`media_publish`, `room_moderate`, `gift_verify`, `vod_manage`, `live_manage`, `token_revoke`, and the
internal `node_status_write`.

## Payload and replay limits

Regular JSON bodies are at most 1 MiB. VOD chunks, live init segments, and live media segments are
at most 8 MiB. WHIP/WHEP raw bodies use smaller protocol-specific limits. SDK validation applies
before network I/O: chat is 1-4096 UTF-8 bytes, custom namespaces are 1-64 safe ASCII characters,
custom JSON is at most 60 KiB, P2P signaling is at most 64 KiB, SDP is at most 256 KiB, and media
uploads are 1-8 MiB.

Signal pages, initial WebSocket replay, and live queues contain at most 128 records. Per-room replay
retains at most 128 records and 8 MiB, whichever limit is reached first.

## Rooms and interaction

| Method | Path | Purpose |
|---|---|---|
| POST | `/v1/rooms` | Create an `sfu`, `p2p`, `live`, or `vod` room |
| GET | `/v1/rooms/{room_id}` | Read the room snapshot |
| POST | `/v1/rooms/{room_id}/join` | Join a room |
| POST | `/v1/rooms/{room_id}/leave` | Leave and reclaim the participant's media resources |
| POST | `/v1/rooms/{room_id}/end` | End a room and reclaim all sessions, tracks, and tasks |
| POST | `/v1/rooms/{room_id}/roles` | Set a participant role |
| POST | `/v1/rooms/{room_id}/chat` | Append a durable chat event |
| POST | `/v1/rooms/{room_id}/custom` | Append a typed extension event |
| POST | `/v1/rooms/{room_id}/gifts` | Record a gift already verified by a trusted payment service |
| POST | `/v1/rooms/{room_id}/events/tickets` | Issue a one-time WebSocket ticket |
| GET | `/v1/rooms/{room_id}/events?ticket=...` | Subscribe to ordered room events |

Create-room example:

```json
{"mode":"sfu","max_members":50,"max_publishers":10}
```

Chat and custom events use:

```json
{"message_id":"client-unique-id","text":"hello"}
{"namespace":"com.example.whiteboard","schema_version":1,"payload":{"x":12,"y":8}}
```

Only a trusted service with `gift_verify` may call the gift endpoint. `transaction_id` is the
payment-provider idempotency key. The request includes provider metadata, sender/recipient IDs, gift
identity, quantity, unit value, and currency. `provider_signature` is a no-padding base64url
HMAC-SHA256 over the v1 domain-separated canonical payload using
`FLUVORA_GIFT_WEBHOOK_SECRET`; timestamps have a ±5 minute window.

## WebRTC and SFU

| Method | Path | Purpose |
|---|---|---|
| GET | `/v1/rooms/{room_id}/ice-servers` | Obtain short-lived TURN REST credentials |
| POST | `/v1/rooms/{room_id}/webrtc/offer` | Standard SDK offer/answer |
| POST | `/v1/rooms/{room_id}/whip` | Create a WHIP publishing session |
| PATCH/DELETE | `/v1/rooms/{room_id}/whip/{session_id}` | Trickle ICE, ICE restart, or delete |
| POST | `/v1/rooms/{room_id}/whep` | Create a WHEP playback session |
| PATCH/DELETE | `/v1/rooms/{room_id}/whep/{session_id}` | Trickle ICE, ICE restart, or delete |
| POST | `/v1/rooms/{room_id}/tracks` | Register a published track and Simulcast encodings |
| DELETE | `/v1/rooms/{room_id}/tracks/{track_id}` | Stop publishing and remove subscriptions |
| POST | `/v1/rooms/{room_id}/subscriptions` | Create an SFU downlink and select a media path |
| DELETE | `/v1/rooms/{room_id}/subscriptions/{subscription_id}` | Remove a subscription |
| POST | `/v1/rooms/{room_id}/subscriptions/{subscription_id}/layer` | Select spatial/temporal layer |

WHIP/WHEP use `application/sdp`; PATCH uses `application/trickle-ice-sdpfrag` and requires the
current strong `If-Match` ETag. A fragment must keep both current ICE credentials or replace both;
credential lengths, line lengths, candidate fields, and MID are validated before media-node calls.

SDK negotiation order is:

1. Create a standard `RTCPeerConnection` or native peer adapter.
2. Prepare the reliable ordered `fluvora.room.v1` DataChannel.
3. Add transceivers, create an offer, and set the local description.
4. POST the offer and set the returned answer.
5. Register tracks/subscriptions, or use end-to-end signaling in a P2P room.

Subscriptions may include subscriber codecs, network quality, target resolution, frame rate, and
bitrate. The server selects direct SFU, shared real-time transcode, or `hls_fallback_url`. Response
`path` is `direct`, `transcode`, `hls`, or idempotent `existing`. Transport-CC continues adapting the
Simulcast/SVC layer at runtime.

## P2P signaling

| Method | Path | Purpose |
|---|---|---|
| POST | `/v1/rooms/{room_id}/signals` | Append offer/answer/ICE/restart/renegotiate/bye |
| GET | `/v1/rooms/{room_id}/signals?after={sequence}` | Read incremental signaling |

```json
{
  "to": "optional-peer-hex-id",
  "kind": "offer",
  "payload": {"sdp": "..."}
}
```

Media is end-to-end by default and falls back to TURN candidates returned by `/ice-servers`. The
server stores only a bounded monotonically sequenced signaling backlog and never processes P2P
media.

## DataChannel room data

`fluvora.room.v1` uses binary Envelope v1. Clients may send `chat`, `control`, and `custom`; the
server verifies participant binding and rewrites room, sender, sequence, and timestamp before
broadcast. Only trusted control-plane code creates `gift` and `presence`.

A complete message is at most 16 KiB: a fixed 60-byte header and at most 16,324 application bytes.
Other labels may relay bounded text or binary extension data. The SCTP path supports ordered and
unordered reliable channels and DCEP `maxRetransmits`/`maxPacketLifeTime` partially reliable
channels, negotiated through PR-SCTP/FORWARD-TSN.

## VOD

| Method | Path | Purpose |
|---|---|---|
| POST | `/v1/assets` | Create an asset |
| GET | `/v1/assets/{asset_id}` | Read upload/processing state and playback URL |
| DELETE | `/v1/assets/{asset_id}` | Idempotently delete the asset and objects |
| PATCH | `/v1/assets/{asset_id}/source?offset={n}` | Resume binary source upload at an offset |
| POST | `/v1/assets/{asset_id}/complete` | Seal the source and submit probe/transcode |

Completion provides `source_bytes`, `segment_duration_millis`, and ABR `renditions`. The state
machine is `created → uploading → uploaded → probing → transcoding → ready`; failures return a
bounded reason and `retryable`. The media gateway (default `8093`) serves manifests, segments, and
Range requests. `FLUVORA_PUBLIC_MEDIA_BASE_URL` defines its public origin.

The API preserves successful and business 4xx responses from the media gateway. Redirects, 5xx,
oversized bodies, or non-JSON control responses become `502`.

## Live

| Method | Path | Purpose |
|---|---|---|
| POST/GET | `/v1/live/{stream_id}` | Create or inspect a live HLS output |
| DELETE | `/v1/live/{stream_id}` | Stop and delete the output |
| PUT | `/v1/live/{stream_id}/init` | Upload a CMAF init segment |
| PUT | `/v1/live/{stream_id}/segments/{sequence}` | Upload the next media segment |
| POST | `/v1/live/{stream_id}/finish` | Write ENDLIST and finish |

Creation may specify `source_tracks`, allowing the media node to feed SFU RTP into the real-time
worker for WebRTC/WHIP-to-HLS. Up to eight optional renditions share the VOD width, height, video
bitrate, and audio bitrate shape. Configured ABR returns `master.m3u8`; the single rendition remains
`index.m3u8`. External packagers may upload CMAF directly. Manifests reference only atomically
published segments and maintain a bounded live window.

## SDKs

| SDK | Directory | WebRTC integration |
|---|---|---|
| Web/TypeScript | `sdk/web` | Native `RTCPeerConnection`; built-in DataChannel/P2P orchestration |
| Rust | `sdk/rust` | Application implements async `WebRtcPeer` |
| C ABI | `sdk/c-abi` | Stable blocking control subset returning JSON to engine bindings |
| Android/Kotlin | `sdk/android` | Application implements `WebRtcPeer` |
| iOS/Swift | `sdk/ios` | Application implements `WebRTCPeer` |

High-level SDKs reject unsafe base URLs, empty/oversized/control-character tokens, redirects, and
oversized streamed responses. Reverse-proxy path prefixes are preserved consistently. See the
[SDK integration guide](SDK_INTEGRATION.md).

## Operations interfaces

Every service exposes `/health/live`, `/health/ready`, and `/metrics`. The status service also
exposes `/v1/status` for aggregated heartbeat and capacity data. Prometheus scrapes internal metrics;
business tokens, internal service tokens, TURN secrets, and certificates must never appear in
metrics or logs.
