import { FluvoraClient } from "../../sdk/web/dist/index.js";

const statusElement = document.querySelector("#status");
const detailsElement = document.querySelector("#details");
const fragment = new URLSearchParams(globalThis.location.hash.slice(1));
const api = fragment.get("api") ?? "http://127.0.0.1:18080";
const tokenA = fragment.get("token");
const tokenB = fragment.get("token2");
const participantA = "00000000000000000000000000000001";
const participantB = "00000000000000000000000000000002";

function render(status, details) {
  statusElement.textContent = status;
  statusElement.dataset.result = status.toLowerCase();
  detailsElement.textContent = JSON.stringify(details, null, 2);
  globalThis.__fluvoraP2pInterop = { status, ...details };
}

function timeout(milliseconds, message) {
  return new Promise((_, reject) => {
    globalThis.setTimeout(() => reject(new Error(message)), milliseconds);
  });
}

function createVideoSource() {
  const canvas = document.createElement("canvas");
  canvas.width = 160;
  canvas.height = 90;
  const context = canvas.getContext("2d");
  let frame = 0;
  const draw = () => {
    context.fillStyle = frame % 2 === 0 ? "#165dff" : "#00b578";
    context.fillRect(0, 0, canvas.width, canvas.height);
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

async function waitForInboundVideo(peer) {
  const deadline = performance.now() + 10_000;
  while (performance.now() < deadline) {
    const stats = await peer.getStats();
    let inbound;
    stats.forEach((report) => {
      if (report.type === "inbound-rtp" && report.kind === "video" && !report.isRemote) {
        inbound = report;
      }
    });
    if (inbound && Number(inbound.packetsReceived ?? 0) >= 5) {
      return {
        packetsReceived: Number(inbound.packetsReceived ?? 0),
        bytesReceived: Number(inbound.bytesReceived ?? 0),
        framesDecoded: Number(inbound.framesDecoded ?? 0),
      };
    }
    await new Promise((resolve) => globalThis.setTimeout(resolve, 100));
  }
  throw new Error("P2P video was not delivered");
}

async function run() {
  if (!tokenA || !tokenB) {
    throw new Error("the URL fragment must contain token and token2");
  }
  const clientA = new FluvoraClient({ baseUrl: api, accessToken: tokenA });
  const clientB = new FluvoraClient({ baseUrl: api, accessToken: tokenB });
  render("RUNNING", { stage: "creating-room" });
  const room = await clientA.createRoom("p2p", { maxMembers: 2, maxPublishers: 2 });
  render("RUNNING", { stage: "joining-second-participant", roomId: room.roomId });
  await clientB.join(room.roomId);
  render("RUNNING", { stage: "creating-peer-connections", roomId: room.roomId });

  const peerA = new RTCPeerConnection({ iceServers: [] });
  const peerB = new RTCPeerConnection({ iceServers: [] });
  const source = createVideoSource();
  for (const track of source.stream.getTracks()) peerA.addTrack(track, source.stream);
  let remoteVideoTrack;
  peerB.addEventListener("track", (event) => {
    if (event.track.kind === "video") remoteVideoTrack = event.track;
  });
  const channelA = peerA.createDataChannel("fluvora.p2p.v1", {
    ordered: true,
    protocol: "fluvora.v1",
  });
  let channelB;
  let openedA;
  let openedB;
  let receivedB;
  const channelAOpened = new Promise((resolve) => {
    openedA = resolve;
  });
  const channelBOpened = new Promise((resolve) => {
    openedB = resolve;
  });
  const messageAtB = new Promise((resolve) => {
    receivedB = resolve;
  });
  channelA.addEventListener("open", openedA);
  peerB.addEventListener("datachannel", (event) => {
    channelB = event.channel;
    channelB.addEventListener("open", openedB);
    channelB.addEventListener("message", (message) => receivedB(message.data));
  });

  const sessionA = clientA.createP2pSession(room.roomId, participantA, peerA);
  const sessionB = clientB.createP2pSession(room.roomId, participantB, peerB);
  sessionA.start();
  sessionB.start();
  try {
    render("RUNNING", { stage: "posting-offer", roomId: room.roomId });
    await sessionA.offer(participantB);
    render("RUNNING", { stage: "waiting-for-data-channels", roomId: room.roomId });
    await Promise.race([
      Promise.all([channelAOpened, channelBOpened]),
      timeout(20_000, "P2P DataChannels did not open"),
    ]);
    channelA.send("fluvora-p2p-probe");
    const message = await Promise.race([
      messageAtB,
      timeout(5_000, "P2P message was not delivered"),
    ]);
    const inboundVideo = await waitForInboundVideo(peerB);
    render("PASS", {
      roomId: room.roomId,
      peerA: {
        connectionState: peerA.connectionState,
        iceConnectionState: peerA.iceConnectionState,
        dataChannelState: channelA.readyState,
      },
      peerB: {
        connectionState: peerB.connectionState,
        iceConnectionState: peerB.iceConnectionState,
        dataChannelState: channelB?.readyState,
      },
      message,
      remoteVideoTrackState: remoteVideoTrack?.readyState ?? "unavailable",
      inboundVideo,
    });
  } catch (error) {
    render("FAIL", {
      name: error instanceof Error ? error.name : "Error",
      message: error instanceof Error ? error.message : String(error),
      peerA: {
        connectionState: peerA.connectionState,
        iceConnectionState: peerA.iceConnectionState,
        signalingState: peerA.signalingState,
        dataChannelState: channelA.readyState,
      },
      peerB: {
        connectionState: peerB.connectionState,
        iceConnectionState: peerB.iceConnectionState,
        signalingState: peerB.signalingState,
        dataChannelState: channelB?.readyState,
      },
    });
  } finally {
    sessionA.close();
    sessionB.close();
    source.stop();
    await clientA.end(room.roomId).catch(() => {});
  }
}

run().catch((error) => {
  render("FAIL", {
    name: error instanceof Error ? error.name : "Error",
    message: error instanceof Error ? error.message : String(error),
  });
});
