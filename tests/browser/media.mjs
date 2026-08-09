import { FluvoraClient } from "../../sdk/web/dist/index.js";

const statusElement = document.querySelector("#status");
const detailsElement = document.querySelector("#details");
const fragment = new URLSearchParams(globalThis.location.hash.slice(1));
const api = fragment.get("api") ?? "http://127.0.0.1:18080";
const token = fragment.get("token");
const secondToken = fragment.get("token2");

function render(status, details) {
  statusElement.textContent = status;
  statusElement.dataset.result = status.toLowerCase();
  detailsElement.textContent = JSON.stringify(details, null, 2);
  globalThis.__fluvoraMediaInterop = { status, ...details };
}

function parseVp8(sdp) {
  const payloadType = sdp.match(/^a=rtpmap:(\d+) VP8\/90000\r?$/imu)?.[1];
  const fidSsrc = sdp.match(/^a=ssrc-group:FID (\d+) \d+\r?$/imu)?.[1];
  const firstSsrc = sdp.match(/^a=ssrc:(\d+) /imu)?.[1];
  if (!payloadType) throw new Error("browser SDP did not advertise VP8");
  return {
    payloadType: Number(payloadType),
    ssrc: Number(fidSsrc ?? firstSsrc),
  };
}

function createVideoSource() {
  const canvas = document.createElement("canvas");
  canvas.width = 320;
  canvas.height = 180;
  const context = canvas.getContext("2d");
  let frame = 0;
  const draw = () => {
    const hue = (frame * 7) % 360;
    context.fillStyle = `hsl(${hue} 80% 45%)`;
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.fillStyle = "white";
    context.font = "32px sans-serif";
    context.fillText(`Fluvora ${frame}`, 30, 100);
    frame += 1;
  };
  draw();
  const timer = globalThis.setInterval(draw, 50);
  const stream = canvas.captureStream(20);
  return {
    stream,
    stop() {
      globalThis.clearInterval(timer);
      for (const track of stream.getTracks()) track.stop();
    },
  };
}

async function waitForInboundVideo(peer, timeoutMillis) {
  const deadline = performance.now() + timeoutMillis;
  while (performance.now() < deadline) {
    const stats = await peer.getStats();
    let inbound;
    stats.forEach((report) => {
      if (report.type === "inbound-rtp" && report.kind === "video" && !report.isRemote) {
        inbound = report;
      }
    });
    if (
      inbound &&
      Number(inbound.packetsReceived ?? 0) >= 5 &&
      Number(inbound.bytesReceived ?? 0) >= 1_000
    ) {
      return {
        packetsReceived: Number(inbound.packetsReceived ?? 0),
        bytesReceived: Number(inbound.bytesReceived ?? 0),
        framesDecoded: Number(inbound.framesDecoded ?? 0),
        framesPerSecond: Number(inbound.framesPerSecond ?? 0),
      };
    }
    await new Promise((resolve) => globalThis.setTimeout(resolve, 100));
  }
  throw new Error("subscriber did not receive forwarded VP8 RTP before the timeout");
}

async function run() {
  if (!token || !secondToken) {
    throw new Error("token and token2 are required in the URL fragment");
  }
  const publisherClient = new FluvoraClient({ baseUrl: api, accessToken: token });
  const subscriberClient = new FluvoraClient({ baseUrl: api, accessToken: secondToken });
  const room = await publisherClient.createRoom("sfu", {
    maxMembers: 4,
    maxPublishers: 2,
  });
  const source = createVideoSource();
  let publisher;
  let subscriber;
  let remoteTrack;
  try {
    await subscriberClient.join(room.roomId);
    await publisherClient.startPublishing(room.roomId);
    publisher = await publisherClient.connectSfu(room.roomId, {
      rtcConfiguration: { iceServers: [] },
      localStream: source.stream,
      receiveAudio: false,
      receiveVideo: false,
      dataChannel: false,
    });
    const publisherVideo = parseVp8(publisher.peerConnection.localDescription?.sdp ?? "");
    if (!Number.isSafeInteger(publisherVideo.ssrc) || publisherVideo.ssrc <= 0) {
      throw new Error("browser SDP did not expose the primary video SSRC");
    }
    await publisherClient.publishTrack(room.roomId, {
      trackId: 1_001,
      kind: "video",
      codec: "vp8",
      clockRate: 90_000,
      payloadType: publisherVideo.payloadType,
      width: 320,
      height: 180,
      framesPerSecond: 20,
      encodings: [
        {
          ssrc: publisherVideo.ssrc,
          spatialLayer: 0,
          maxBitrateBps: 500_000,
        },
      ],
    });

    subscriber = await subscriberClient.connectSfu(room.roomId, {
      rtcConfiguration: { iceServers: [] },
      receiveAudio: false,
      receiveVideo: true,
      dataChannel: false,
      onRemoteTrack(event) {
        if (event.track.kind === "video") remoteTrack = event.track;
      },
    });
    const subscriberVideo = parseVp8(subscriber.peerConnection.localDescription?.sdp ?? "");
    const subscription = await subscriberClient.subscribeTrack(room.roomId, {
      subscriptionId: 2_001,
      trackId: 1_001,
      outputSsrc: 0x22334455,
      outputPayloadType: subscriberVideo.payloadType,
      spatialLayer: 0,
      temporalLayer: 0,
      initialSequenceNumber: 10_000,
      initialTimestamp: 1_000_000,
      subscriberCodecs: ["vp8"],
      allowTranscoding: false,
    });
    const inbound = await waitForInboundVideo(subscriber.peerConnection, 15_000);
    render("PASS", {
      roomId: room.roomId,
      publisherSessionId: publisher.sessionId,
      subscriberSessionId: subscriber.sessionId,
      publisherSsrc: publisherVideo.ssrc,
      publisherPayloadType: publisherVideo.payloadType,
      subscriberPayloadType: subscriberVideo.payloadType,
      subscriptionPath: subscription.path,
      remoteTrackState: remoteTrack?.readyState ?? "unavailable",
      inbound,
    });
  } catch (error) {
    render("FAIL", {
      name: error instanceof Error ? error.name : "Error",
      message: error instanceof Error ? error.message : String(error),
      publisherConnectionState: publisher?.peerConnection.connectionState,
      subscriberConnectionState: subscriber?.peerConnection.connectionState,
      publisherSdp: publisher?.peerConnection.localDescription?.sdp,
      subscriberSdp: subscriber?.peerConnection.localDescription?.sdp,
    });
  } finally {
    subscriber?.close();
    publisher?.close();
    source.stop();
    await publisherClient.end(room.roomId).catch(() => {});
  }
}

run().catch((error) => {
  render("FAIL", {
    name: error instanceof Error ? error.name : "Error",
    message: error instanceof Error ? error.message : String(error),
  });
});
