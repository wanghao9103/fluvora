import { FluvoraClient } from "../../sdk/web/dist/index.js";

const elements = Object.fromEntries(
  [
    "base-url", "token", "mode", "room-id", "participant-id", "state", "local-video",
    "remote-video", "quality", "rtt", "loss", "remote-participant-id", "chat-text",
    "custom-json", "manifest-url", "log",
  ].map((id) => [id, document.getElementById(id)]),
);

let client;
let localStream;
let sfuSession;
let p2pSession;
let p2pPeer;
let p2pDataChannel;

function log(message, details) {
  const timestamp = new Date().toLocaleTimeString();
  const suffix = details === undefined ? "" : ` ${JSON.stringify(details, null, 2)}`;
  elements.log.textContent += `[${timestamp}] ${message}${suffix}\n`;
  elements.log.scrollTop = elements.log.scrollHeight;
}

function setState(label, state = "idle") {
  elements.state.textContent = label;
  elements.state.dataset.state = state;
}

function requireValue(id, label) {
  const value = elements[id].value.trim();
  if (!value) throw new Error(`${label}不能为空`);
  return value;
}

function currentClient() {
  const baseUrl = requireValue("base-url", "API 地址");
  const token = requireValue("token", "Token");
  client = new FluvoraClient({ baseUrl, accessToken: token });
  return client;
}

function currentRoom() {
  return requireValue("room-id", "房间 ID");
}

async function run(label, operation, completed = { label: "已就绪", state: "connected" }) {
  try {
    setState(`${label}…`);
    await operation();
    setState(completed.label, completed.state);
  } catch (error) {
    setState("发生错误", "error");
    log(`${label}失败`, { message: error instanceof Error ? error.message : String(error) });
  }
}

elements.token.addEventListener("input", () => {
  try {
    const encoded = elements.token.value.split(".")[1].replaceAll("-", "+").replaceAll("_", "/");
    const padded = encoded.padEnd(Math.ceil(encoded.length / 4) * 4, "=");
    const payload = JSON.parse(
      new TextDecoder().decode(
        Uint8Array.from(
          atob(padded),
          (character) => character.charCodeAt(0),
        ),
      ),
    );
    if (typeof payload.sub === "string" || typeof payload.sub === "number") {
      elements["participant-id"].value = String(payload.sub);
    }
  } catch {
    // The API remains the source of truth; this unverified decode is only a UI convenience.
  }
});

async function ensureLocalStream() {
  if (localStream) return localStream;
  localStream = await navigator.mediaDevices.getUserMedia({ audio: true, video: true });
  elements["local-video"].srcObject = localStream;
  log("本地音视频已打开", localStream.getTracks().map((track) => track.kind));
  return localStream;
}

function attachRemoteTrack(event) {
  const [stream] = event.streams;
  if (stream) {
    elements["remote-video"].srcObject = stream;
    return;
  }
  const current = elements["remote-video"].srcObject ?? new MediaStream();
  current.addTrack(event.track);
  elements["remote-video"].srcObject = current;
}

document.getElementById("create-room").addEventListener("click", () => run("创建房间", async () => {
  const room = await currentClient().createRoom(elements.mode.value, {
    maxMembers: 64,
    maxPublishers: elements.mode.value === "sfu" ? 16 : 2,
  });
  elements["room-id"].value = room.roomId;
  log("房间已创建", room);
}));

document.getElementById("join-room").addEventListener("click", () => run("加入房间", async () => {
  const result = await currentClient().join(currentRoom());
  log("已加入房间", result);
}));

document.getElementById("start-media").addEventListener("click", () => run("打开媒体", async () => {
  await ensureLocalStream();
}));

document.getElementById("connect-sfu").addEventListener("click", () => run("连接 SFU", async () => {
  const sdk = currentClient();
  const roomId = currentRoom();
  await sdk.startPublishing(roomId);
  sfuSession?.close();
  sfuSession = await sdk.connectSfu(roomId, {
    localStream: await ensureLocalStream(),
    onRemoteTrack: attachRemoteTrack,
    fallbackHlsUrl: elements["manifest-url"].value.trim() || undefined,
    onFallback: (url) => {
      elements["remote-video"].srcObject = null;
      elements["remote-video"].src = url;
      void elements["remote-video"].play();
      log("连续弱网，已切换 HLS", { url });
    },
    onNetworkSample: (sample) => {
      elements.quality.textContent = sample.quality;
      elements.rtt.textContent =
        sample.roundTripTimeMs === undefined ? "—" : `${sample.roundTripTimeMs.toFixed(0)} ms`;
      elements.loss.textContent = `${(sample.packetLossRatio * 100).toFixed(1)}%`;
    },
    dataChannel: {
      onOpen: () => log("房间 DataChannel 已打开", { label: "fluvora.room.v1" }),
      onRoomEnvelope: (envelope) => log("收到房间 DataChannel 信封", {
        kind: envelope.kind,
        senderId: envelope.senderId,
        payload: new TextDecoder().decode(envelope.payload),
      }),
      onError: (error) => log("DataChannel 错误", { message: error.message }),
    },
  });
  log("SFU 协商完成", { sessionId: sfuSession.sessionId });
}));

document.getElementById("start-p2p").addEventListener("click", () => run("启动 P2P", async () => {
  const sdk = currentClient();
  const roomId = currentRoom();
  const ice = await sdk.getIceConfiguration(roomId);
  p2pSession?.close();
  p2pPeer = new RTCPeerConnection({ iceServers: ice.iceServers });
  for (const track of (await ensureLocalStream()).getTracks()) {
    p2pPeer.addTrack(track, localStream);
  }
  p2pPeer.addEventListener("track", attachRemoteTrack);
  p2pPeer.addEventListener("datachannel", (event) => {
    p2pDataChannel = event.channel;
    p2pDataChannel.addEventListener("message", (message) => log("收到 P2P 数据", message.data));
  });
  p2pDataChannel = p2pPeer.createDataChannel("fluvora.room.v1", {
    ordered: true,
    protocol: "fluvora.v1",
  });
  p2pDataChannel.addEventListener("message", (event) => log("收到 P2P 数据", event.data));
  p2pSession = sdk.createP2pSession(
    roomId,
    requireValue("participant-id", "参与者 ID"),
    p2pPeer,
  );
  p2pSession.start();
  log("P2P 信令轮询已启动", { iceServerCount: ice.iceServers.length });
}));

document.getElementById("send-offer").addEventListener("click", () => run("发送 P2P Offer", async () => {
  if (!p2pSession) throw new Error("请先启动 P2P");
  const recipient = requireValue("remote-participant-id", "对端参与者 ID");
  await p2pSession.offer(recipient);
  log("P2P Offer 已发送", { recipient });
}));

document.getElementById("send-chat").addEventListener("click", () => run("发送聊天", async () => {
  const text = requireValue("chat-text", "聊天内容");
  const result = await currentClient().sendChat(currentRoom(), text);
  if (sfuSession?.dataChannel?.readyState === "open") {
    sfuSession.sendRoomData("chat", text, { acknowledgementRequired: true });
  }
  if (p2pDataChannel?.readyState === "open") p2pDataChannel.send(text);
  log("聊天已发送", result);
}));

document.getElementById("send-custom").addEventListener("click", () => run("发送自定义数据", async () => {
  const payload = JSON.parse(requireValue("custom-json", "自定义 JSON"));
  const result = await currentClient().sendCustomData(currentRoom(), "demo.interaction", 1, payload);
  if (sfuSession?.dataChannel?.readyState === "open") {
    sfuSession.sendRoomData({ custom: 1000 }, JSON.stringify(payload));
  }
  log("自定义数据已发送", result);
}));

document.getElementById("play-manifest").addEventListener("click", () => run("播放 HLS", async () => {
  const url = requireValue("manifest-url", "Manifest URL");
  elements["remote-video"].srcObject = null;
  elements["remote-video"].src = url;
  await elements["remote-video"].play();
  log("已加载直播/点播 Manifest", { url });
}));

document.getElementById("leave-room").addEventListener("click", () =>
  run(
    "离开房间",
    async () => {
      sfuSession?.close();
      sfuSession = undefined;
      p2pSession?.close();
      p2pSession = undefined;
      p2pPeer = undefined;
      p2pDataChannel = undefined;
      for (const track of localStream?.getTracks() ?? []) track.stop();
      localStream = undefined;
      elements["local-video"].srcObject = null;
      elements["remote-video"].srcObject = null;
      elements["remote-video"].removeAttribute("src");
      if (client && elements["room-id"].value.trim()) {
        await client.leave(currentRoom());
      }
      log("本地媒体、PeerConnection 与房间成员状态已清理");
    },
    { label: "未连接", state: "idle" },
  ),
);

document.getElementById("clear-log").addEventListener("click", () => {
  elements.log.textContent = "";
});

window.addEventListener("beforeunload", () => {
  sfuSession?.close();
  p2pSession?.close();
  for (const track of localStream?.getTracks() ?? []) track.stop();
});

log("Demo 已加载；Token 只保存在当前页面内存中");
