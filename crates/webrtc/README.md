# WebRTC and real-time media

Protocol implementations and real-time forwarding engines:

- Connectivity and security: `stun`, `ice-lite`, `dtls-adapter`, `srtp`.
- Media transport: `rtp`, `rtcp`, `rtc-datagram`, `rtc-session`, `sdp`.
- Interactive data: `data-channel`.
- Forwarding and adaptation: `sfu-core`, `congestion-control`, `media-codec`.
- Relay capability: `turn`.

These crates implement reusable protocol behavior. Process startup and HTTP
control APIs belong in `../services/`.
