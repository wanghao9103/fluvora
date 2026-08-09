package com.fluvora.sdk

import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URI
import java.net.URISyntaxException
import java.net.URLEncoder
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

private const val MAX_BASE_URL_BYTES: Int = 2_048
private const val MAX_ACCESS_TOKEN_BYTES: Int = 4_096
private const val MAX_JSON_RESPONSE_BYTES: Int = 32 * 1_024 * 1_024
private const val MAX_ERROR_RESPONSE_BYTES: Int = 64 * 1_024
private const val MAX_JSON_REQUEST_BYTES: Int = 1 * 1_024 * 1_024
private const val MAX_CHAT_BYTES: Int = 4_096
private const val MAX_CUSTOM_PAYLOAD_BYTES: Int = 60 * 1_024
private const val MAX_SIGNAL_PAYLOAD_BYTES: Int = 64 * 1_024
private const val MAX_SDP_BYTES: Int = 256 * 1_024
private const val MAX_MEDIA_UPLOAD_BYTES: Int = 8 * 1_024 * 1_024
private const val SIGNAL_PAGE_MESSAGES: Int = 128

@Serializable
public enum class RoomMode {
    @SerialName("sfu") SFU,
    @SerialName("p2p") P2P,
    @SerialName("live") LIVE,
    @SerialName("vod") VOD,
}

@Serializable
public data class Room(
    @SerialName("room_id") val roomId: String,
    val mode: RoomMode,
    val sequence: Long,
    val duplicate: Boolean,
)

@Serializable
public data class RoomSnapshot(
    @SerialName("room_id") val roomId: String,
    val mode: RoomMode,
    val sequence: Long,
    val ended: Boolean,
    @SerialName("member_count") val memberCount: Int,
    @SerialName("publisher_count") val publisherCount: Int,
)

@Serializable
public enum class MemberRole {
    @SerialName("host") HOST,
    @SerialName("publisher") PUBLISHER,
    @SerialName("viewer") VIEWER,
}

@Serializable
public data class VerifiedGift(
    val provider: String,
    @SerialName("provider_timestamp_millis") val providerTimestampMillis: Long,
    @SerialName("provider_signature") val providerSignature: String,
    @SerialName("sender_id") val senderId: String,
    @SerialName("recipient_id") val recipientId: String,
    @SerialName("transaction_id") val transactionId: String,
    @SerialName("gift_id") val giftId: String,
    val quantity: Int,
    @SerialName("unit_value") val unitValue: Long,
    val currency: String,
)

@Serializable
public data class CommandResult(
    val sequence: Long,
    val duplicate: Boolean,
)

@Serializable
public data class WebRtcSession(
    @SerialName("session_id") val sessionId: String,
    @SerialName("answer_sdp") val answerSdp: String,
)

@Serializable
public data class EventTicket(
    val ticket: String,
    @SerialName("expires_at_millis") val expiresAtMillis: Long,
)

@Serializable
public data class IceServer(
    val urls: List<String>,
    val username: String,
    val credential: String,
)

@Serializable
public data class IceConfiguration(
    @SerialName("ice_servers") val iceServers: List<IceServer>,
    @SerialName("expires_at_millis") val expiresAtMillis: Long,
)

@Serializable
public data class Signal(
    val sequence: Long,
    @SerialName("from") val senderId: String,
    @SerialName("to") val recipientId: String? = null,
    val kind: String,
    val payload: JsonElement,
    @SerialName("timestamp_millis") val timestampMillis: Long,
)

@Serializable
public data class SignalPage(
    val signals: List<Signal>,
    @SerialName("latest_sequence") val latestSequence: Long,
)

@Serializable
public data class TrackEncoding(
    val ssrc: Long,
    val rid: String? = null,
    @SerialName("spatial_layer") val spatialLayer: Int,
    @SerialName("max_bitrate_bps") val maxBitrateBps: Long,
)

@Serializable
public data class HeaderExtensionRewrite(
    @SerialName("source_id") val sourceId: Int,
    @SerialName("destination_id") val destinationId: Int? = null,
    val replacement: ByteArray? = null,
)

@Serializable
public data class PublishTrack(
    @SerialName("track_id") val trackId: Long,
    val kind: String,
    val codec: String,
    @SerialName("clock_rate") val clockRate: Long,
    @SerialName("payload_type") val payloadType: Int,
    val encodings: List<TrackEncoding>,
    val width: Int = 0,
    val height: Int = 0,
    @SerialName("frames_per_second") val framesPerSecond: Int = 0,
)

@Serializable
public data class SubscribeTrack(
    @SerialName("subscription_id") val subscriptionId: Long,
    @SerialName("track_id") val trackId: Long,
    @SerialName("output_ssrc") val outputSsrc: Long,
    @SerialName("output_payload_type") val outputPayloadType: Int,
    @SerialName("spatial_layer") val spatialLayer: Int,
    @SerialName("temporal_layer") val temporalLayer: Int,
    @SerialName("initial_sequence_number") val initialSequenceNumber: Int,
    @SerialName("initial_timestamp") val initialTimestamp: Long,
    @SerialName("extension_rewrites")
    val extensionRewrites: List<HeaderExtensionRewrite> = emptyList(),
    @SerialName("transport_wide_extension_id")
    val transportWideExtensionId: Int? = null,
    @SerialName("subscriber_codecs") val subscriberCodecs: List<String> = emptyList(),
    @SerialName("allow_transcoding") val allowTranscoding: Boolean = false,
    @SerialName("network_quality") val networkQuality: String? = null,
    @SerialName("hls_fallback_url") val hlsFallbackUrl: String? = null,
    @SerialName("target_width") val targetWidth: Int? = null,
    @SerialName("target_height") val targetHeight: Int? = null,
    @SerialName("target_frames_per_second") val targetFramesPerSecond: Int? = null,
    @SerialName("target_bitrate_bps") val targetBitrateBps: Long? = null,
)

@Serializable
public data class SubscribeTrackResult(
    val path: String,
    @SerialName("source_track_id") val sourceTrackId: Long,
    @SerialName("selected_track_id") val selectedTrackId: Long? = null,
    val codec: String? = null,
    @SerialName("transcode_job_id") val transcodeJobId: Long? = null,
    @SerialName("fallback_url") val fallbackUrl: String? = null,
)

@Serializable
public data class Rendition(
    val width: Int,
    val height: Int,
    @SerialName("video_bitrate_bps") val videoBitrateBps: Long,
    @SerialName("audio_bitrate_bps") val audioBitrateBps: Long,
)

@Serializable
public data class VodAsset(
    @SerialName("asset_id") val assetId: String,
    @SerialName("tenant_id") val tenantId: String,
    val version: Long,
    val state: String,
    @SerialName("received_bytes") val receivedBytes: Long? = null,
    @SerialName("source_bytes") val sourceBytes: Long? = null,
    @SerialName("manifest_url") val manifestUrl: String? = null,
    @SerialName("duration_millis") val durationMillis: Long? = null,
    @SerialName("failure_reason") val failureReason: String? = null,
    val retryable: Boolean? = null,
    @SerialName("job_id") val jobId: Long? = null,
)

@Serializable
public data class LiveOutput(
    @SerialName("stream_id") val streamId: String,
    @SerialName("next_sequence") val nextSequence: Long,
    @SerialName("manifest_url") val manifestUrl: String,
    @SerialName("worker_job_id") val workerJobId: Long? = null,
)

@Serializable
public data class LiveSourceTrack(
    @SerialName("room_id") val roomId: String,
    @SerialName("track_id") val trackId: Long,
    val kind: String,
    val codec: String,
    @SerialName("payload_type") val payloadType: Int,
    @SerialName("clock_rate") val clockRate: Long,
    val channels: Int? = null,
    val fmtp: String? = null,
)

/**
 * Adapter for the application's standards-compatible native WebRTC implementation.
 *
 * Implement this interface with the peer connection already used by the Android application.
 * The SDK intentionally does not force a particular WebRTC binary distribution.
 */
public interface WebRtcPeer {
    /**
     * Creates the reliable ordered `fluvora.room.v1` DataChannel before offer generation.
     *
     * Media-only adapters may retain this default implementation.
     */
    public suspend fun prepareRoomDataChannel() {}

    public suspend fun createAndSetLocalOffer(): String
    public suspend fun setRemoteAnswer(sdp: String)
}

/**
 * Closure adapter for the native WebRTC peer connection already owned by an application.
 *
 * It provides a ready-to-use [WebRtcPeer] without coupling Fluvora to a particular Android WebRTC
 * binary or ABI. The callbacks can directly bridge coroutine wrappers around libwebrtc.
 */
public class CallbackWebRtcPeer(
    private val createOffer: suspend () -> String,
    private val applyRemoteAnswer: suspend (String) -> Unit,
    private val createRoomDataChannel: suspend () -> Unit = {},
) : WebRtcPeer {
    override suspend fun prepareRoomDataChannel(): Unit = createRoomDataChannel()

    override suspend fun createAndSetLocalOffer(): String = createOffer()

    override suspend fun setRemoteAnswer(sdp: String): Unit = applyRemoteAnswer(sdp)
}

public class FluvoraException(
    public val status: Int,
    public val code: String,
    message: String,
) : Exception(message)

public class FluvoraClient(
    baseUrl: String,
    accessToken: String,
) {
    private val endpoint: URI
    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    @Volatile
    private var token: String = accessToken

    init {
        val normalizedBaseUrl = baseUrl.trimEnd('/')
        endpoint = try {
            URI(normalizedBaseUrl)
        } catch (error: URISyntaxException) {
            throw IllegalArgumentException("baseUrl must be a valid URI", error)
        }
        require(
            normalizedBaseUrl.toByteArray(Charsets.UTF_8).size in 1..MAX_BASE_URL_BYTES &&
                normalizedBaseUrl.none { isAsciiControl(it) } &&
                (endpoint.scheme?.lowercase() == "https" || endpoint.scheme?.lowercase() == "http") &&
                endpoint.host != null &&
                endpoint.rawUserInfo == null &&
                endpoint.rawQuery == null &&
                endpoint.rawFragment == null,
        ) {
            "baseUrl must be an uncredentialed HTTP(S) URL without query or fragment"
        }
        requireValidAccessToken(accessToken)
    }

    public fun setAccessToken(accessToken: String) {
        requireValidAccessToken(accessToken)
        token = accessToken
    }

    public suspend fun createRoom(
        mode: RoomMode,
        maxMembers: Int? = null,
        maxPublishers: Int? = null,
    ): Room = request(
        path = "/v1/rooms",
        body = json.encodeToString(
            CreateRoomRequest(mode, maxMembers, maxPublishers),
        ),
        idempotent = true,
    )

    public suspend fun getRoom(roomId: String): RoomSnapshot = request(
        path = "/v1/rooms/${checkedId(roomId)}",
        method = "GET",
        body = null,
        idempotent = false,
    )

    public suspend fun join(roomId: String): CommandResult =
        write("/v1/rooms/${checkedId(roomId)}/join")

    public suspend fun leave(roomId: String): CommandResult =
        write("/v1/rooms/${checkedId(roomId)}/leave")

    public suspend fun end(roomId: String): CommandResult =
        write("/v1/rooms/${checkedId(roomId)}/end")

    public suspend fun startPublishing(roomId: String): CommandResult =
        write("/v1/rooms/${checkedId(roomId)}/publish/start")

    public suspend fun stopPublishing(roomId: String): CommandResult =
        write("/v1/rooms/${checkedId(roomId)}/publish/stop")

    public suspend fun setRole(
        roomId: String,
        userId: String,
        role: MemberRole,
    ): CommandResult = write(
        "/v1/rooms/${checkedId(roomId)}/roles",
        json.encodeToString(RoleRequest(checkedId(userId), role)),
    )

    public suspend fun sendChat(
        roomId: String,
        text: String,
        messageId: String = randomId(),
    ): CommandResult {
        require(text.isNotEmpty() && text.toByteArray(Charsets.UTF_8).size <= MAX_CHAT_BYTES) {
            "chat message must contain 1..$MAX_CHAT_BYTES UTF-8 bytes"
        }
        return write(
            "/v1/rooms/${checkedId(roomId)}/chat",
            json.encodeToString(ChatRequest(messageId, text)),
        )
    }

    public suspend fun sendCustomData(
        roomId: String,
        namespace: String,
        schemaVersion: Int,
        payload: JsonElement,
    ): CommandResult {
        require(namespace.matches(Regex("[A-Za-z0-9](?:[A-Za-z0-9._-]{0,62}[A-Za-z0-9])?"))) {
            "namespace must contain 1..64 safe ASCII characters"
        }
        require(schemaVersion in 0..65_535) { "schemaVersion must fit UInt16" }
        requireJsonSize(payload, MAX_CUSTOM_PAYLOAD_BYTES, "custom payload")
        return write(
            "/v1/rooms/${checkedId(roomId)}/custom",
            json.encodeToString(CustomDataRequest(namespace, schemaVersion, payload)),
        )
    }

    public suspend fun recordVerifiedGift(
        roomId: String,
        gift: VerifiedGift,
    ): CommandResult {
        checkedId(gift.senderId)
        checkedId(gift.recipientId)
        return write(
            "/v1/rooms/${checkedId(roomId)}/gifts",
            json.encodeToString(gift),
        )
    }

    public suspend fun connectSfu(roomId: String, peer: WebRtcPeer): WebRtcSession {
        peer.prepareRoomDataChannel()
        val offer = peer.createAndSetLocalOffer()
        requireUtf8Size(offer, MAX_SDP_BYTES, "SDP offer")
        val session: WebRtcSession = request(
            path = "/v1/rooms/${checkedId(roomId)}/webrtc/offer",
            body = json.encodeToString(OfferRequest(offer)),
            idempotent = false,
        )
        peer.setRemoteAnswer(session.answerSdp)
        return session
    }

    public suspend fun issueEventTicket(roomId: String): EventTicket = request(
        path = "/v1/rooms/${checkedId(roomId)}/events/tickets",
        body = "{}",
        idempotent = false,
    )

    public suspend fun getIceConfiguration(roomId: String): IceConfiguration = request(
        path = "/v1/rooms/${checkedId(roomId)}/ice-servers",
        method = "GET",
        body = null,
        idempotent = false,
    )

    public suspend fun postSignal(
        roomId: String,
        recipientId: String?,
        kind: String,
        payload: JsonElement,
    ): Signal {
        require(kind in SIGNAL_KINDS) { "unsupported P2P signal kind" }
        recipientId?.let { checkedId(it) }
        requireJsonSize(payload, MAX_SIGNAL_PAYLOAD_BYTES, "signal payload")
        return request(
            path = "/v1/rooms/${checkedId(roomId)}/signals",
            body = json.encodeToString(SignalRequest(recipientId, kind, payload)),
            idempotent = true,
        )
    }

    public suspend fun pollSignals(roomId: String, after: Long = 0): SignalPage {
        require(after >= 0) { "after cannot be negative" }
        return request(
            path = "/v1/rooms/${checkedId(roomId)}/signals?after=$after&limit=$SIGNAL_PAGE_MESSAGES",
            method = "GET",
            body = null,
            idempotent = false,
        )
    }

    public suspend fun publishTrack(roomId: String, track: PublishTrack) {
        requestNoContent(
            path = "/v1/rooms/${checkedId(roomId)}/tracks",
            body = json.encodeToString(track),
            idempotent = true,
        )
    }

    public suspend fun unpublishTrack(roomId: String, trackId: Long) {
        require(trackId > 0) { "trackId must be positive" }
        requestNoContent(
            path = "/v1/rooms/${checkedId(roomId)}/tracks/$trackId",
            method = "DELETE",
            body = "{}",
            idempotent = false,
        )
    }

    public suspend fun subscribeTrack(
        roomId: String,
        subscription: SubscribeTrack,
    ): SubscribeTrackResult = request(
            path = "/v1/rooms/${checkedId(roomId)}/subscriptions",
            body = json.encodeToString(subscription),
            idempotent = true,
        )

    public suspend fun unsubscribeTrack(roomId: String, subscriptionId: Long) {
        require(subscriptionId >= 0) { "subscriptionId cannot be negative" }
        requestNoContent(
            path = "/v1/rooms/${checkedId(roomId)}/subscriptions/$subscriptionId",
            method = "DELETE",
            body = "{}",
            idempotent = false,
        )
    }

    public suspend fun setSubscriptionLayer(
        roomId: String,
        subscriptionId: Long,
        spatialLayer: Int,
        temporalLayer: Int,
    ) {
        require(subscriptionId >= 0) { "subscriptionId cannot be negative" }
        requestNoContent(
            path = "/v1/rooms/${checkedId(roomId)}/subscriptions/$subscriptionId/layer",
            body = json.encodeToString(LayerRequest(spatialLayer, temporalLayer)),
            idempotent = true,
        )
    }

    public suspend fun createAsset(assetId: String, tenantId: String): VodAsset {
        checkedMediaId(assetId)
        checkedMediaId(tenantId)
        return request(
            path = "/v1/assets",
            body = json.encodeToString(CreateAssetRequest(assetId, tenantId)),
            idempotent = true,
        )
    }

    public suspend fun getAsset(assetId: String): VodAsset = request(
        path = "/v1/assets/${checkedMediaId(assetId)}",
        method = "GET",
        body = null,
        idempotent = false,
    )

    public suspend fun deleteAsset(assetId: String) {
        requestNoContent(
            path = "/v1/assets/${checkedMediaId(assetId)}",
            method = "DELETE",
            body = "{}",
            idempotent = true,
        )
    }

    public suspend fun uploadAssetChunk(
        assetId: String,
        offset: Long,
        bytes: ByteArray,
    ): VodAsset {
        require(offset >= 0) { "offset cannot be negative" }
        requireMediaUpload(bytes, "upload chunk")
        return requestRaw(
            path = "/v1/assets/${checkedMediaId(assetId)}/source?offset=$offset",
            method = "PATCH",
            body = bytes,
            contentType = "application/octet-stream",
            idempotent = false,
        )
    }

    public suspend fun completeAsset(
        assetId: String,
        sourceBytes: Long,
        renditions: List<Rendition>,
        segmentDurationMillis: Int = 4_000,
    ): VodAsset = request(
        path = "/v1/assets/${checkedMediaId(assetId)}/complete",
        body = json.encodeToString(
            CompleteAssetRequest(sourceBytes, segmentDurationMillis, renditions),
        ),
        idempotent = true,
    )

    public suspend fun createLiveOutput(
        streamId: String,
        windowSegments: Int = 6,
        firstSequence: Long = 0,
    ): LiveOutput = createLiveOutputFromTracks(
        streamId = streamId,
        sourceTracks = emptyList(),
        windowSegments = windowSegments,
        firstSequence = firstSequence,
    )

    public suspend fun createLiveOutputFromTracks(
        streamId: String,
        sourceTracks: List<LiveSourceTrack>,
        windowSegments: Int = 6,
        firstSequence: Long = 0,
        segmentDurationMillis: Int = 4_000,
    ): LiveOutput = createLiveOutputFromTracksWithRenditions(
        streamId,
        sourceTracks,
        emptyList(),
        windowSegments,
        firstSequence,
        segmentDurationMillis,
    )

    public suspend fun createLiveAbrOutputFromTracks(
        streamId: String,
        sourceTracks: List<LiveSourceTrack>,
        renditions: List<Rendition>,
        windowSegments: Int = 6,
        firstSequence: Long = 0,
        segmentDurationMillis: Int = 4_000,
    ): LiveOutput {
        require(renditions.isNotEmpty()) { "live ABR requires at least one rendition" }
        return createLiveOutputFromTracksWithRenditions(
            streamId,
            sourceTracks,
            renditions,
            windowSegments,
            firstSequence,
            segmentDurationMillis,
        )
    }

    private suspend fun createLiveOutputFromTracksWithRenditions(
        streamId: String,
        sourceTracks: List<LiveSourceTrack>,
        renditions: List<Rendition>,
        windowSegments: Int,
        firstSequence: Long,
        segmentDurationMillis: Int,
    ): LiveOutput {
        sourceTracks.forEach { checkedId(it.roomId) }
        return request(
            path = "/v1/live/${checkedMediaId(streamId)}",
            body = json.encodeToString(
                CreateLiveRequest(
                    windowSegments,
                    firstSequence,
                    segmentDurationMillis,
                    sourceTracks,
                    renditions,
                ),
            ),
            idempotent = true,
        )
    }

    public suspend fun getLiveOutput(streamId: String): LiveOutput = request(
        path = "/v1/live/${checkedMediaId(streamId)}",
        method = "GET",
        body = null,
        idempotent = false,
    )

    public suspend fun deleteLiveOutput(streamId: String) {
        requestNoContent(
            path = "/v1/live/${checkedMediaId(streamId)}",
            method = "DELETE",
            body = "{}",
            idempotent = true,
        )
    }

    public suspend fun uploadLiveInit(streamId: String, bytes: ByteArray) {
        requireMediaUpload(bytes, "initialization segment")
        requestRawNoContent(
            path = "/v1/live/${checkedMediaId(streamId)}/init",
            method = "PUT",
            body = bytes,
            contentType = "video/mp4",
            idempotent = false,
        )
    }

    public suspend fun uploadLiveSegment(
        streamId: String,
        sequence: Long,
        durationMillis: Long,
        bytes: ByteArray,
        discontinuity: Boolean = false,
        programDateTime: String? = null,
    ): LiveOutput {
        require(sequence >= 0 && durationMillis > 0) { "invalid live segment metadata" }
        requireMediaUpload(bytes, "media segment")
        val query = buildString {
            append("duration_millis=$durationMillis&discontinuity=$discontinuity")
            if (programDateTime != null) {
                append("&program_date_time=")
                append(URLEncoder.encode(programDateTime, Charsets.UTF_8.name()))
            }
        }
        return requestRaw(
            path = "/v1/live/${checkedMediaId(streamId)}/segments/$sequence?$query",
            method = "PUT",
            body = bytes,
            contentType = "video/iso.segment",
            idempotent = false,
        )
    }

    public suspend fun finishLiveOutput(streamId: String) {
        requestNoContent(
            path = "/v1/live/${checkedMediaId(streamId)}/finish",
            body = "{}",
            idempotent = true,
        )
    }

    private suspend inline fun <reified T> write(path: String, body: String? = null): T =
        request(path, body = body ?: "{}", idempotent = true)

    private suspend inline fun <reified T> request(
        path: String,
        method: String = "POST",
        body: String?,
        idempotent: Boolean,
    ): T = json.decodeFromString(
        execute(
            path = path,
            method = method,
            body = body?.let(::checkedJsonBody),
            contentType = "application/json",
            idempotent = idempotent,
        ),
    )

    private suspend fun requestNoContent(
        path: String,
        method: String = "POST",
        body: String,
        idempotent: Boolean,
    ) {
        execute(
            path,
            method,
            checkedJsonBody(body),
            "application/json",
            idempotent,
        )
    }

    private suspend inline fun <reified T> requestRaw(
        path: String,
        method: String,
        body: ByteArray,
        contentType: String,
        idempotent: Boolean,
    ): T = json.decodeFromString(execute(path, method, body, contentType, idempotent))

    private suspend fun requestRawNoContent(
        path: String,
        method: String,
        body: ByteArray,
        contentType: String,
        idempotent: Boolean,
    ) {
        execute(path, method, body, contentType, idempotent)
    }

    private suspend fun execute(
        path: String,
        method: String,
        body: ByteArray?,
        contentType: String,
        idempotent: Boolean,
    ): String = withContext(Dispatchers.IO) {
        val connection = URI(endpoint.toString() + path).toURL().openConnection() as HttpURLConnection
        try {
            connection.requestMethod = method
            connection.connectTimeout = 10_000
            connection.readTimeout = 20_000
            connection.instanceFollowRedirects = false
            connection.setRequestProperty("Accept", "application/json")
            connection.setRequestProperty("Content-Type", contentType)
            connection.setRequestProperty("Authorization", "Bearer $token")
            if (idempotent) {
                connection.setRequestProperty("Idempotency-Key", randomId())
            }
            if (body != null) {
                connection.doOutput = true
                connection.outputStream.use { it.write(body) }
            }
            val status = connection.responseCode
            val stream = if (status in 200..299) connection.inputStream else connection.errorStream
            val limit = if (status in 200..299) {
                MAX_JSON_RESPONSE_BYTES
            } else {
                MAX_ERROR_RESPONSE_BYTES
            }
            val response = readBoundedResponse(
                stream = stream,
                contentLength = connection.contentLengthLong,
                limit = limit,
                status = status,
            )
            if (status !in 200..299) {
                val error = runCatching { json.decodeFromString<ApiError>(response) }.getOrNull()
                throw FluvoraException(
                    status,
                    error?.code ?: "http_error",
                    error?.message ?: "Fluvora API returned $status",
                )
            }
            response
        } finally {
            connection.disconnect()
        }
    }

    private fun readBoundedResponse(
        stream: InputStream?,
        contentLength: Long,
        limit: Int,
        status: Int,
    ): String {
        if (contentLength > limit.toLong()) {
            throw responseTooLarge(status, limit)
        }
        if (stream == null) return ""

        val initialCapacity = if (contentLength in 1..limit.toLong()) {
            contentLength.toInt()
        } else {
            8_192
        }
        val output = ByteArrayOutputStream(initialCapacity)
        val buffer = ByteArray(8_192)
        stream.use { input ->
            while (true) {
                val count = input.read(buffer)
                if (count == -1) break
                if (count == 0) continue
                if (output.size() > limit - count) {
                    throw responseTooLarge(status, limit)
                }
                output.write(buffer, 0, count)
            }
        }
        return output.toString(Charsets.UTF_8.name())
    }

    private fun responseTooLarge(status: Int, limit: Int): FluvoraException = FluvoraException(
        status = status,
        code = "response_too_large",
        message = "Fluvora response exceeds $limit bytes",
    )

    private fun requireValidAccessToken(value: String) {
        require(
            value.toByteArray(Charsets.UTF_8).size in 1..MAX_ACCESS_TOKEN_BYTES &&
                value.none { isAsciiControl(it) },
        ) {
            "accessToken must be 1-4096 bytes without control characters"
        }
    }

    private fun requireJsonSize(value: JsonElement, limit: Int, label: String) {
        requireUtf8Size(json.encodeToString(value), limit, label)
    }

    private fun requireUtf8Size(value: String, limit: Int, label: String) {
        require(value.toByteArray(Charsets.UTF_8).size <= limit) {
            "$label exceeds $limit bytes"
        }
    }

    private fun checkedJsonBody(value: String): ByteArray = value.toByteArray(Charsets.UTF_8).also {
        require(it.size <= MAX_JSON_REQUEST_BYTES) {
            "JSON request body exceeds $MAX_JSON_REQUEST_BYTES bytes"
        }
    }

    private fun requireMediaUpload(bytes: ByteArray, label: String) {
        require(bytes.isNotEmpty()) { "$label cannot be empty" }
        require(bytes.size <= MAX_MEDIA_UPLOAD_BYTES) {
            "$label exceeds $MAX_MEDIA_UPLOAD_BYTES bytes"
        }
    }

    private fun isAsciiControl(value: Char): Boolean = value.code < 0x20 || value.code == 0x7f

    private fun checkedId(value: String): String {
        require(value.matches(Regex("[0-9a-fA-F]{1,32}"))) {
            "identifier must be hexadecimal"
        }
        return value
    }

    private fun checkedMediaId(value: String): String {
        require(value.matches(Regex("[A-Za-z0-9_-]{1,128}"))) {
            "media identifier must contain only letters, digits, underscore, or hyphen"
        }
        return value
    }

    private fun randomId(): String = UUID.randomUUID().toString().replace("-", "")

    private companion object {
        val SIGNAL_KINDS: Set<String> =
            setOf("offer", "answer", "ice-candidate", "ice-restart", "renegotiate", "bye")
    }
}

@Serializable
private data class CreateRoomRequest(
    val mode: RoomMode,
    @SerialName("max_members") val maxMembers: Int?,
    @SerialName("max_publishers") val maxPublishers: Int?,
)

@Serializable
private data class ChatRequest(
    @SerialName("message_id") val messageId: String,
    val text: String,
)

@Serializable
private data class CustomDataRequest(
    val namespace: String,
    @SerialName("schema_version") val schemaVersion: Int,
    val payload: JsonElement,
)

@Serializable
private data class RoleRequest(
    @SerialName("user_id") val userId: String,
    val role: MemberRole,
)

@Serializable
private data class OfferRequest(val sdp: String)

@Serializable
private data class SignalRequest(
    @SerialName("to") val recipientId: String?,
    val kind: String,
    val payload: JsonElement,
)

@Serializable
private data class LayerRequest(
    @SerialName("spatial_layer") val spatialLayer: Int,
    @SerialName("temporal_layer") val temporalLayer: Int,
)

@Serializable
private data class CreateAssetRequest(
    @SerialName("asset_id") val assetId: String,
    @SerialName("tenant_id") val tenantId: String,
)

@Serializable
private data class CompleteAssetRequest(
    @SerialName("source_bytes") val sourceBytes: Long,
    @SerialName("segment_duration_millis") val segmentDurationMillis: Int,
    val renditions: List<Rendition>,
)

@Serializable
private data class CreateLiveRequest(
    @SerialName("window_segments") val windowSegments: Int,
    @SerialName("first_sequence") val firstSequence: Long,
    @SerialName("segment_duration_millis") val segmentDurationMillis: Int,
    @SerialName("source_tracks") val sourceTracks: List<LiveSourceTrack>,
    val renditions: List<Rendition>,
)

@Serializable
private data class ApiError(
    val code: String? = null,
    val message: String? = null,
)
