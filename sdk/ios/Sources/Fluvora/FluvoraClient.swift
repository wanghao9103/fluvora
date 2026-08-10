import Foundation

private let maxBaseURLBytes = 2_048
private let maxAccessTokenBytes = 4_096
private let maxJSONResponseBytes = 32 * 1_024 * 1_024
private let maxErrorResponseBytes = 64 * 1_024
private let maxJSONRequestBytes = 1 * 1_024 * 1_024
private let maxChatBytes = 4_096
private let maxCustomPayloadBytes = 60 * 1_024
private let maxSignalPayloadBytes = 64 * 1_024
private let maxSDPBytes = 256 * 1_024
private let maxMediaUploadBytes = 8 * 1_024 * 1_024
private let signalPageMessages = 128

private final class RedirectRejectingURLSessionDelegate: NSObject,
    URLSessionTaskDelegate,
    @unchecked Sendable
{
    func urlSession(
        _: URLSession,
        task _: URLSessionTask,
        willPerformHTTPRedirection _: HTTPURLResponse,
        newRequest _: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

private func makeDefaultURLSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 30
    configuration.timeoutIntervalForResource = 30
    return URLSession(
        configuration: configuration,
        delegate: RedirectRejectingURLSessionDelegate(),
        delegateQueue: nil
    )
}

public enum RoomMode: String, Codable, Sendable {
    case sfu
    case p2p
    case live
    case vod
}

public struct Room: Codable, Sendable, Equatable {
    public let roomId: String
    public let mode: RoomMode
    public let sequence: UInt64
    public let duplicate: Bool

    enum CodingKeys: String, CodingKey {
        case roomId = "room_id"
        case mode
        case sequence
        case duplicate
    }
}

public struct RoomSnapshot: Codable, Sendable, Equatable {
    public let roomId: String
    public let mode: RoomMode
    public let sequence: UInt64
    public let ended: Bool
    public let memberCount: Int
    public let publisherCount: Int

    enum CodingKeys: String, CodingKey {
        case roomId = "room_id"
        case mode
        case sequence
        case ended
        case memberCount = "member_count"
        case publisherCount = "publisher_count"
    }
}

public enum MemberRole: String, Codable, Sendable {
    case host
    case publisher
    case viewer
}

public struct VerifiedGift: Codable, Sendable, Equatable {
    public let provider: String
    public let providerTimestampMillis: UInt64
    public let providerSignature: String
    public let senderId: String
    public let recipientId: String
    public let transactionId: String
    public let giftId: String
    public let quantity: UInt32
    public let unitValue: UInt64
    public let currency: String

    public init(
        provider: String,
        providerTimestampMillis: UInt64,
        providerSignature: String,
        senderId: String,
        recipientId: String,
        transactionId: String,
        giftId: String,
        quantity: UInt32,
        unitValue: UInt64,
        currency: String
    ) {
        self.provider = provider
        self.providerTimestampMillis = providerTimestampMillis
        self.providerSignature = providerSignature
        self.senderId = senderId
        self.recipientId = recipientId
        self.transactionId = transactionId
        self.giftId = giftId
        self.quantity = quantity
        self.unitValue = unitValue
        self.currency = currency
    }

    enum CodingKeys: String, CodingKey {
        case provider
        case providerTimestampMillis = "provider_timestamp_millis"
        case providerSignature = "provider_signature"
        case senderId = "sender_id"
        case recipientId = "recipient_id"
        case transactionId = "transaction_id"
        case giftId = "gift_id"
        case quantity
        case unitValue = "unit_value"
        case currency
    }
}

public struct CommandResult: Codable, Sendable, Equatable {
    public let sequence: UInt64
    public let duplicate: Bool
}

public struct WebRTCSession: Codable, Sendable, Equatable {
    public let sessionId: String
    public let answerSDP: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case answerSDP = "answer_sdp"
    }
}

public struct EventTicket: Codable, Sendable, Equatable {
    public let ticket: String
    public let expiresAtMillis: UInt64

    enum CodingKeys: String, CodingKey {
        case ticket
        case expiresAtMillis = "expires_at_millis"
    }
}

public struct IceServer: Codable, Sendable, Equatable {
    public let urls: [String]
    public let username: String
    public let credential: String
}

public struct IceConfiguration: Codable, Sendable, Equatable {
    public let iceServers: [IceServer]
    public let expiresAtMillis: UInt64

    enum CodingKeys: String, CodingKey {
        case iceServers = "ice_servers"
        case expiresAtMillis = "expires_at_millis"
    }
}

public enum JSONValue: Codable, Sendable, Equatable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null:
            try container.encodeNil()
        case .bool(let value):
            try container.encode(value)
        case .number(let value):
            try container.encode(value)
        case .string(let value):
            try container.encode(value)
        case .array(let value):
            try container.encode(value)
        case .object(let value):
            try container.encode(value)
        }
    }
}

public struct Signal: Codable, Sendable, Equatable {
    public let sequence: UInt64
    public let senderId: String
    public let recipientId: String?
    public let kind: String
    public let payload: JSONValue
    public let timestampMillis: UInt64

    enum CodingKeys: String, CodingKey {
        case sequence
        case senderId = "from"
        case recipientId = "to"
        case kind
        case payload
        case timestampMillis = "timestamp_millis"
    }
}

public struct SignalPage: Codable, Sendable, Equatable {
    public let signals: [Signal]
    public let latestSequence: UInt64

    enum CodingKeys: String, CodingKey {
        case signals
        case latestSequence = "latest_sequence"
    }
}

public struct TrackEncoding: Codable, Sendable, Equatable {
    public let ssrc: UInt32
    public let rid: String?
    public let spatialLayer: UInt8
    public let maxBitrateBps: UInt64

    public init(ssrc: UInt32, rid: String? = nil, spatialLayer: UInt8, maxBitrateBps: UInt64) {
        self.ssrc = ssrc
        self.rid = rid
        self.spatialLayer = spatialLayer
        self.maxBitrateBps = maxBitrateBps
    }

    enum CodingKeys: String, CodingKey {
        case ssrc
        case rid
        case spatialLayer = "spatial_layer"
        case maxBitrateBps = "max_bitrate_bps"
    }
}

public struct HeaderExtensionRewrite: Codable, Sendable, Equatable {
    public let sourceId: UInt8
    public let destinationId: UInt8?
    public let replacement: [UInt8]?

    public init(sourceId: UInt8, destinationId: UInt8? = nil, replacement: [UInt8]? = nil) {
        self.sourceId = sourceId
        self.destinationId = destinationId
        self.replacement = replacement
    }

    enum CodingKeys: String, CodingKey {
        case sourceId = "source_id"
        case destinationId = "destination_id"
        case replacement
    }
}

public struct PublishTrack: Codable, Sendable, Equatable {
    public let trackId: UInt64
    public let kind: String
    public let codec: String
    public let clockRate: UInt32
    public let payloadType: UInt8
    public let encodings: [TrackEncoding]
    public let width: UInt16
    public let height: UInt16
    public let framesPerSecond: UInt16

    public init(
        trackId: UInt64,
        kind: String,
        codec: String,
        clockRate: UInt32,
        payloadType: UInt8,
        encodings: [TrackEncoding],
        width: UInt16 = 0,
        height: UInt16 = 0,
        framesPerSecond: UInt16 = 0
    ) {
        self.trackId = trackId
        self.kind = kind
        self.codec = codec
        self.clockRate = clockRate
        self.payloadType = payloadType
        self.encodings = encodings
        self.width = width
        self.height = height
        self.framesPerSecond = framesPerSecond
    }

    enum CodingKeys: String, CodingKey {
        case trackId = "track_id"
        case kind
        case codec
        case clockRate = "clock_rate"
        case payloadType = "payload_type"
        case encodings
        case width
        case height
        case framesPerSecond = "frames_per_second"
    }
}

public struct SubscribeTrack: Codable, Sendable, Equatable {
    public let subscriptionId: UInt64
    public let trackId: UInt64
    public let outputSsrc: UInt32
    public let outputPayloadType: UInt8
    public let spatialLayer: UInt8
    public let temporalLayer: UInt8
    public let initialSequenceNumber: UInt16
    public let initialTimestamp: UInt32
    public let extensionRewrites: [HeaderExtensionRewrite]
    public let transportWideExtensionId: UInt8?
    public let subscriberCodecs: [String]
    public let allowTranscoding: Bool
    public let networkQuality: String?
    public let hlsFallbackUrl: String?
    public let targetWidth: UInt16?
    public let targetHeight: UInt16?
    public let targetFramesPerSecond: UInt16?
    public let targetBitrateBps: UInt64?

    public init(
        subscriptionId: UInt64,
        trackId: UInt64,
        outputSsrc: UInt32,
        outputPayloadType: UInt8,
        spatialLayer: UInt8,
        temporalLayer: UInt8,
        initialSequenceNumber: UInt16,
        initialTimestamp: UInt32,
        extensionRewrites: [HeaderExtensionRewrite] = [],
        transportWideExtensionId: UInt8? = nil,
        subscriberCodecs: [String] = [],
        allowTranscoding: Bool = false,
        networkQuality: String? = nil,
        hlsFallbackUrl: String? = nil,
        targetWidth: UInt16? = nil,
        targetHeight: UInt16? = nil,
        targetFramesPerSecond: UInt16? = nil,
        targetBitrateBps: UInt64? = nil
    ) {
        self.subscriptionId = subscriptionId
        self.trackId = trackId
        self.outputSsrc = outputSsrc
        self.outputPayloadType = outputPayloadType
        self.spatialLayer = spatialLayer
        self.temporalLayer = temporalLayer
        self.initialSequenceNumber = initialSequenceNumber
        self.initialTimestamp = initialTimestamp
        self.extensionRewrites = extensionRewrites
        self.transportWideExtensionId = transportWideExtensionId
        self.subscriberCodecs = subscriberCodecs
        self.allowTranscoding = allowTranscoding
        self.networkQuality = networkQuality
        self.hlsFallbackUrl = hlsFallbackUrl
        self.targetWidth = targetWidth
        self.targetHeight = targetHeight
        self.targetFramesPerSecond = targetFramesPerSecond
        self.targetBitrateBps = targetBitrateBps
    }

    enum CodingKeys: String, CodingKey {
        case subscriptionId = "subscription_id"
        case trackId = "track_id"
        case outputSsrc = "output_ssrc"
        case outputPayloadType = "output_payload_type"
        case spatialLayer = "spatial_layer"
        case temporalLayer = "temporal_layer"
        case initialSequenceNumber = "initial_sequence_number"
        case initialTimestamp = "initial_timestamp"
        case extensionRewrites = "extension_rewrites"
        case transportWideExtensionId = "transport_wide_extension_id"
        case subscriberCodecs = "subscriber_codecs"
        case allowTranscoding = "allow_transcoding"
        case networkQuality = "network_quality"
        case hlsFallbackUrl = "hls_fallback_url"
        case targetWidth = "target_width"
        case targetHeight = "target_height"
        case targetFramesPerSecond = "target_frames_per_second"
        case targetBitrateBps = "target_bitrate_bps"
    }
}

public struct SubscribeTrackResult: Codable, Sendable, Equatable {
    public let path: String
    public let sourceTrackId: UInt64
    public let selectedTrackId: UInt64?
    public let codec: String?
    public let transcodeJobId: UInt64?
    public let fallbackUrl: String?

    enum CodingKeys: String, CodingKey {
        case path
        case sourceTrackId = "source_track_id"
        case selectedTrackId = "selected_track_id"
        case codec
        case transcodeJobId = "transcode_job_id"
        case fallbackUrl = "fallback_url"
    }
}

public struct Rendition: Codable, Sendable, Equatable {
    public let width: UInt16
    public let height: UInt16
    public let videoBitrateBps: UInt64
    public let audioBitrateBps: UInt32

    public init(
        width: UInt16,
        height: UInt16,
        videoBitrateBps: UInt64,
        audioBitrateBps: UInt32
    ) {
        self.width = width
        self.height = height
        self.videoBitrateBps = videoBitrateBps
        self.audioBitrateBps = audioBitrateBps
    }

    enum CodingKeys: String, CodingKey {
        case width
        case height
        case videoBitrateBps = "video_bitrate_bps"
        case audioBitrateBps = "audio_bitrate_bps"
    }
}

public struct VodAsset: Codable, Sendable, Equatable {
    public let assetId: String
    public let tenantId: String
    public let version: UInt64
    public let state: String
    public let receivedBytes: UInt64?
    public let sourceBytes: UInt64?
    public let manifestURL: String?
    public let durationMillis: UInt64?
    public let failureReason: String?
    public let retryable: Bool?
    public let jobId: UInt64?

    enum CodingKeys: String, CodingKey {
        case assetId = "asset_id"
        case tenantId = "tenant_id"
        case version
        case state
        case receivedBytes = "received_bytes"
        case sourceBytes = "source_bytes"
        case manifestURL = "manifest_url"
        case durationMillis = "duration_millis"
        case failureReason = "failure_reason"
        case retryable
        case jobId = "job_id"
    }
}

public struct LiveOutput: Codable, Sendable, Equatable {
    public let streamId: String
    public let nextSequence: UInt64
    public let manifestURL: String
    public let workerJobId: UInt64?

    enum CodingKeys: String, CodingKey {
        case streamId = "stream_id"
        case nextSequence = "next_sequence"
        case manifestURL = "manifest_url"
        case workerJobId = "worker_job_id"
    }
}

public struct LiveSourceTrack: Codable, Sendable, Equatable {
    public let roomId: String
    public let trackId: UInt64
    public let kind: String
    public let codec: String
    public let payloadType: UInt8
    public let clockRate: UInt32
    public let channels: UInt8?
    public let fmtp: String?

    public init(
        roomId: String,
        trackId: UInt64,
        kind: String,
        codec: String,
        payloadType: UInt8,
        clockRate: UInt32,
        channels: UInt8? = nil,
        fmtp: String? = nil
    ) {
        self.roomId = roomId
        self.trackId = trackId
        self.kind = kind
        self.codec = codec
        self.payloadType = payloadType
        self.clockRate = clockRate
        self.channels = channels
        self.fmtp = fmtp
    }

    enum CodingKeys: String, CodingKey {
        case roomId = "room_id"
        case trackId = "track_id"
        case kind
        case codec
        case payloadType = "payload_type"
        case clockRate = "clock_rate"
        case channels
        case fmtp
    }
}

/// Adapter implemented by the application's native standards-compatible WebRTC peer connection.
public protocol WebRTCPeer: Sendable {
    /// Creates the reliable ordered `fluvora.room.v1` DataChannel before offer generation.
    func prepareRoomDataChannel() async throws
    func createAndSetLocalOffer() async throws -> String
    func setRemoteAnswer(sdp: String) async throws
}

public extension WebRTCPeer {
    /// Media-only adapters may retain this default implementation.
    func prepareRoomDataChannel() async throws {}
}

/// Closure adapter for the standards-compatible WebRTC peer already owned by an application.
///
/// This bridges async wrappers around libwebrtc without making the Fluvora package select or ship
/// a particular WebRTC binary distribution.
public struct CallbackWebRTCPeer: WebRTCPeer {
    private let createDataChannel: @Sendable () async throws -> Void
    private let createOffer: @Sendable () async throws -> String
    private let applyAnswer: @Sendable (String) async throws -> Void

    public init(
        createAndSetLocalOffer: @escaping @Sendable () async throws -> String,
        setRemoteAnswer: @escaping @Sendable (String) async throws -> Void,
        prepareRoomDataChannel: @escaping @Sendable () async throws -> Void = {}
    ) {
        self.createOffer = createAndSetLocalOffer
        self.applyAnswer = setRemoteAnswer
        self.createDataChannel = prepareRoomDataChannel
    }

    public func prepareRoomDataChannel() async throws {
        try await createDataChannel()
    }

    public func createAndSetLocalOffer() async throws -> String {
        try await createOffer()
    }

    public func setRemoteAnswer(sdp: String) async throws {
        try await applyAnswer(sdp)
    }
}

public struct FluvoraAPIError: Error, Sendable {
    public let status: Int
    public let code: String
    public let message: String
}

public actor FluvoraClient {
    private let baseURL: String
    private let session: URLSession
    private var accessToken: String
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    public init(
        baseURL: URL,
        accessToken: String,
        session: URLSession? = nil
    ) throws {
        let absoluteURL = baseURL.absoluteString
        guard absoluteURL.utf8.count <= maxBaseURLBytes,
              !Self.containsASCIIControl(absoluteURL),
              ["http", "https"].contains(baseURL.scheme?.lowercased() ?? ""),
              baseURL.host != nil,
              baseURL.user == nil,
              baseURL.password == nil,
              baseURL.query == nil,
              baseURL.fragment == nil,
              Self.isValidAccessToken(accessToken)
        else {
            throw FluvoraAPIError(
                status: 0,
                code: "invalid_configuration",
                message: "An uncredentialed HTTP(S) base URL without query or fragment and a valid access token are required"
            )
        }
        self.baseURL = absoluteURL.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        self.accessToken = accessToken
        self.session = session ?? makeDefaultURLSession()
    }

    public func setAccessToken(_ token: String) throws {
        guard Self.isValidAccessToken(token) else {
            throw FluvoraAPIError(
                status: 0,
                code: "invalid_token",
                message: "Access token must be 1-4096 bytes without control characters"
            )
        }
        accessToken = token
    }

    public func createRoom(
        mode: RoomMode,
        maxMembers: Int? = nil,
        maxPublishers: Int? = nil
    ) async throws -> Room {
        try await post(
            path: "/v1/rooms",
            body: CreateRoomRequest(
                mode: mode,
                maxMembers: maxMembers,
                maxPublishers: maxPublishers
            ),
            idempotent: true
        )
    }

    public func getRoom(roomId: String) async throws -> RoomSnapshot {
        try validateIdentifier(roomId)
        return try await get(path: "/v1/rooms/\(roomId)")
    }

    public func join(roomId: String) async throws -> CommandResult {
        try validateIdentifier(roomId)
        return try await post(
            path: "/v1/rooms/\(roomId)/join",
            body: EmptyRequest(),
            idempotent: true
        )
    }

    public func leave(roomId: String) async throws -> CommandResult {
        try validateIdentifier(roomId)
        return try await post(
            path: "/v1/rooms/\(roomId)/leave",
            body: EmptyRequest(),
            idempotent: true
        )
    }

    public func end(roomId: String) async throws -> CommandResult {
        try await roomCommand(roomId: roomId, operation: "end")
    }

    public func startPublishing(roomId: String) async throws -> CommandResult {
        try await roomCommand(roomId: roomId, operation: "publish/start")
    }

    public func stopPublishing(roomId: String) async throws -> CommandResult {
        try await roomCommand(roomId: roomId, operation: "publish/stop")
    }

    public func setRole(
        roomId: String,
        userId: String,
        role: MemberRole
    ) async throws -> CommandResult {
        try validateIdentifier(roomId)
        try validateIdentifier(userId)
        return try await post(
            path: "/v1/rooms/\(roomId)/roles",
            body: RoleRequest(userId: userId, role: role),
            idempotent: true
        )
    }

    public func sendChat(
        roomId: String,
        text: String
    ) async throws -> CommandResult {
        try await sendChat(
            roomId: roomId,
            text: text,
            messageId: Self.randomIdentifier()
        )
    }

    public func sendChat(
        roomId: String,
        text: String,
        messageId: String
    ) async throws -> CommandResult {
        try validateIdentifier(roomId)
        try validateIdentifier(messageId)
        guard !text.isEmpty, text.utf8.count <= maxChatBytes else {
            throw validationError("Chat message must contain 1...\(maxChatBytes) UTF-8 bytes")
        }
        return try await post(
            path: "/v1/rooms/\(roomId)/chat",
            body: ChatRequest(messageId: messageId, text: text),
            idempotent: true
        )
    }

    public func sendCustomData(
        roomId: String,
        namespace: String,
        schemaVersion: UInt16,
        payload: JSONValue
    ) async throws -> CommandResult {
        try validateIdentifier(roomId)
        try validateCustomNamespace(namespace)
        try validateJSONSize(payload, limit: maxCustomPayloadBytes, label: "Custom payload")
        return try await post(
            path: "/v1/rooms/\(roomId)/custom",
            body: CustomDataRequest(
                namespace: namespace,
                schemaVersion: schemaVersion,
                payload: payload
            ),
            idempotent: true
        )
    }

    public func recordVerifiedGift(
        roomId: String,
        gift: VerifiedGift
    ) async throws -> CommandResult {
        try validateIdentifier(roomId)
        try validateIdentifier(gift.senderId)
        try validateIdentifier(gift.recipientId)
        return try await post(
            path: "/v1/rooms/\(roomId)/gifts",
            body: gift,
            idempotent: true
        )
    }

    public func connectSFU(roomId: String, peer: any WebRTCPeer) async throws -> WebRTCSession {
        try validateIdentifier(roomId)
        try await peer.prepareRoomDataChannel()
        let offer = try await peer.createAndSetLocalOffer()
        guard offer.utf8.count <= maxSDPBytes else {
            throw validationError("SDP offer exceeds \(maxSDPBytes) bytes")
        }
        let mediaSession: WebRTCSession = try await post(
            path: "/v1/rooms/\(roomId)/webrtc/offer",
            body: OfferRequest(sdp: offer),
            idempotent: false
        )
        try await peer.setRemoteAnswer(sdp: mediaSession.answerSDP)
        return mediaSession
    }

    public func issueEventTicket(roomId: String) async throws -> EventTicket {
        try validateIdentifier(roomId)
        return try await post(
            path: "/v1/rooms/\(roomId)/events/tickets",
            body: EmptyRequest(),
            idempotent: false
        )
    }

    public func getIceConfiguration(roomId: String) async throws -> IceConfiguration {
        try validateIdentifier(roomId)
        return try await get(path: "/v1/rooms/\(roomId)/ice-servers")
    }

    public func postSignal(
        roomId: String,
        recipientId: String?,
        kind: String,
        payload: JSONValue
    ) async throws -> Signal {
        try validateIdentifier(roomId)
        if let recipientId {
            try validateIdentifier(recipientId)
        }
        guard ["offer", "answer", "ice-candidate", "ice-restart", "renegotiate", "bye"]
            .contains(kind)
        else {
            throw validationError("Unsupported P2P signal kind")
        }
        try validateJSONSize(payload, limit: maxSignalPayloadBytes, label: "Signal payload")
        return try await post(
            path: "/v1/rooms/\(roomId)/signals",
            body: SignalRequest(recipientId: recipientId, kind: kind, payload: payload),
            idempotent: true
        )
    }

    public func pollSignals(roomId: String, after: UInt64 = 0) async throws -> SignalPage {
        try validateIdentifier(roomId)
        return try await get(
            path: "/v1/rooms/\(roomId)/signals?after=\(after)&limit=\(signalPageMessages)"
        )
    }

    public func publishTrack(roomId: String, track: PublishTrack) async throws {
        try validateIdentifier(roomId)
        try await postNoContent(
            path: "/v1/rooms/\(roomId)/tracks",
            body: track,
            idempotent: true
        )
    }

    public func unpublishTrack(roomId: String, trackId: UInt64) async throws {
        try validateIdentifier(roomId)
        _ = try await performRequest(
            path: "/v1/rooms/\(roomId)/tracks/\(trackId)",
            method: "DELETE",
            body: nil,
            contentType: "application/json",
            idempotent: false
        )
    }

    public func subscribeTrack(
        roomId: String,
        subscription: SubscribeTrack
    ) async throws -> SubscribeTrackResult {
        try validateIdentifier(roomId)
        return try await post(
            path: "/v1/rooms/\(roomId)/subscriptions",
            body: subscription,
            idempotent: true
        )
    }

    public func unsubscribeTrack(roomId: String, subscriptionId: UInt64) async throws {
        try validateIdentifier(roomId)
        _ = try await performRequest(
            path: "/v1/rooms/\(roomId)/subscriptions/\(subscriptionId)",
            method: "DELETE",
            body: nil,
            contentType: "application/json",
            idempotent: false
        )
    }

    public func setSubscriptionLayer(
        roomId: String,
        subscriptionId: UInt64,
        spatialLayer: UInt8,
        temporalLayer: UInt8
    ) async throws {
        try validateIdentifier(roomId)
        try await postNoContent(
            path: "/v1/rooms/\(roomId)/subscriptions/\(subscriptionId)/layer",
            body: LayerRequest(spatialLayer: spatialLayer, temporalLayer: temporalLayer),
            idempotent: true
        )
    }

    public func createAsset(assetId: String, tenantId: String) async throws -> VodAsset {
        try validateMediaIdentifier(assetId)
        try validateMediaIdentifier(tenantId)
        return try await post(
            path: "/v1/assets",
            body: CreateAssetRequest(assetId: assetId, tenantId: tenantId),
            idempotent: true
        )
    }

    public func getAsset(assetId: String) async throws -> VodAsset {
        try validateMediaIdentifier(assetId)
        return try await get(path: "/v1/assets/\(assetId)")
    }

    public func deleteAsset(assetId: String) async throws {
        try validateMediaIdentifier(assetId)
        _ = try await performRequest(
            path: "/v1/assets/\(assetId)",
            method: "DELETE",
            body: nil,
            contentType: "application/json",
            idempotent: true
        )
    }

    public func uploadAssetChunk(
        assetId: String,
        offset: UInt64,
        data: Data
    ) async throws -> VodAsset {
        try validateMediaIdentifier(assetId)
        try validateMediaUpload(data, label: "upload chunk")
        return try await rawResponse(
            path: "/v1/assets/\(assetId)/source?offset=\(offset)",
            method: "PATCH",
            body: data,
            contentType: "application/octet-stream",
            idempotent: false
        )
    }

    public func completeAsset(
        assetId: String,
        sourceBytes: UInt64,
        renditions: [Rendition],
        segmentDurationMillis: UInt32 = 4_000
    ) async throws -> VodAsset {
        try validateMediaIdentifier(assetId)
        return try await post(
            path: "/v1/assets/\(assetId)/complete",
            body: CompleteAssetRequest(
                sourceBytes: sourceBytes,
                segmentDurationMillis: segmentDurationMillis,
                renditions: renditions
            ),
            idempotent: true
        )
    }

    public func createLiveOutput(
        streamId: String,
        windowSegments: Int = 6,
        firstSequence: UInt64 = 0
    ) async throws -> LiveOutput {
        try await createLiveOutputFromTracks(
            streamId: streamId,
            sourceTracks: [],
            windowSegments: windowSegments,
            firstSequence: firstSequence
        )
    }

    public func createLiveOutputFromTracks(
        streamId: String,
        sourceTracks: [LiveSourceTrack],
        windowSegments: Int = 6,
        firstSequence: UInt64 = 0,
        segmentDurationMillis: UInt32 = 4_000
    ) async throws -> LiveOutput {
        try await createLiveOutputFromTracksWithRenditions(
            streamId: streamId,
            sourceTracks: sourceTracks,
            renditions: [],
            windowSegments: windowSegments,
            firstSequence: firstSequence,
            segmentDurationMillis: segmentDurationMillis
        )
    }

    public func createLiveAbrOutputFromTracks(
        streamId: String,
        sourceTracks: [LiveSourceTrack],
        renditions: [Rendition],
        windowSegments: Int = 6,
        firstSequence: UInt64 = 0,
        segmentDurationMillis: UInt32 = 4_000
    ) async throws -> LiveOutput {
        guard !renditions.isEmpty else {
            throw validationError("live ABR requires at least one rendition")
        }
        return try await createLiveOutputFromTracksWithRenditions(
            streamId: streamId,
            sourceTracks: sourceTracks,
            renditions: renditions,
            windowSegments: windowSegments,
            firstSequence: firstSequence,
            segmentDurationMillis: segmentDurationMillis
        )
    }

    private func createLiveOutputFromTracksWithRenditions(
        streamId: String,
        sourceTracks: [LiveSourceTrack],
        renditions: [Rendition],
        windowSegments: Int,
        firstSequence: UInt64,
        segmentDurationMillis: UInt32
    ) async throws -> LiveOutput {
        try validateMediaIdentifier(streamId)
        for track in sourceTracks {
            try validateIdentifier(track.roomId)
        }
        return try await post(
            path: "/v1/live/\(streamId)",
            body: CreateLiveRequest(
                windowSegments: windowSegments,
                firstSequence: firstSequence,
                segmentDurationMillis: segmentDurationMillis,
                sourceTracks: sourceTracks,
                renditions: renditions
            ),
            idempotent: true
        )
    }

    public func getLiveOutput(streamId: String) async throws -> LiveOutput {
        try validateMediaIdentifier(streamId)
        return try await get(path: "/v1/live/\(streamId)")
    }

    public func deleteLiveOutput(streamId: String) async throws {
        try validateMediaIdentifier(streamId)
        _ = try await performRequest(
            path: "/v1/live/\(streamId)",
            method: "DELETE",
            body: nil,
            contentType: "application/json",
            idempotent: true
        )
    }

    public func uploadLiveInit(streamId: String, data: Data) async throws {
        try validateMediaIdentifier(streamId)
        try validateMediaUpload(data, label: "initialization segment")
        _ = try await performRequest(
            path: "/v1/live/\(streamId)/init",
            method: "PUT",
            body: data,
            contentType: "video/mp4",
            idempotent: false
        )
    }

    public func uploadLiveSegment(
        streamId: String,
        sequence: UInt64,
        durationMillis: UInt64,
        data: Data,
        discontinuity: Bool = false,
        programDateTime: String? = nil
    ) async throws -> LiveOutput {
        try validateMediaIdentifier(streamId)
        guard durationMillis > 0 else {
            throw validationError("invalid live segment")
        }
        try validateMediaUpload(data, label: "media segment")
        var components = URLComponents()
        components.queryItems = [
            URLQueryItem(name: "duration_millis", value: String(durationMillis)),
            URLQueryItem(name: "discontinuity", value: String(discontinuity)),
        ]
        if let programDateTime {
            components.queryItems?.append(
                URLQueryItem(name: "program_date_time", value: programDateTime)
            )
        }
        let query = components.percentEncodedQuery ?? ""
        return try await rawResponse(
            path: "/v1/live/\(streamId)/segments/\(sequence)?\(query)",
            method: "PUT",
            body: data,
            contentType: "video/iso.segment",
            idempotent: false
        )
    }

    public func finishLiveOutput(streamId: String) async throws {
        try validateMediaIdentifier(streamId)
        try await postNoContent(
            path: "/v1/live/\(streamId)/finish",
            body: EmptyRequest(),
            idempotent: true
        )
    }

    private func post<Body: Encodable, Response: Decodable>(
        path: String,
        body: Body,
        idempotent: Bool
    ) async throws -> Response {
        let data = try await performPost(path: path, body: body, idempotent: idempotent)
        return try decoder.decode(Response.self, from: data)
    }

    private func postNoContent<Body: Encodable>(
        path: String,
        body: Body,
        idempotent: Bool
    ) async throws {
        _ = try await performPost(path: path, body: body, idempotent: idempotent)
    }

    private func roomCommand(roomId: String, operation: String) async throws -> CommandResult {
        try validateIdentifier(roomId)
        return try await post(
            path: "/v1/rooms/\(roomId)/\(operation)",
            body: EmptyRequest(),
            idempotent: true
        )
    }

    private func get<Response: Decodable>(path: String) async throws -> Response {
        let data = try await performRequest(
            path: path,
            method: "GET",
            body: nil,
            contentType: "application/json",
            idempotent: false
        )
        return try decoder.decode(Response.self, from: data)
    }

    private func rawResponse<Response: Decodable>(
        path: String,
        method: String,
        body: Data,
        contentType: String,
        idempotent: Bool
    ) async throws -> Response {
        let data = try await performRequest(
            path: path,
            method: method,
            body: body,
            contentType: contentType,
            idempotent: idempotent
        )
        return try decoder.decode(Response.self, from: data)
    }

    private func performPost<Body: Encodable>(
        path: String,
        body: Body,
        idempotent: Bool
    ) async throws -> Data {
        let data = try encoder.encode(body)
        guard data.count <= maxJSONRequestBytes else {
            throw validationError("JSON request body exceeds \(maxJSONRequestBytes) bytes")
        }
        return try await performRequest(
            path: path,
            method: "POST",
            body: data,
            contentType: "application/json",
            idempotent: idempotent
        )
    }

    private func performRequest(
        path: String,
        method: String,
        body: Data?,
        contentType: String,
        idempotent: Bool
    ) async throws -> Data {
        guard path.hasPrefix("/"), let url = URL(string: baseURL + path) else {
            throw FluvoraAPIError(status: 0, code: "invalid_url", message: "Invalid API path")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 20
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue(contentType, forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        if idempotent {
            request.setValue(Self.randomIdentifier(), forHTTPHeaderField: "Idempotency-Key")
        }
        request.httpBody = body
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw FluvoraAPIError(status: 0, code: "invalid_response", message: "Not HTTP")
        }
        guard http.url == url else {
            throw FluvoraAPIError(
                status: http.statusCode,
                code: "redirect_rejected",
                message: "Fluvora API redirects are not allowed"
            )
        }
        let limit = (200..<300).contains(http.statusCode)
            ? maxJSONResponseBytes
            : maxErrorResponseBytes
        if http.expectedContentLength > Int64(limit) {
            throw Self.responseTooLarge(status: http.statusCode, limit: limit)
        }
        var data = Data()
        if http.expectedContentLength > 0 {
            data.reserveCapacity(min(Int(http.expectedContentLength), limit))
        }
        for try await byte in bytes {
            guard data.count < limit else {
                throw Self.responseTooLarge(status: http.statusCode, limit: limit)
            }
            data.append(byte)
        }
        guard (200..<300).contains(http.statusCode) else {
            let error = try? decoder.decode(ErrorResponse.self, from: data)
            throw FluvoraAPIError(
                status: http.statusCode,
                code: error?.code ?? "http_error",
                message: error?.message ?? "Fluvora API returned \(http.statusCode)"
            )
        }
        return data
    }

    private static func isValidAccessToken(_ value: String) -> Bool {
        !value.isEmpty &&
            value.utf8.count <= maxAccessTokenBytes &&
            !containsASCIIControl(value)
    }

    private static func containsASCIIControl(_ value: String) -> Bool {
        value.unicodeScalars.contains { scalar in
            scalar.value < 0x20 || scalar.value == 0x7f
        }
    }

    private static func responseTooLarge(status: Int, limit: Int) -> FluvoraAPIError {
        FluvoraAPIError(
            status: status,
            code: "response_too_large",
            message: "Fluvora response exceeds \(limit) bytes"
        )
    }

    private func validateIdentifier(_ value: String) throws {
        let allowed = CharacterSet(charactersIn: "0123456789abcdefABCDEF")
        guard (1...32).contains(value.count),
              value.unicodeScalars.allSatisfy(allowed.contains)
        else {
            throw FluvoraAPIError(
                status: 0,
                code: "invalid_identifier",
                message: "Identifier must be hexadecimal"
            )
        }
    }

    private func validateMediaIdentifier(_ value: String) throws {
        let allowed = CharacterSet.alphanumerics.union(
            CharacterSet(charactersIn: "_-")
        )
        guard (1...128).contains(value.count),
              value.unicodeScalars.allSatisfy(allowed.contains)
        else {
            throw validationError(
                "Media identifier may only contain letters, digits, underscore, or hyphen"
            )
        }
    }

    private func validateCustomNamespace(_ value: String) throws {
        let bytes = Array(value.utf8)
        guard let first = bytes.first,
              let last = bytes.last,
              bytes.count <= 64,
              Self.isASCIIAlphanumeric(first),
              Self.isASCIIAlphanumeric(last),
              bytes.allSatisfy({
                  Self.isASCIIAlphanumeric($0) || $0 == 46 || $0 == 95 || $0 == 45
              })
        else {
            throw validationError("Namespace must contain 1...64 safe ASCII characters")
        }
    }

    private func validateJSONSize(
        _ value: JSONValue,
        limit: Int,
        label: String
    ) throws {
        let encoded = try encoder.encode(value)
        guard encoded.count <= limit else {
            throw validationError("\(label) exceeds \(limit) bytes")
        }
    }

    private func validateMediaUpload(_ data: Data, label: String) throws {
        guard !data.isEmpty else {
            throw validationError("\(label) cannot be empty")
        }
        guard data.count <= maxMediaUploadBytes else {
            throw validationError("\(label) exceeds \(maxMediaUploadBytes) bytes")
        }
    }

    private nonisolated static func isASCIIAlphanumeric(_ value: UInt8) -> Bool {
        (48...57).contains(value) || (65...90).contains(value) || (97...122).contains(value)
    }

    private func validationError(_ message: String) -> FluvoraAPIError {
        FluvoraAPIError(status: 0, code: "invalid_argument", message: message)
    }

    private nonisolated static func randomIdentifier() -> String {
        UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
    }
}

private struct EmptyRequest: Encodable {}

private struct CreateRoomRequest: Encodable {
    let mode: RoomMode
    let maxMembers: Int?
    let maxPublishers: Int?

    enum CodingKeys: String, CodingKey {
        case mode
        case maxMembers = "max_members"
        case maxPublishers = "max_publishers"
    }
}

private struct ChatRequest: Encodable {
    let messageId: String
    let text: String

    enum CodingKeys: String, CodingKey {
        case messageId = "message_id"
        case text
    }
}

private struct CustomDataRequest: Encodable {
    let namespace: String
    let schemaVersion: UInt16
    let payload: JSONValue

    enum CodingKeys: String, CodingKey {
        case namespace
        case schemaVersion = "schema_version"
        case payload
    }
}

private struct RoleRequest: Encodable {
    let userId: String
    let role: MemberRole

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case role
    }
}

private struct OfferRequest: Encodable {
    let sdp: String
}

private struct SignalRequest: Encodable {
    let recipientId: String?
    let kind: String
    let payload: JSONValue

    enum CodingKeys: String, CodingKey {
        case recipientId = "to"
        case kind
        case payload
    }
}

private struct LayerRequest: Encodable {
    let spatialLayer: UInt8
    let temporalLayer: UInt8

    enum CodingKeys: String, CodingKey {
        case spatialLayer = "spatial_layer"
        case temporalLayer = "temporal_layer"
    }
}

private struct CreateAssetRequest: Encodable {
    let assetId: String
    let tenantId: String

    enum CodingKeys: String, CodingKey {
        case assetId = "asset_id"
        case tenantId = "tenant_id"
    }
}

private struct CompleteAssetRequest: Encodable {
    let sourceBytes: UInt64
    let segmentDurationMillis: UInt32
    let renditions: [Rendition]

    enum CodingKeys: String, CodingKey {
        case sourceBytes = "source_bytes"
        case segmentDurationMillis = "segment_duration_millis"
        case renditions
    }
}

private struct CreateLiveRequest: Encodable {
    let windowSegments: Int
    let firstSequence: UInt64
    let segmentDurationMillis: UInt32
    let sourceTracks: [LiveSourceTrack]
    let renditions: [Rendition]

    enum CodingKeys: String, CodingKey {
        case windowSegments = "window_segments"
        case firstSequence = "first_sequence"
        case segmentDurationMillis = "segment_duration_millis"
        case sourceTracks = "source_tracks"
        case renditions
    }
}

private struct ErrorResponse: Decodable {
    let code: String?
    let message: String?
}
