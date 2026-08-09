import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const contract = JSON.parse(
  await readFile(path.join(root, "docs", "sdk-contract-v1.json"), "utf8"),
);
const files = {
  api: "crates/services/api-server/src/main.rs",
  apiApp: "crates/services/api-server/src/app.rs",
  apiRoutesRooms: "crates/services/api-server/src/routes/rooms.rs",
  apiRoutesMedia: "crates/services/api-server/src/routes/media.rs",
  apiRoutesSignaling: "crates/services/api-server/src/routes/signaling.rs",
  apiRoutesWebRtc: "crates/services/api-server/src/routes/webrtc.rs",
  apiSignals: "crates/services/api-server/src/signals.rs",
  apiPersistence: "crates/services/api-server/src/persistence.rs",
  apiGatewayRoutes: "crates/services/api-server/src/gateway_routes.rs",
  controlStore: "crates/control-plane/control-store/src/lib.rs",
  eventDispatcher: "crates/control-plane/event-dispatcher/src/main.rs",
  outboxCleanup: "crates/control-plane/event-dispatcher/src/outbox_cleanup.rs",
  domain: "crates/foundation/domain/src/lib.rs",
  web: "sdk/web/src/index.ts",
  rust: "sdk/rust/src/lib.rs",
  android: "sdk/android/fluvora/src/main/java/com/fluvora/sdk/FluvoraClient.kt",
  ios: "sdk/ios/Sources/Fluvora/FluvoraClient.swift",
  c: "sdk/c-abi/include/fluvora.h",
  cImpl: "sdk/c-abi/src/lib.rs",
  protocol: "crates/foundation/protocol/src/lib.rs",
  mediaNode: "crates/services/media-node/src/main.rs",
};
const sources = Object.fromEntries(
  await Promise.all(
    Object.entries(files).map(async ([name, relative]) => [
      name,
      await readFile(path.join(root, relative), "utf8"),
    ]),
  ),
);
const apiSurface = [
  sources.api,
  sources.apiApp,
  sources.apiRoutesRooms,
  sources.apiRoutesMedia,
  sources.apiRoutesSignaling,
  sources.apiRoutesWebRtc,
].join("\n");
const failures = [];
for (const operation of contract.operations) {
  if (!apiSurface.includes(`"${operation.path}"`)) {
    failures.push(`API route missing for ${operation.id}: ${operation.path}`);
  }
  for (const [sdk, marker] of Object.entries(operation.markers)) {
    if (!sources[sdk].includes(marker)) {
      failures.push(`${sdk} SDK missing ${operation.id}: ${marker}`);
    }
  }
}
for (const marker of contract.cAbiMarkers) {
  if (!sources.c.includes(marker)) failures.push(`C ABI missing ${marker}`);
}
for (const [sdk, marker] of Object.entries(contract.webRtcAdapterMarkers)) {
  if (!sources[sdk].includes(marker)) {
    failures.push(`${sdk} SDK missing WebRTC adapter: ${marker}`);
  }
}
for (const field of contract.trustedGiftFields) {
  for (const sdk of ["web", "rust", "android", "ios"]) {
    const camel = field.replace(/_([a-z])/gu, (_, letter) => letter.toUpperCase());
    if (!sources[sdk].includes(field) && !sources[sdk].includes(camel)) {
      failures.push(`${sdk} trusted gift contract missing ${field}`);
    }
  }
}
for (const sdk of ["web", "rust", "android", "ios"]) {
  if (!sources[sdk].includes(contract.protocol.dataChannelLabel)) {
    failures.push(`${sdk} missing DataChannel label ${contract.protocol.dataChannelLabel}`);
  }
}
const transportHardeningMarkers = {
  web: [
    'redirect: "error"',
    "MAX_JSON_RESPONSE_BYTES",
    "MAX_ERROR_RESPONSE_BYTES",
    "validateAccessToken",
    "parsed.username",
  ],
  rust: [
    "Policy::none()",
    "MAX_JSON_RESPONSE_BYTES",
    "MAX_ERROR_RESPONSE_BYTES",
    "valid_access_token",
    "parsed.username()",
  ],
  android: [
    "instanceFollowRedirects = false",
    "endpoint.toString() + path",
    "MAX_JSON_RESPONSE_BYTES",
    "MAX_ERROR_RESPONSE_BYTES",
    "requireValidAccessToken",
    "endpoint.rawUserInfo",
  ],
  ios: [
    "RedirectRejectingURLSessionDelegate",
    "URL(string: baseURL + path)",
    "maxJSONResponseBytes",
    "maxErrorResponseBytes",
    "isValidAccessToken",
    "baseURL.user",
  ],
};
for (const [sdk, markers] of Object.entries(transportHardeningMarkers)) {
  for (const marker of markers) {
    if (!sources[sdk].includes(marker)) {
      failures.push(`${sdk} SDK transport hardening missing: ${marker}`);
    }
  }
}
const realtimeAndFfiHardeningMarkers = {
  web: [
    "MAX_REALTIME_MESSAGE_BYTES = 16 * 1_024",
    "MAX_REALTIME_PAYLOAD_BYTES",
    "validateRealtimeMessageSize(value.size)",
    "use sendRoomData for the authoritative",
  ],
  protocol: ["pub const ENVELOPE_HEADER_BYTES"],
  mediaNode: [
    "MAX_DATA_CHANNEL_MESSAGE_BYTES: usize = 16 * 1_024",
    "MAX_DATA_CHANNEL_MESSAGE_BYTES - ENVELOPE_HEADER_BYTES",
  ],
  c: [
    "FLUVORA_MAX_BASE_URL_BYTES",
    "FLUVORA_MAX_STRUCTURED_INPUT_BYTES",
  ],
  cImpl: ["MAX_STRUCTURED_INPUT_BYTES", "read_c_string(value, maximum_bytes)"],
};
for (const [component, markers] of Object.entries(realtimeAndFfiHardeningMarkers)) {
  for (const marker of markers) {
    if (!sources[component].includes(marker)) {
      failures.push(`${component} realtime/FFI hardening missing: ${marker}`);
    }
  }
}
const expectedPayloadLimits = {
  jsonRequestBytes: 1_048_576,
  chatBytes: 4_096,
  customNamespaceBytes: 64,
  customPayloadBytes: 61_440,
  signalPayloadBytes: 65_536,
  signalPageMessages: 128,
  signalBacklogMessagesPerRoom: 128,
  signalBroadcastMessagesPerRoom: 128,
  signalCacheBytesPerRoom: 8_388_608,
  sdpBytes: 262_144,
  mediaUploadBytes: 8_388_608,
};
for (const [name, expected] of Object.entries(expectedPayloadLimits)) {
  if (contract.payloadLimits?.[name] !== expected) {
    failures.push(`payload limit ${name} must be ${expected}`);
  }
}
const expectedPersistenceLimits = {
  roomSnapshotBytes: 33_554_432,
  roomEventBytes: 1_048_576,
  commandHistoryEntries: 4_096,
  giftTransactionHistoryEntries: 4_096,
  deliveredOutboxRetentionHours: 168,
  deliveredOutboxCleanupBatch: 10_000,
};
for (const [name, expected] of Object.entries(expectedPersistenceLimits)) {
  if (contract.persistenceLimits?.[name] !== expected) {
    failures.push(`persistence limit ${name} must be ${expected}`);
  }
}
const controlPayloadHardeningMarkers = {
  apiApp: ["DefaultBodyLimit::max(MAX_JSON_REQUEST_BYTES)"],
  apiRoutesWebRtc: ["MAX_SDP_BODY_BYTES"],
  apiGatewayRoutes: [
    "MAX_MEDIA_CONTROL_BODY_BYTES: usize = 8 * 1_024 * 1_024",
    'code: "empty_media_upload"',
    'Some((bytes, "application/octet-stream"))',
    'Some((bytes, "video/mp4"))',
    'Some((bytes, "video/iso.segment"))',
  ],
  apiSignals: [
    "MAX_JSON_REQUEST_BYTES: usize = 1024 * 1024",
    "MAX_SIGNAL_PAGE_MESSAGES: usize = 128",
    "MAX_SIGNAL_CACHE_BYTES: usize = 8 * 1024 * 1024",
    "validate_signal_payload(&payload)",
    "trim_signal_cache(&mut room.signals, &mut room.signal_cache_bytes)",
    "room.signal_cache_bytes.saturating_add(encoded_bytes)",
    "SIGNAL_BACKLOG: usize = MAX_SIGNAL_PAGE_MESSAGES",
  ],
  apiPersistence: [
    "EVENT_CHANNEL_CAPACITY: usize = 128",
    "state: Option<RoomSnapshot>",
    "state: Some(managed.room.snapshot())",
    "events: Vec::new()",
  ],
  controlStore: [
    "SIGNAL_BACKLOG_MESSAGES: u32 = 128",
    "prune_delivered_outbox",
    "InvalidOutboxRetention",
    "MAX_ROOM_SNAPSHOT_BYTES: usize = 32 * 1_024 * 1_024",
    "MAX_ROOM_EVENT_BYTES: usize = 1_024 * 1_024",
  ],
  eventDispatcher: [
    "FLUVORA_OUTBOX_RETENTION_HOURS",
    "FLUVORA_OUTBOX_CLEANUP_BATCH",
    "fluvora_event_dispatcher_pruned_total",
  ],
  outboxCleanup: ["prune_delivered_outbox", "MissedTickBehavior::Skip"],
  domain: [
    "pub const MAX_CHAT_BYTES: usize = 4_096",
    "pub const MAX_CUSTOM_NAMESPACE_BYTES: usize = 64",
    "pub const MAX_CUSTOM_DATA_BYTES: usize = 60 * 1_024",
    "valid_custom_namespace(&data.namespace)",
    "pub struct RoomSnapshot",
    "pub fn restore_snapshot(",
    "GIFT_TRANSACTION_HISTORY_LIMIT: usize = 4_096",
  ],
  web: [
    "MAX_JSON_REQUEST_BYTES",
    "MAX_CHAT_BYTES",
    "MAX_CUSTOM_PAYLOAD_BYTES",
    "MAX_SIGNAL_PAYLOAD_BYTES",
    "MAX_SDP_BYTES",
    "MAX_MEDIA_UPLOAD_BYTES = 8 * 1_024 * 1_024",
    "SIGNAL_PAGE_MESSAGES = 128",
  ],
  rust: [
    "MAX_JSON_REQUEST_BYTES",
    "MAX_CHAT_BYTES",
    "MAX_CUSTOM_PAYLOAD_BYTES",
    "MAX_SIGNAL_PAYLOAD_BYTES",
    "MAX_SDP_BYTES",
    "MAX_MEDIA_UPLOAD_BYTES: usize = 8 * 1_024 * 1_024",
    "SIGNAL_PAGE_MESSAGES: usize = 128",
  ],
  android: [
    "MAX_JSON_REQUEST_BYTES",
    "MAX_CHAT_BYTES",
    "MAX_CUSTOM_PAYLOAD_BYTES",
    "MAX_SIGNAL_PAYLOAD_BYTES",
    "MAX_SDP_BYTES",
    "MAX_MEDIA_UPLOAD_BYTES: Int = 8 * 1_024 * 1_024",
    "SIGNAL_PAGE_MESSAGES: Int = 128",
  ],
  ios: [
    "maxJSONRequestBytes",
    "maxChatBytes",
    "maxCustomPayloadBytes",
    "maxSignalPayloadBytes",
    "maxSDPBytes",
    "maxMediaUploadBytes = 8 * 1_024 * 1_024",
    "signalPageMessages = 128",
  ],
};
for (const [component, markers] of Object.entries(controlPayloadHardeningMarkers)) {
  for (const marker of markers) {
    if (!sources[component].includes(marker)) {
      failures.push(`${component} control payload hardening missing: ${marker}`);
    }
  }
}
for (const sdk of ["web", "rust", "android", "ios"]) {
  if (sources[sdk].includes("limit=500")) {
    failures.push(`${sdk} still requests an oversized 500-message signal page`);
  }
}
if (failures.length > 0) {
  throw new Error(`SDK contract v${contract.version} failed:\n${failures.join("\n")}`);
}
console.log(
  `SDK contract v${contract.version}: ${contract.operations.length} operations, ` +
    `${contract.cAbiMarkers.length} C ABI symbols, native WebRTC adapters, protocol constants, ` +
    `gift schema, bounded transports, control payloads, realtime envelopes, and FFI inputs verified`,
);
