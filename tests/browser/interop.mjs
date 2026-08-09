import { FluvoraClient } from "../../sdk/web/dist/index.js";

const statusElement = document.querySelector("#status");
const detailsElement = document.querySelector("#details");
const fragment = new URLSearchParams(globalThis.location.hash.slice(1));
const api = fragment.get("api") ?? "http://127.0.0.1:18080";
const token = fragment.get("token");

function render(status, details) {
  statusElement.textContent = status;
  statusElement.dataset.result = status.toLowerCase();
  detailsElement.textContent = JSON.stringify(details, null, 2);
  globalThis.__fluvoraInterop = { status, ...details };
}

function timeout(milliseconds, message) {
  return new Promise((_, reject) => {
    globalThis.setTimeout(() => reject(new Error(message)), milliseconds);
  });
}

async function connectionDiagnostics(peer) {
  const reports = [];
  const stats = await peer.getStats();
  stats.forEach((report) => {
    if (
      report.type === "candidate-pair" ||
      report.type === "local-candidate" ||
      report.type === "remote-candidate" ||
      report.type === "transport" ||
      report.type === "data-channel"
    ) {
      reports.push({
        id: report.id,
        type: report.type,
        state: report.state,
        selectedCandidatePairId: report.selectedCandidatePairId,
        localCandidateId: report.localCandidateId,
        remoteCandidateId: report.remoteCandidateId,
        candidateType: report.candidateType,
        protocol: report.protocol,
        address: report.address,
        port: report.port,
        bytesSent: report.bytesSent,
        bytesReceived: report.bytesReceived,
        dtlsState: report.dtlsState,
        sctpState: report.sctpState,
        dataChannelState: report.state,
      });
    }
  });
  return reports;
}

async function run() {
  if (!token) {
    throw new Error("the URL fragment must contain token=<short-lived access token>");
  }

  const client = new FluvoraClient({ baseUrl: api, accessToken: token });
  const room = await client.createRoom("sfu", { maxMembers: 4, maxPublishers: 2 });

  let opened;
  const channelOpened = new Promise((resolve) => {
    opened = resolve;
  });
  let session;
  try {
    session = await client.connectSfu(room.roomId, {
      rtcConfiguration: { iceServers: [] },
      receiveAudio: false,
      receiveVideo: false,
      dataChannel: { onOpen: opened },
    });
    await Promise.race([
      channelOpened,
      timeout(15_000, "the reliable WebRTC DataChannel did not open"),
    ]);

    const channel = session.dataChannel;
    const peer = session.peerConnection;
    const partialChannel = peer.createDataChannel("fluvora.partial.v1", {
      ordered: false,
      maxRetransmits: 0,
      protocol: "fluvora.partial.v1",
    });
    await Promise.race([
      new Promise((resolve) => partialChannel.addEventListener("open", resolve, { once: true })),
      timeout(15_000, "the partial-reliability WebRTC DataChannel did not open"),
    ]);
    render("PASS", {
      roomId: room.roomId,
      sessionId: session.sessionId,
      connectionState: peer.connectionState,
      iceConnectionState: peer.iceConnectionState,
      iceGatheringState: peer.iceGatheringState,
      signalingState: peer.signalingState,
      sctpState: peer.sctp?.transport.state ?? "unavailable",
      dataChannel: {
        label: channel?.label,
        protocol: channel?.protocol,
        ordered: channel?.ordered,
        readyState: channel?.readyState,
      },
      partialDataChannel: {
        label: partialChannel.label,
        protocol: partialChannel.protocol,
        ordered: partialChannel.ordered,
        maxRetransmits: partialChannel.maxRetransmits,
        readyState: partialChannel.readyState,
      },
    });
  } catch (error) {
    const peer = session?.peerConnection;
    render("FAIL", {
      name: error instanceof Error ? error.name : "Error",
      message: error instanceof Error ? error.message : String(error),
      connectionState: peer?.connectionState,
      iceConnectionState: peer?.iceConnectionState,
      iceGatheringState: peer?.iceGatheringState,
      signalingState: peer?.signalingState,
      sctpState: peer?.sctp?.transport.state ?? "unavailable",
      dataChannelState: session?.dataChannel?.readyState,
      remoteSdp: peer?.remoteDescription?.sdp
        .split(/\r?\n/u)
        .filter((line) =>
          /^(a=(ice-lite|ice-ufrag|ice-pwd|candidate|end-of-candidates|setup|fingerprint|sctp-port|max-message-size)|m=application)/u.test(
            line,
          ),
        ),
      stats: peer ? await connectionDiagnostics(peer) : [],
    });
  } finally {
    session?.close();
    await client.end(room.roomId).catch(() => {});
  }
}

run().catch((error) => {
  render("FAIL", {
    name: error instanceof Error ? error.name : "Error",
    message: error instanceof Error ? error.message : String(error),
  });
});
