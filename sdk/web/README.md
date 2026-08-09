# @fluvora/web

Complete installation, authentication, SFU/P2P, retry, cleanup, and troubleshooting guidance:
[`docs/SDK_INTEGRATION.md`](../../docs/SDK_INTEGRATION.md#5-webtypescript).

Browser SDK for Fluvora room control, WebRTC SFU negotiation, P2P signaling, interactive data,
and sender-side weak-network adaptation.

```ts
import { FluvoraClient } from "@fluvora/web";

const client = new FluvoraClient({
  baseUrl: "https://api.example.com",
  accessToken: async () => getShortLivedToken(),
});

await client.join(roomId);
const session = await client.connectSfu(roomId, {
  localStream,
  onRemoteTrack: ({ streams }) => {
    remoteVideo.srcObject = streams[0];
  },
  fallbackHlsUrl: "https://cdn.example.com/live/channel/master.m3u8",
  onFallback: (url) => {
    remoteVideo.src = url;
  },
  dataChannel: {
    onRoomEnvelope: (envelope) => {
      console.log(envelope.kind, new TextDecoder().decode(envelope.payload));
    },
  },
});

session.sendRoomData("chat", "hello");

await client.createLiveAbrOutputFromTracks("stream-id", sourceTracks, {
  segmentDurationMillis: 2000,
  renditions: [
    { width: 1280, height: 720, videoBitrate: 2_500_000, audioBitrate: 128_000 },
    { width: 640, height: 360, videoBitrate: 800_000, audioBitrate: 96_000 },
  ],
});
```

The SDK uses the browser's standards-compliant `RTCPeerConnection`. Fluvora's signaling, ICE-lite,
DTLS-SRTP, RTP/RTCP, SCTP/DCEP, congestion control, and SFU implementations remain server-owned.
Unless disabled with `dataChannel: false`, the SDK creates a reliable ordered
`fluvora.room.v1` channel before producing the SDP offer.

The HTTP client rejects credential-bearing or ambiguous base URLs and access tokens containing
control characters. Requests do not follow redirects. JSON responses are streamed into bounded
buffers: 32 MiB for successful responses and 64 KiB for API error bodies.

`sendRoomData` is the only raw-send path for the authoritative `fluvora.room.v1` channel. All
DataChannel messages are capped at the server's 16 KiB message limit before browser transmission.
