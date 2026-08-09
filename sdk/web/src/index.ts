export type RoomMode = "sfu" | "p2p" | "live" | "vod";
export type MemberRole = "co_host" | "publisher" | "audience";

export interface FluvoraClientOptions {
  baseUrl: string;
  accessToken: string | (() => Promise<string>);
  fetch?: typeof globalThis.fetch;
}

const MAX_BASE_URL_BYTES = 2_048;
const MAX_ACCESS_TOKEN_BYTES = 4_096;
const MAX_JSON_RESPONSE_BYTES = 32 * 1_024 * 1_024;
const MAX_ERROR_RESPONSE_BYTES = 64 * 1_024;
const MAX_JSON_REQUEST_BYTES = 1 * 1_024 * 1_024;
const MAX_CHAT_BYTES = 4_096;
const MAX_CUSTOM_PAYLOAD_BYTES = 60 * 1_024;
const MAX_SIGNAL_PAYLOAD_BYTES = 64 * 1_024;
const MAX_SDP_BYTES = 256 * 1_024;
const MAX_MEDIA_UPLOAD_BYTES = 8 * 1_024 * 1_024;
const SIGNAL_PAGE_MESSAGES = 128;
const REALTIME_ENVELOPE_HEADER_BYTES = 60;
const MAX_REALTIME_MESSAGE_BYTES = 16 * 1_024;
const MAX_REALTIME_PAYLOAD_BYTES = MAX_REALTIME_MESSAGE_BYTES - REALTIME_ENVELOPE_HEADER_BYTES;

export interface Room {
  roomId: string;
  mode: RoomMode;
  sequence: number;
  duplicate: boolean;
}

export interface RoomSnapshot {
  roomId: string;
  mode: RoomMode;
  sequence: number;
  ended: boolean;
  memberCount: number;
  publisherCount: number;
}

export interface CommandResult {
  sequence: number;
  duplicate: boolean;
}

export interface NetworkSample {
  availableOutgoingBitrate?: number;
  roundTripTimeMs?: number;
  packetLossRatio: number;
  quality: "good" | "constrained" | "critical";
}

export interface SfuConnectOptions {
  localStream?: MediaStream;
  receiveAudio?: boolean;
  receiveVideo?: boolean;
  rtcConfiguration?: RTCConfiguration;
  fallbackHlsUrl?: string;
  onRemoteTrack?: (event: RTCTrackEvent) => void;
  onNetworkSample?: (sample: NetworkSample) => void;
  onFallback?: (url: string) => void;
  dataChannel?: false | DataChannelOptions;
}

export interface DataChannelOptions {
  label?: string;
  protocol?: string;
  onOpen?: () => void;
  onClose?: () => void;
  onMessage?: (event: MessageEvent) => void;
  onRoomEnvelope?: (envelope: RealtimeEnvelope) => void;
  onError?: (error: Error) => void;
}

export type RealtimeDataKind = "chat" | "control" | { custom: number };

export interface RealtimeEnvelope {
  kind: "presence" | "chat" | "gift" | "control" | number;
  reliable: boolean;
  ordered: boolean;
  acknowledgementRequired: boolean;
  roomId: string;
  senderId: string;
  sequence: bigint;
  timestampMillis: bigint;
  payload: Uint8Array;
}

export interface IceConfiguration {
  iceServers: RTCIceServer[];
  expiresAtMillis: number;
}

export interface SignalRecord {
  sequence: number;
  from: string;
  to?: string;
  kind: "offer" | "answer" | "ice-candidate" | "ice-restart" | "renegotiate" | "bye";
  payload: unknown;
  timestamp_millis: number;
}

export interface TrackEncoding {
  ssrc: number;
  rid?: string;
  spatialLayer: number;
  maxBitrateBps: number;
}

export interface HeaderExtensionRewrite {
  sourceId: number;
  destinationId?: number;
  replacement?: Uint8Array | readonly number[];
}

export interface PublishTrack {
  trackId: number;
  kind: "audio" | "video";
  codec: "opus" | "vp8" | "vp9" | "h264" | "av1";
  clockRate: number;
  payloadType: number;
  encodings: readonly TrackEncoding[];
  width?: number;
  height?: number;
  framesPerSecond?: number;
}

export interface SubscribeTrack {
  subscriptionId: number;
  trackId: number;
  outputSsrc: number;
  outputPayloadType: number;
  spatialLayer: number;
  temporalLayer: number;
  initialSequenceNumber: number;
  initialTimestamp: number;
  extensionRewrites?: readonly HeaderExtensionRewrite[];
  transportWideExtensionId?: number;
  subscriberCodecs?: readonly PublishTrack["codec"][];
  allowTranscoding?: boolean;
  networkQuality?: NetworkSample["quality"];
  hlsFallbackUrl?: string;
  targetWidth?: number;
  targetHeight?: number;
  targetFramesPerSecond?: number;
  targetBitrateBps?: number;
}

export interface SubscribeTrackResult {
  path: "direct" | "transcode" | "hls" | "existing";
  sourceTrackId: number;
  selectedTrackId?: number | undefined;
  codec?: PublishTrack["codec"] | undefined;
  transcodeJobId?: number | undefined;
  fallbackUrl?: string | undefined;
}

export interface Rendition {
  width: number;
  height: number;
  videoBitrateBps: number;
  audioBitrateBps: number;
}

export interface VodAsset {
  assetId: string;
  tenantId: string;
  version: number;
  state:
    | "created"
    | "uploading"
    | "uploaded"
    | "probing"
    | "transcoding"
    | "ready"
    | "failed"
    | "deleting"
    | "deleted";
  receivedBytes?: number;
  sourceBytes?: number;
  manifestUrl?: string;
  durationMillis?: number;
  failureReason?: string;
  retryable?: boolean;
  jobId?: number;
}

export interface LiveOutput {
  streamId: string;
  nextSequence: number;
  manifestUrl: string;
  workerJobId?: number;
}

export interface LiveSourceTrack {
  roomId: string;
  trackId: number;
  kind: "audio" | "video";
  codec: "opus" | "vp8" | "vp9" | "h264" | "av1";
  payloadType: number;
  clockRate: number;
  channels?: number;
  fmtp?: string;
}

interface ApiRoom {
  room_id: string;
  mode: RoomMode;
  sequence: number;
  duplicate: boolean;
}

interface ApiRoomSnapshot {
  room_id: string;
  mode: RoomMode;
  sequence: number;
  ended: boolean;
  member_count: number;
  publisher_count: number;
}

interface OfferAnswer {
  session_id: string;
  answer_sdp: string;
}

interface SignalResponse {
  signals: SignalRecord[];
  latest_sequence: number;
}

export interface EventTicket {
  ticket: string;
  expiresAtMillis: number;
}

interface ApiEventTicket {
  ticket: string;
  expires_at_millis: number;
}

interface ApiVodAsset {
  asset_id: string;
  tenant_id: string;
  version: number;
  state: VodAsset["state"];
  received_bytes?: number;
  source_bytes?: number;
  manifest_url?: string;
  duration_millis?: number;
  failure_reason?: string;
  retryable?: boolean;
  job_id?: number;
}

interface ApiLiveOutput {
  stream_id: string;
  next_sequence: number;
  manifest_url: string;
  worker_job_id?: number;
}

interface ApiIceConfiguration {
  ice_servers: Array<{
    urls: string[];
    username: string;
    credential: string;
  }>;
  expires_at_millis: number;
}

export class FluvoraError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "FluvoraError";
    this.status = status;
    this.code = code;
  }
}

export class FluvoraResponseTooLargeError extends Error {
  readonly limit: number;

  constructor(limit: number) {
    super(`Fluvora response exceeds ${limit} bytes`);
    this.name = "FluvoraResponseTooLargeError";
    this.limit = limit;
  }
}

export class FluvoraClient {
  readonly #baseUrl: string;
  readonly #token: FluvoraClientOptions["accessToken"];
  readonly #fetch: typeof globalThis.fetch;

  constructor(options: FluvoraClientOptions) {
    this.#baseUrl = normalizeBaseUrl(options.baseUrl);
    if (typeof options.accessToken === "string") validateAccessToken(options.accessToken);
    this.#token = options.accessToken;
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  }

  async createRoom(
    mode: RoomMode,
    limits: { maxMembers?: number; maxPublishers?: number } = {},
  ): Promise<Room> {
    const response = await this.#request<ApiRoom>("/v1/rooms", {
      method: "POST",
      idempotent: true,
      body: {
        mode,
        max_members: limits.maxMembers,
        max_publishers: limits.maxPublishers,
      },
    });
    return {
      roomId: response.room_id,
      mode: response.mode,
      sequence: response.sequence,
      duplicate: response.duplicate,
    };
  }

  async getRoom(roomId: string): Promise<RoomSnapshot> {
    const room = await this.#request<ApiRoomSnapshot>(`/v1/rooms/${encodeId(roomId)}`, {
      method: "GET",
    });
    return {
      roomId: room.room_id,
      mode: room.mode,
      sequence: room.sequence,
      ended: room.ended,
      memberCount: room.member_count,
      publisherCount: room.publisher_count,
    };
  }

  async getIceConfiguration(roomId: string): Promise<IceConfiguration> {
    const response = await this.#request<ApiIceConfiguration>(
      `/v1/rooms/${encodeId(roomId)}/ice-servers`,
      { method: "GET" },
    );
    return {
      iceServers: response.ice_servers.map((server) => ({
        urls: server.urls,
        username: server.username,
        credential: server.credential,
      })),
      expiresAtMillis: response.expires_at_millis,
    };
  }

  join(roomId: string): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/join`);
  }

  leave(roomId: string): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/leave`);
  }

  end(roomId: string): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/end`);
  }

  startPublishing(roomId: string): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/publish/start`);
  }

  stopPublishing(roomId: string): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/publish/stop`);
  }

  setRole(roomId: string, userId: string, role: MemberRole): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/roles`, {
      user_id: userId,
      role,
    });
  }

  sendChat(roomId: string, text: string, messageId = randomId()): Promise<CommandResult> {
    if (text.length === 0) throw new TypeError("chat message cannot be empty");
    encodeBoundedUtf8(text, MAX_CHAT_BYTES, "chat message");
    return this.#write(`/v1/rooms/${encodeId(roomId)}/chat`, {
      message_id: messageId,
      text,
    });
  }

  sendCustomData(
    roomId: string,
    namespace: string,
    schemaVersion: number,
    payload: unknown,
  ): Promise<CommandResult> {
    validateCustomNamespace(namespace);
    if (!Number.isInteger(schemaVersion) || schemaVersion < 0 || schemaVersion > 65_535) {
      throw new RangeError("schemaVersion must fit an unsigned 16-bit integer");
    }
    validateJsonBytes(payload, MAX_CUSTOM_PAYLOAD_BYTES, "custom payload");
    return this.#write(`/v1/rooms/${encodeId(roomId)}/custom`, {
      namespace,
      schema_version: schemaVersion,
      payload,
    });
  }

  recordVerifiedGift(
    roomId: string,
    gift: {
      provider: string;
      providerTimestampMillis: number;
      providerSignature: string;
      senderId: string;
      recipientId: string;
      transactionId: string;
      giftId: string;
      quantity: number;
      unitValue: number;
      currency: string;
    },
  ): Promise<CommandResult> {
    return this.#write(`/v1/rooms/${encodeId(roomId)}/gifts`, {
      provider: gift.provider,
      provider_timestamp_millis: gift.providerTimestampMillis,
      provider_signature: gift.providerSignature,
      sender_id: gift.senderId,
      recipient_id: gift.recipientId,
      transaction_id: gift.transactionId,
      gift_id: gift.giftId,
      quantity: gift.quantity,
      unit_value: gift.unitValue,
      currency: gift.currency,
    });
  }

  async connectSfu(roomId: string, options: SfuConnectOptions = {}): Promise<SfuSession> {
    const rtcConfiguration =
      options.rtcConfiguration ??
      { iceServers: (await this.getIceConfiguration(roomId)).iceServers };
    const peer = new RTCPeerConnection(rtcConfiguration);
    if (options.localStream) {
      for (const track of options.localStream.getTracks()) {
        peer.addTrack(track, options.localStream);
      }
    }
    if (options.receiveAudio ?? true) {
      peer.addTransceiver("audio", { direction: "recvonly" });
    }
    if (options.receiveVideo ?? true) {
      peer.addTransceiver("video", { direction: "recvonly" });
    }
    if (options.onRemoteTrack) {
      peer.addEventListener("track", options.onRemoteTrack);
    }
    const dataOptions = options.dataChannel === false ? undefined : (options.dataChannel ?? {});
    const dataChannel = dataOptions
      ? peer.createDataChannel(dataOptions.label ?? "fluvora.room.v1", {
          ordered: true,
          protocol: dataOptions.protocol ?? "fluvora.v1",
        })
      : undefined;
    if (dataChannel && dataOptions) configureDataChannel(dataChannel, dataOptions);
    try {
      const offer = await peer.createOffer();
      await peer.setLocalDescription(offer);
      await waitForIceGathering(peer, 8_000);
      const localDescription = peer.localDescription;
      if (!localDescription?.sdp) {
        throw new Error("browser did not produce an SDP offer");
      }
      encodeBoundedUtf8(localDescription.sdp, MAX_SDP_BYTES, "SDP offer");
      const answer = await this.#request<OfferAnswer>(
        `/v1/rooms/${encodeId(roomId)}/webrtc/offer`,
        {
          method: "POST",
          body: { sdp: localDescription.sdp },
        },
      );
      await peer.setRemoteDescription({ type: "answer", sdp: answer.answer_sdp });
      return new SfuSession(answer.session_id, peer, options, dataChannel);
    } catch (error) {
      peer.close();
      throw error;
    }
  }

  createP2pSession(
    roomId: string,
    participantId: string,
    peer: RTCPeerConnection,
  ): P2pSession {
    return new P2pSession(this, roomId, participantId, peer);
  }

  async postSignal(
    roomId: string,
    signal: { to?: string; kind: SignalRecord["kind"]; payload: unknown },
  ): Promise<SignalRecord> {
    validateJsonBytes(signal.payload, MAX_SIGNAL_PAYLOAD_BYTES, "signal payload");
    return this.#request<SignalRecord>(`/v1/rooms/${encodeId(roomId)}/signals`, {
      method: "POST",
      idempotent: true,
      body: signal,
    });
  }

  async pollSignals(roomId: string, after: number, signal?: AbortSignal): Promise<SignalResponse> {
    const options: { method: "GET"; signal?: AbortSignal } = { method: "GET" };
    if (signal) options.signal = signal;
    return this.#request<SignalResponse>(
      `/v1/rooms/${encodeId(roomId)}/signals?after=${after}&limit=${SIGNAL_PAGE_MESSAGES}`,
      options,
    );
  }

  async issueEventTicket(roomId: string): Promise<EventTicket> {
    const response = await this.#request<ApiEventTicket>(
      `/v1/rooms/${encodeId(roomId)}/events/tickets`,
      { method: "POST" },
    );
    return {
      ticket: response.ticket,
      expiresAtMillis: response.expires_at_millis,
    };
  }

  async openEventStream(roomId: string, after = 0): Promise<WebSocket> {
    if (!Number.isSafeInteger(after) || after < 0) {
      throw new TypeError("event cursor must be a non-negative safe integer");
    }
    const ticket = await this.issueEventTicket(roomId);
    const endpoint = new URL(
      `${this.#baseUrl}/v1/rooms/${encodeId(roomId)}/events`,
    );
    endpoint.protocol = endpoint.protocol === "https:" ? "wss:" : "ws:";
    endpoint.searchParams.set("ticket", ticket.ticket);
    endpoint.searchParams.set("after", String(after));
    return new WebSocket(endpoint);
  }

  async publishTrack(roomId: string, track: PublishTrack): Promise<void> {
    await this.#request<void>(`/v1/rooms/${encodeId(roomId)}/tracks`, {
      method: "POST",
      idempotent: true,
      body: {
        track_id: track.trackId,
        kind: track.kind,
        codec: track.codec,
        clock_rate: track.clockRate,
        payload_type: track.payloadType,
        width: track.width,
        height: track.height,
        frames_per_second: track.framesPerSecond,
        encodings: track.encodings.map((encoding) => ({
          ssrc: encoding.ssrc,
          rid: encoding.rid,
          spatial_layer: encoding.spatialLayer,
          max_bitrate_bps: encoding.maxBitrateBps,
        })),
      },
    });
  }

  async unpublishTrack(roomId: string, trackId: number): Promise<void> {
    await this.#request<void>(
      `/v1/rooms/${encodeId(roomId)}/tracks/${encodeNumericId(trackId)}`,
      { method: "DELETE" },
    );
  }

  async subscribeTrack(
    roomId: string,
    subscription: SubscribeTrack,
  ): Promise<SubscribeTrackResult> {
    const response = await this.#request<{
      path: SubscribeTrackResult["path"];
      source_track_id: number;
      selected_track_id?: number;
      codec?: PublishTrack["codec"];
      transcode_job_id?: number;
      fallback_url?: string;
    }>(`/v1/rooms/${encodeId(roomId)}/subscriptions`, {
      method: "POST",
      idempotent: true,
      body: {
        subscription_id: subscription.subscriptionId,
        track_id: subscription.trackId,
        output_ssrc: subscription.outputSsrc,
        output_payload_type: subscription.outputPayloadType,
        spatial_layer: subscription.spatialLayer,
        temporal_layer: subscription.temporalLayer,
        initial_sequence_number: subscription.initialSequenceNumber,
        initial_timestamp: subscription.initialTimestamp,
        extension_rewrites: (subscription.extensionRewrites ?? []).map((rewrite) => ({
          source_id: rewrite.sourceId,
          destination_id: rewrite.destinationId,
          replacement:
            rewrite.replacement === undefined ? undefined : Array.from(rewrite.replacement),
        })),
        transport_wide_extension_id: subscription.transportWideExtensionId,
        subscriber_codecs: subscription.subscriberCodecs,
        allow_transcoding: subscription.allowTranscoding ?? false,
        network_quality: subscription.networkQuality,
        hls_fallback_url: subscription.hlsFallbackUrl,
        target_width: subscription.targetWidth,
        target_height: subscription.targetHeight,
        target_frames_per_second: subscription.targetFramesPerSecond,
        target_bitrate_bps: subscription.targetBitrateBps,
      },
    });
    return {
      path: response.path,
      sourceTrackId: response.source_track_id,
      selectedTrackId: response.selected_track_id,
      codec: response.codec,
      transcodeJobId: response.transcode_job_id,
      fallbackUrl: response.fallback_url,
    };
  }

  async unsubscribeTrack(roomId: string, subscriptionId: number): Promise<void> {
    await this.#request<void>(
      `/v1/rooms/${encodeId(roomId)}/subscriptions/${encodeNumericId(subscriptionId)}`,
      { method: "DELETE" },
    );
  }

  async setSubscriptionLayer(
    roomId: string,
    subscriptionId: number,
    spatialLayer: number,
    temporalLayer: number,
  ): Promise<void> {
    await this.#request<void>(
      `/v1/rooms/${encodeId(roomId)}/subscriptions/${encodeNumericId(subscriptionId)}/layer`,
      {
        method: "POST",
        idempotent: true,
        body: {
          spatial_layer: spatialLayer,
          temporal_layer: temporalLayer,
        },
      },
    );
  }

  async createAsset(assetId: string, tenantId: string): Promise<VodAsset> {
    validateMediaId(assetId);
    validateMediaId(tenantId);
    const asset = await this.#request<ApiVodAsset>("/v1/assets", {
      method: "POST",
      idempotent: true,
      body: { asset_id: assetId, tenant_id: tenantId },
    });
    return mapAsset(asset);
  }

  async getAsset(assetId: string): Promise<VodAsset> {
    validateMediaId(assetId);
    const asset = await this.#request<ApiVodAsset>(
      `/v1/assets/${encodeURIComponent(assetId)}`,
      { method: "GET" },
    );
    return mapAsset(asset);
  }

  async deleteAsset(assetId: string): Promise<void> {
    validateMediaId(assetId);
    await this.#request<void>(`/v1/assets/${encodeURIComponent(assetId)}`, {
      method: "DELETE",
      idempotent: true,
    });
  }

  async uploadAssetChunk(
    assetId: string,
    offset: number,
    bytes: Uint8Array,
  ): Promise<VodAsset> {
    validateMediaId(assetId);
    if (!Number.isSafeInteger(offset) || offset < 0) {
      throw new TypeError("upload offset must be a non-negative safe integer");
    }
    validateMediaUpload(bytes, "upload chunk");
    const asset = await this.#request<ApiVodAsset>(
      `/v1/assets/${encodeURIComponent(assetId)}/source?offset=${offset}`,
      {
        method: "PATCH",
        rawBody: bytes,
        contentType: "application/octet-stream",
      },
    );
    return mapAsset(asset);
  }

  async completeAsset(
    assetId: string,
    sourceBytes: number,
    renditions: readonly Rendition[],
    segmentDurationMillis = 4_000,
  ): Promise<VodAsset> {
    validateMediaId(assetId);
    const asset = await this.#request<ApiVodAsset>(
      `/v1/assets/${encodeURIComponent(assetId)}/complete`,
      {
        method: "POST",
        idempotent: true,
        body: {
          source_bytes: sourceBytes,
          segment_duration_millis: segmentDurationMillis,
          renditions: renditions.map((rendition) => ({
            width: rendition.width,
            height: rendition.height,
            video_bitrate_bps: rendition.videoBitrateBps,
            audio_bitrate_bps: rendition.audioBitrateBps,
          })),
        },
      },
    );
    return mapAsset(asset);
  }

  async createLiveOutput(
    streamId: string,
    windowSegments = 6,
    firstSequence = 0,
  ): Promise<LiveOutput> {
    validateMediaId(streamId);
    const output = await this.#request<ApiLiveOutput>(
      `/v1/live/${encodeURIComponent(streamId)}`,
      {
        method: "POST",
        idempotent: true,
        body: {
          window_segments: windowSegments,
          first_sequence: firstSequence,
        },
      },
    );
    return mapLiveOutput(output);
  }

  async createLiveOutputFromTracks(
    streamId: string,
    sourceTracks: readonly LiveSourceTrack[],
    options: {
      windowSegments?: number;
      firstSequence?: number;
      segmentDurationMillis?: number;
      renditions?: readonly Rendition[];
    } = {},
  ): Promise<LiveOutput> {
    validateMediaId(streamId);
    const output = await this.#request<ApiLiveOutput>(
      `/v1/live/${encodeURIComponent(streamId)}`,
      {
        method: "POST",
        idempotent: true,
        body: {
          window_segments: options.windowSegments ?? 6,
          first_sequence: options.firstSequence ?? 0,
          segment_duration_millis: options.segmentDurationMillis ?? 4_000,
          source_tracks: sourceTracks.map((track) => ({
            room_id: encodeId(track.roomId),
            track_id: track.trackId,
            kind: track.kind,
            codec: track.codec,
            payload_type: track.payloadType,
            clock_rate: track.clockRate,
            channels: track.channels,
            fmtp: track.fmtp,
          })),
          renditions: (options.renditions ?? []).map((rendition) => ({
            width: rendition.width,
            height: rendition.height,
            video_bitrate_bps: rendition.videoBitrateBps,
            audio_bitrate_bps: rendition.audioBitrateBps,
          })),
        },
      },
    );
    return mapLiveOutput(output);
  }

  async createLiveAbrOutputFromTracks(
    streamId: string,
    sourceTracks: readonly LiveSourceTrack[],
    renditions: readonly Rendition[],
    options: {
      windowSegments?: number;
      firstSequence?: number;
      segmentDurationMillis?: number;
    } = {},
  ): Promise<LiveOutput> {
    if (renditions.length === 0) {
      throw new TypeError("live ABR requires at least one rendition");
    }
    return this.createLiveOutputFromTracks(streamId, sourceTracks, {
      ...options,
      renditions,
    });
  }

  async getLiveOutput(streamId: string): Promise<LiveOutput> {
    validateMediaId(streamId);
    const output = await this.#request<ApiLiveOutput>(
      `/v1/live/${encodeURIComponent(streamId)}`,
      { method: "GET" },
    );
    return mapLiveOutput(output);
  }

  async deleteLiveOutput(streamId: string): Promise<void> {
    validateMediaId(streamId);
    await this.#request<void>(`/v1/live/${encodeURIComponent(streamId)}`, {
      method: "DELETE",
      idempotent: true,
    });
  }

  async uploadLiveInit(streamId: string, bytes: Uint8Array): Promise<void> {
    validateMediaId(streamId);
    validateMediaUpload(bytes, "initialization segment");
    await this.#request<void>(`/v1/live/${encodeURIComponent(streamId)}/init`, {
      method: "PUT",
      rawBody: bytes,
      contentType: "video/mp4",
    });
  }

  async uploadLiveSegment(
    streamId: string,
    sequence: number,
    durationMillis: number,
    bytes: Uint8Array,
    options: { discontinuity?: boolean; programDateTime?: string } = {},
  ): Promise<LiveOutput> {
    validateMediaId(streamId);
    validateMediaUpload(bytes, "media segment");
    const query = new URLSearchParams({
      duration_millis: String(durationMillis),
    });
    if (options.discontinuity) query.set("discontinuity", "true");
    if (options.programDateTime) query.set("program_date_time", options.programDateTime);
    const output = await this.#request<ApiLiveOutput>(
      `/v1/live/${encodeURIComponent(streamId)}/segments/${encodeNumericId(sequence)}?${query}`,
      {
        method: "PUT",
        rawBody: bytes,
        contentType: "video/iso.segment",
      },
    );
    return mapLiveOutput(output);
  }

  async finishLiveOutput(streamId: string): Promise<void> {
    validateMediaId(streamId);
    await this.#request<void>(`/v1/live/${encodeURIComponent(streamId)}/finish`, {
      method: "POST",
      idempotent: true,
    });
  }

  async #write(path: string, body?: unknown): Promise<CommandResult> {
    return this.#request<CommandResult>(path, {
      method: "POST",
      idempotent: true,
      body,
    });
  }

  async #request<T>(
    path: string,
    options: {
      method: "GET" | "POST" | "PATCH" | "PUT" | "DELETE";
      body?: unknown;
      rawBody?: Uint8Array;
      contentType?: string;
      idempotent?: boolean;
      signal?: AbortSignal;
    },
  ): Promise<T> {
    const token = typeof this.#token === "string" ? this.#token : await this.#token();
    validateAccessToken(token);
    const headers = new Headers({
      Accept: "application/json",
      Authorization: `Bearer ${token}`,
    });
    if (options.body !== undefined || options.rawBody !== undefined) {
      headers.set("Content-Type", options.contentType ?? "application/json");
    }
    if (options.idempotent) {
      headers.set("Idempotency-Key", randomId());
    }
    const init: RequestInit = {
      method: options.method,
      headers,
      redirect: "error",
    };
    if (options.body !== undefined) {
      const body = JSON.stringify(options.body);
      if (body === undefined) throw new TypeError("JSON request body is not serializable");
      encodeBoundedUtf8(body, MAX_JSON_REQUEST_BYTES, "JSON request body");
      init.body = body;
    } else if (options.rawBody !== undefined) {
      const owned = new Uint8Array(options.rawBody.byteLength);
      owned.set(options.rawBody);
      init.body = owned.buffer;
    }
    if (options.signal) {
      init.signal = options.signal;
    }
    const response = await this.#fetch(`${this.#baseUrl}${path}`, init);
    if (!response.ok) {
      const responseBody = await readBoundedResponse(response, MAX_ERROR_RESPONSE_BYTES);
      const body = parseJsonObject(responseBody);
      throw new FluvoraError(
        response.status,
        typeof body.code === "string" ? body.code : "http_error",
        typeof body.message === "string"
          ? body.message
          : `Fluvora request failed with ${response.status}`,
      );
    }
    const responseBody = await readBoundedResponse(response, MAX_JSON_RESPONSE_BYTES);
    if (responseBody.length === 0) {
      return undefined as T;
    }
    return JSON.parse(responseBody) as T;
  }
}

function normalizeBaseUrl(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new TypeError("baseUrl must be an absolute HTTP(S) URL");
  }
  if (
    bytes.byteLength === 0 ||
    bytes.byteLength > MAX_BASE_URL_BYTES ||
    /[\u0000-\u001f\u007f]/u.test(value) ||
    (parsed.protocol !== "http:" && parsed.protocol !== "https:") ||
    parsed.hostname.length === 0 ||
    parsed.username.length > 0 ||
    parsed.password.length > 0 ||
    parsed.search.length > 0 ||
    parsed.hash.length > 0
  ) {
    throw new TypeError("baseUrl must be an uncredentialed HTTP(S) URL without query or fragment");
  }
  return parsed.href.replace(/\/+$/u, "");
}

function validateAccessToken(value: string): void {
  const byteLength = new TextEncoder().encode(value).byteLength;
  if (
    byteLength === 0 ||
    byteLength > MAX_ACCESS_TOKEN_BYTES ||
    /[\u0000-\u001f\u007f]/u.test(value)
  ) {
    throw new TypeError("accessToken must be 1-4096 bytes without control characters");
  }
}

function validateCustomNamespace(value: string): void {
  if (!/^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,62}[A-Za-z0-9])?$/u.test(value)) {
    throw new TypeError("namespace must contain 1..64 safe ASCII characters");
  }
}

function validateJsonBytes(value: unknown, maximumBytes: number, label: string): void {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) throw new TypeError(`${label} is not JSON serializable`);
  encodeBoundedUtf8(encoded, maximumBytes, label);
}

function parseJsonObject(value: string): Record<string, unknown> {
  if (value.length === 0) return {};
  try {
    const parsed = JSON.parse(value) as unknown;
    if (typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
    return {};
  } catch {
    return {};
  }
}

async function readBoundedResponse(response: Response, limit: number): Promise<string> {
  const contentLength = response.headers.get("Content-Length");
  if (contentLength !== null && /^\d+$/u.test(contentLength)) {
    if (BigInt(contentLength) > BigInt(limit)) {
      throw new FluvoraResponseTooLargeError(limit);
    }
  }
  if (response.body === null) return "";

  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (value === undefined) continue;
    byteLength += value.byteLength;
    if (byteLength > limit) {
      try {
        await reader.cancel();
      } catch {
        // The size violation is the actionable error even if cancellation also fails.
      }
      throw new FluvoraResponseTooLargeError(limit);
    }
    chunks.push(value);
  }

  const bytes = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

export class SfuSession {
  readonly sessionId: string;
  readonly peerConnection: RTCPeerConnection;
  readonly dataChannel?: RTCDataChannel;
  #timer: number | undefined;
  #previousPackets = 0;
  #previousLost = 0;
  #criticalSamples = 0;

  constructor(
    sessionId: string,
    peer: RTCPeerConnection,
    options: SfuConnectOptions,
    dataChannel?: RTCDataChannel,
  ) {
    this.sessionId = sessionId;
    this.peerConnection = peer;
    if (dataChannel) this.dataChannel = dataChannel;
    this.#timer = globalThis.setInterval(() => {
      void this.#adapt(options).catch(() => undefined);
    }, 2_000);
  }

  close(): void {
    if (this.#timer !== undefined) {
      globalThis.clearInterval(this.#timer);
      this.#timer = undefined;
    }
    this.peerConnection.close();
  }

  sendData(data: string | Uint8Array | ArrayBuffer): void {
    const channel = this.requireOpenDataChannel();
    if (channel.label === "fluvora.room.v1") {
      throw new Error("use sendRoomData for the authoritative fluvora.room.v1 channel");
    }
    if (typeof data === "string") {
      encodeBoundedUtf8(data, MAX_REALTIME_MESSAGE_BYTES, "DataChannel message");
      channel.send(data);
      return;
    }
    const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
    validateRealtimeMessageSize(bytes.byteLength);
    const owned = new Uint8Array(bytes.byteLength);
    owned.set(bytes);
    channel.send(owned.buffer);
  }

  sendRoomData(
    kind: RealtimeDataKind,
    payload: string | Uint8Array,
    options: { acknowledgementRequired?: boolean } = {},
  ): void {
    const channel = this.requireOpenDataChannel();
    if (channel.label !== "fluvora.room.v1") {
      throw new Error("authoritative room envelopes require the fluvora.room.v1 channel");
    }
    const payloadBytes =
      typeof payload === "string"
        ? encodeBoundedUtf8(payload, MAX_REALTIME_PAYLOAD_BYTES, "realtime payload")
        : payload;
    channel.send(
      encodeRealtimeEnvelope(kind, payloadBytes, options.acknowledgementRequired ?? false),
    );
  }

  #requireDataChannel(): RTCDataChannel {
    if (!this.dataChannel) throw new Error("this SFU session has no data channel");
    return this.dataChannel;
  }

  requireOpenDataChannel(): RTCDataChannel {
    const channel = this.#requireDataChannel();
    if (channel.readyState !== "open") {
      throw new Error(`data channel is ${channel.readyState}`);
    }
    return channel;
  }

  async #adapt(options: SfuConnectOptions): Promise<void> {
    if (this.peerConnection.connectionState === "closed") {
      this.close();
      return;
    }
    const stats = await this.peerConnection.getStats();
    let packets = 0;
    let lost = 0;
    let remotePacketLossRatio = 0;
    let availableOutgoingBitrate: number | undefined;
    let roundTripTimeMs: number | undefined;
    stats.forEach((report) => {
      if (report.type === "inbound-rtp" && report.kind === "video") {
        packets += Number(report.packetsReceived ?? 0);
        lost += Number(report.packetsLost ?? 0);
      } else if (report.type === "remote-inbound-rtp" && report.kind === "video") {
        if (typeof report.fractionLost === "number") {
          remotePacketLossRatio = Math.max(remotePacketLossRatio, report.fractionLost);
        }
        if (typeof report.roundTripTime === "number") {
          roundTripTimeMs = Math.max(roundTripTimeMs ?? 0, report.roundTripTime * 1_000);
        }
      } else if (report.type === "candidate-pair" && report.state === "succeeded") {
        if (typeof report.availableOutgoingBitrate === "number") {
          availableOutgoingBitrate = report.availableOutgoingBitrate;
        }
        if (typeof report.currentRoundTripTime === "number") {
          roundTripTimeMs = report.currentRoundTripTime * 1_000;
        }
      }
    });
    const packetDelta = Math.max(0, packets - this.#previousPackets);
    const lostDelta = Math.max(0, lost - this.#previousLost);
    this.#previousPackets = packets;
    this.#previousLost = lost;
    const denominator = packetDelta + lostDelta;
    const packetLossRatio = Math.max(
      remotePacketLossRatio,
      denominator === 0 ? 0 : lostDelta / denominator,
    );
    const quality =
      packetLossRatio > 0.15 || (roundTripTimeMs ?? 0) > 800
        ? "critical"
        : packetLossRatio > 0.05 || (roundTripTimeMs ?? 0) > 300
          ? "constrained"
          : "good";
    const sample: NetworkSample = { packetLossRatio, quality };
    if (availableOutgoingBitrate !== undefined) {
      sample.availableOutgoingBitrate = availableOutgoingBitrate;
    }
    if (roundTripTimeMs !== undefined) {
      sample.roundTripTimeMs = roundTripTimeMs;
    }
    options.onNetworkSample?.(sample);
    await adaptSenders(this.peerConnection, quality, availableOutgoingBitrate);
    this.#criticalSamples = quality === "critical" ? this.#criticalSamples + 1 : 0;
    if (this.#criticalSamples >= 3 && options.fallbackHlsUrl) {
      options.onFallback?.(options.fallbackHlsUrl);
      this.#criticalSamples = 0;
    }
  }
}

export class P2pSession {
  readonly #client: FluvoraClient;
  readonly #roomId: string;
  readonly #participantId: string;
  readonly #peer: RTCPeerConnection;
  #after = 0;
  #abort = new AbortController();
  #running = false;
  #remoteParticipantId?: string;
  #pendingCandidates: RTCIceCandidateInit[] = [];

  constructor(
    client: FluvoraClient,
    roomId: string,
    participantId: string,
    peer: RTCPeerConnection,
  ) {
    this.#client = client;
    this.#roomId = roomId;
    this.#participantId = participantId;
    this.#peer = peer;
    peer.addEventListener("icecandidate", (event) => {
      if (event.candidate) {
        const signal: {
          to?: string;
          kind: SignalRecord["kind"];
          payload: RTCIceCandidateInit;
        } = {
          kind: "ice-candidate",
          payload: event.candidate.toJSON(),
        };
        if (this.#remoteParticipantId) signal.to = this.#remoteParticipantId;
        void client.postSignal(roomId, signal);
      }
    });
  }

  async offer(to: string): Promise<void> {
    this.#remoteParticipantId = to;
    const offer = await this.#peer.createOffer();
    await this.#peer.setLocalDescription(offer);
    await this.#client.postSignal(this.#roomId, {
      to,
      kind: "offer",
      payload: offer,
    });
  }

  async restartIce(to: string): Promise<void> {
    this.#remoteParticipantId = to;
    this.#peer.restartIce();
    const offer = await this.#peer.createOffer({ iceRestart: true });
    await this.#peer.setLocalDescription(offer);
    await this.#client.postSignal(this.#roomId, {
      to,
      kind: "ice-restart",
      payload: offer,
    });
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    void this.#pollLoop();
  }

  async hangup(): Promise<void> {
    const signal: {
      to?: string;
      kind: SignalRecord["kind"];
      payload: Record<string, never>;
    } = { kind: "bye", payload: {} };
    if (this.#remoteParticipantId) signal.to = this.#remoteParticipantId;
    await this.#client.postSignal(this.#roomId, signal);
    this.close();
  }

  close(): void {
    this.#running = false;
    this.#abort.abort();
    this.#peer.close();
  }

  async #pollLoop(): Promise<void> {
    while (this.#running) {
      const response = await this.#client.pollSignals(
        this.#roomId,
        this.#after,
        this.#abort.signal,
      );
      for (const signal of response.signals) {
        this.#after = Math.max(this.#after, signal.sequence);
        await this.#apply(signal);
      }
      if (response.signals.length === 0) {
        await delay(500, this.#abort.signal);
      }
    }
  }

  async #apply(signal: SignalRecord): Promise<void> {
    if (signal.from === this.#participantId) return;
    if (signal.kind === "offer" || signal.kind === "ice-restart" || signal.kind === "renegotiate") {
      this.#remoteParticipantId = signal.from;
      await this.#peer.setRemoteDescription(signal.payload as RTCSessionDescriptionInit);
      await this.#flushCandidates();
      const answer = await this.#peer.createAnswer();
      await this.#peer.setLocalDescription(answer);
      await this.#client.postSignal(this.#roomId, {
        to: signal.from,
        kind: "answer",
        payload: answer,
      });
    } else if (signal.kind === "answer") {
      this.#remoteParticipantId = signal.from;
      await this.#peer.setRemoteDescription(signal.payload as RTCSessionDescriptionInit);
      await this.#flushCandidates();
    } else if (signal.kind === "ice-candidate") {
      const candidate = signal.payload as RTCIceCandidateInit;
      if (this.#peer.remoteDescription) {
        await this.#peer.addIceCandidate(candidate);
      } else {
        if (this.#pendingCandidates.length >= 256) this.#pendingCandidates.shift();
        this.#pendingCandidates.push(candidate);
      }
    } else if (signal.kind === "bye") {
      this.close();
    }
  }

  async #flushCandidates(): Promise<void> {
    const candidates = this.#pendingCandidates;
    this.#pendingCandidates = [];
    for (const candidate of candidates) {
      await this.#peer.addIceCandidate(candidate);
    }
  }
}

async function adaptSenders(
  peer: RTCPeerConnection,
  quality: NetworkSample["quality"],
  availableBitrate?: number,
): Promise<void> {
  for (const sender of peer.getSenders()) {
    if (sender.track?.kind !== "video") continue;
    const parameters = sender.getParameters();
    if (!parameters.encodings.length) parameters.encodings = [{}];
    const budget = Math.max(100_000, Math.floor((availableBitrate ?? 1_500_000) * 0.8));
    for (const [index, encoding] of parameters.encodings.entries()) {
      encoding.maxBitrate = quality === "critical" ? Math.min(250_000, budget) : budget;
      if (parameters.encodings.length > 1) {
        encoding.active =
          quality === "good" || (quality === "constrained" && index === 0) || index === 0;
      }
    }
    await sender.setParameters(parameters).catch(() => undefined);
  }
}

function encodeId(value: string): string {
  if (!/^[0-9a-f]{1,32}$/iu.test(value)) {
    throw new TypeError("identifier must be hexadecimal");
  }
  return encodeURIComponent(value);
}

function encodeNumericId(value: number): string {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError("numeric identifier must be a non-negative safe integer");
  }
  return String(value);
}

function validateMediaId(value: string): void {
  if (!/^[A-Za-z0-9_-]{1,128}$/u.test(value)) {
    throw new TypeError("media identifier must contain 1..128 safe ASCII characters");
  }
}

function validateMediaUpload(bytes: Uint8Array, label: string): void {
  if (bytes.byteLength === 0) {
    throw new TypeError(`${label} cannot be empty`);
  }
  if (bytes.byteLength > MAX_MEDIA_UPLOAD_BYTES) {
    throw new RangeError(`${label} exceeds ${MAX_MEDIA_UPLOAD_BYTES} bytes`);
  }
}

function mapAsset(asset: ApiVodAsset): VodAsset {
  const mapped: VodAsset = {
    assetId: asset.asset_id,
    tenantId: asset.tenant_id,
    version: asset.version,
    state: asset.state,
  };
  if (asset.received_bytes !== undefined) mapped.receivedBytes = asset.received_bytes;
  if (asset.source_bytes !== undefined) mapped.sourceBytes = asset.source_bytes;
  if (asset.manifest_url !== undefined) mapped.manifestUrl = asset.manifest_url;
  if (asset.duration_millis !== undefined) mapped.durationMillis = asset.duration_millis;
  if (asset.failure_reason !== undefined) mapped.failureReason = asset.failure_reason;
  if (asset.retryable !== undefined) mapped.retryable = asset.retryable;
  if (asset.job_id !== undefined) mapped.jobId = asset.job_id;
  return mapped;
}

function mapLiveOutput(output: ApiLiveOutput): LiveOutput {
  const mapped: LiveOutput = {
    streamId: output.stream_id,
    nextSequence: output.next_sequence,
    manifestUrl: output.manifest_url,
  };
  if (output.worker_job_id !== undefined) mapped.workerJobId = output.worker_job_id;
  return mapped;
}

function randomId(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function waitForIceGathering(peer: RTCPeerConnection, timeoutMs: number): Promise<void> {
  if (peer.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const timeout = globalThis.setTimeout(done, timeoutMs);
    peer.addEventListener("icegatheringstatechange", onChange);
    function onChange(): void {
      if (peer.iceGatheringState === "complete") done();
    }
    function done(): void {
      globalThis.clearTimeout(timeout);
      peer.removeEventListener("icegatheringstatechange", onChange);
      resolve();
    }
  });
}

function configureDataChannel(channel: RTCDataChannel, options: DataChannelOptions): void {
  channel.binaryType = "arraybuffer";
  if (options.onOpen) channel.addEventListener("open", options.onOpen);
  if (options.onClose) channel.addEventListener("close", options.onClose);
  channel.addEventListener("message", (event) => {
    options.onMessage?.(event);
    if (channel.label !== "fluvora.room.v1" || !options.onRoomEnvelope) return;
    void messageBytes(event.data)
      .then((bytes) => options.onRoomEnvelope?.(decodeRealtimeEnvelope(bytes)))
      .catch((error: unknown) => {
        options.onError?.(error instanceof Error ? error : new Error(String(error)));
      });
  });
}

async function messageBytes(value: unknown): Promise<Uint8Array> {
  if (value instanceof ArrayBuffer) {
    validateRealtimeMessageSize(value.byteLength);
    return new Uint8Array(value);
  }
  if (value instanceof Blob) {
    validateRealtimeMessageSize(value.size);
    return new Uint8Array(await value.arrayBuffer());
  }
  if (ArrayBuffer.isView(value)) {
    validateRealtimeMessageSize(value.byteLength);
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new TypeError("fluvora.room.v1 requires binary DataChannel messages");
}

function validateRealtimeMessageSize(byteLength: number): void {
  if (byteLength > MAX_REALTIME_MESSAGE_BYTES) {
    throw new RangeError(`realtime message exceeds ${MAX_REALTIME_MESSAGE_BYTES} bytes`);
  }
}

function encodeBoundedUtf8(value: string, maximumBytes: number, label: string): Uint8Array {
  if (value.length > maximumBytes) {
    throw new RangeError(`${label} exceeds ${maximumBytes} bytes`);
  }
  const encoded = new TextEncoder().encode(value);
  if (encoded.byteLength > maximumBytes) {
    throw new RangeError(`${label} exceeds ${maximumBytes} bytes`);
  }
  return encoded;
}

function encodeRealtimeEnvelope(
  kind: RealtimeDataKind,
  payload: Uint8Array,
  acknowledgementRequired: boolean,
): ArrayBuffer {
  if (payload.byteLength > MAX_REALTIME_PAYLOAD_BYTES) {
    throw new RangeError(`realtime payload exceeds ${MAX_REALTIME_PAYLOAD_BYTES} bytes`);
  }
  const kindCode =
    kind === "chat"
      ? 2
      : kind === "control"
        ? 4
        : validateCustomKind(kind.custom);
  const output = new Uint8Array(REALTIME_ENVELOPE_HEADER_BYTES + payload.byteLength);
  output.set([0x46, 0x4c, 0x55, 0x56, 1, acknowledgementRequired ? 0x07 : 0x03], 0);
  const view = new DataView(output.buffer);
  view.setUint16(6, kindCode);
  view.setUint32(56, payload.byteLength);
  output.set(payload, REALTIME_ENVELOPE_HEADER_BYTES);
  return output.buffer;
}

function decodeRealtimeEnvelope(input: Uint8Array): RealtimeEnvelope {
  if (
    input.byteLength < REALTIME_ENVELOPE_HEADER_BYTES ||
    input.byteLength > MAX_REALTIME_MESSAGE_BYTES ||
    input[0] !== 0x46 ||
    input[1] !== 0x4c ||
    input[2] !== 0x55 ||
    input[3] !== 0x56 ||
    input[4] !== 1
  ) {
    throw new TypeError("invalid Fluvora realtime envelope");
  }
  const flags = input[5] ?? 0;
  if ((flags & ~0x07) !== 0) throw new TypeError("unsupported realtime envelope flags");
  const view = new DataView(input.buffer, input.byteOffset, input.byteLength);
  const payloadLength = view.getUint32(56);
  if (
    payloadLength > MAX_REALTIME_PAYLOAD_BYTES ||
    payloadLength + REALTIME_ENVELOPE_HEADER_BYTES !== input.byteLength
  ) {
    throw new TypeError("invalid realtime envelope payload length");
  }
  const kindCode = view.getUint16(6);
  const payload = new Uint8Array(payloadLength);
  payload.set(input.subarray(REALTIME_ENVELOPE_HEADER_BYTES));
  return {
    kind: decodeRealtimeKind(kindCode),
    reliable: (flags & 0x01) !== 0,
    ordered: (flags & 0x02) !== 0,
    acknowledgementRequired: (flags & 0x04) !== 0,
    roomId: readHexId(view, 8),
    senderId: readHexId(view, 24),
    sequence: view.getBigUint64(40),
    timestampMillis: view.getBigUint64(48),
    payload,
  };
}

function readHexId(view: DataView, offset: number): string {
  return (
    view.getBigUint64(offset).toString(16).padStart(16, "0") +
    view
      .getBigUint64(offset + 8)
      .toString(16)
      .padStart(16, "0")
  ).replace(/^0+(?=[0-9a-f])/u, "");
}

function decodeRealtimeKind(value: number): RealtimeEnvelope["kind"] {
  switch (value) {
    case 1:
      return "presence";
    case 2:
      return "chat";
    case 3:
      return "gift";
    case 4:
      return "control";
    default:
      return value;
  }
}

function validateCustomKind(value: number): number {
  if (!Number.isInteger(value) || value < 0x8000 || value > 0xffff) {
    throw new RangeError("custom realtime kind must be in 0x8000..=0xffff");
  }
  return value;
}

function delay(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timeout = globalThis.setTimeout(resolve, milliseconds);
    signal.addEventListener(
      "abort",
      () => {
        globalThis.clearTimeout(timeout);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}
