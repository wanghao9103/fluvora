# Deployable services

Production process entrypoints:

- `api-server`: REST, WebSocket, WHIP/WHEP, rooms, signaling, and orchestration.
- `media-node`: ICE/DTLS/SRTP termination and SFU data plane.
- `media-worker`: recording, transcoding, and HLS jobs.
- `media-gateway`: upload, media object, range, and HLS delivery.
- `turn-server`: UDP/TCP/TLS TURN relay process.

Service crates compose lower-level capabilities. Reusable rules should be
extracted into `foundation/`, `webrtc/`, `media/`, or `control-plane/`.
