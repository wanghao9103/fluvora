package com.fluvora.demo

import android.app.Activity
import android.os.Bundle
import android.text.InputType
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import com.fluvora.sdk.CallbackWebRtcPeer
import com.fluvora.sdk.FluvoraClient
import com.fluvora.sdk.IceServer
import com.fluvora.sdk.RoomMode
import com.fluvora.sdk.WebRtcSession
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Dependency-neutral contract implemented by the application's standard WebRTC engine.
 *
 * A production implementation owns camera/microphone tracks, creates an ordered reliable
 * `fluvora.room.v1` DataChannel, gathers ICE before returning the offer, applies the answer, and
 * closes every track and PeerConnection from [close].
 */
public interface NativeWebRtcEngine {
    public suspend fun createRoomDataChannel(label: String, protocol: String)
    public suspend fun createAndSetLocalOffer(): String
    public suspend fun setRemoteAnswer(sdp: String)
    public fun close()
}

/**
 * The host application installs its libwebrtc/engine factory before launching [MainActivity].
 *
 * Keeping this registry in the example—not the Fluvora SDK—allows applications to select a
 * supported WebRTC binary and ABI for their own device matrix.
 */
public object WebRtcEngineProvider {
    @Volatile
    public var factory: ((List<IceServer>) -> NativeWebRtcEngine)? = null
}

public class MainActivity : Activity() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private lateinit var baseUrl: EditText
    private lateinit var token: EditText
    private lateinit var roomId: EditText
    private lateinit var remoteParticipant: EditText
    private lateinit var message: EditText
    private lateinit var output: TextView
    private var client: FluvoraClient? = null
    private var engine: NativeWebRtcEngine? = null
    private var p2pEngine: NativeWebRtcEngine? = null
    private var session: WebRtcSession? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val form = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(32, 32, 32, 32)
        }
        baseUrl = field("API URL", "http://10.0.2.2:8080")
        token = field("Short-lived access token").apply {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        }
        roomId = field("Room ID")
        remoteParticipant = field("P2P recipient participant ID")
        message = field("Chat/custom payload", "hello")
        output = TextView(this).apply {
            setPadding(0, 24, 0, 0)
            setTextIsSelectable(true)
        }
        form.addView(baseUrl)
        form.addView(token)
        form.addView(roomId)
        form.addView(remoteParticipant)
        form.addView(message)
        form.addView(button("Create SFU room") { createRoom() })
        form.addView(button("Join room") { joinRoom() })
        form.addView(button("Connect SFU media") { connectMedia() })
        form.addView(button("Send durable chat + custom data") { sendRoomData() })
        form.addView(button("Send P2P offer signal") { sendP2pSignal() })
        form.addView(button("Leave and clean up") { leaveAndCleanUp() })
        form.addView(output)
        setContentView(ScrollView(this).apply { addView(form) })
        log("Install WebRtcEngineProvider.factory to enable the media button.")
    }

    private fun configuredClient(): FluvoraClient {
        val sdk = FluvoraClient(
            baseUrl = baseUrl.text.toString().trim(),
            accessToken = token.text.toString().trim(),
        )
        client = sdk
        return sdk
    }

    private fun createRoom(): Unit = launchOperation("create") {
        val room = configuredClient().createRoom(
            mode = RoomMode.SFU,
            maxMembers = 64,
            maxPublishers = 16,
        )
        roomId.setText(room.roomId)
        log("room=${room.roomId}, sequence=${room.sequence}")
    }

    private fun joinRoom(): Unit = launchOperation("join") {
        val result = configuredClient().join(requiredRoomId())
        log("joined at sequence=${result.sequence}")
    }

    private fun connectMedia(): Unit = launchOperation("connect SFU") {
        val sdk = configuredClient()
        val id = requiredRoomId()
        val ice = sdk.getIceConfiguration(id)
        log("received ${ice.iceServers.size} room-scoped ICE servers")
        val nativePeer = checkNotNull(WebRtcEngineProvider.factory?.invoke(ice.iceServers)) {
            "WebRtcEngineProvider.factory is not installed"
        }
        engine?.close()
        engine = nativePeer
        sdk.startPublishing(id)
        val peer = CallbackWebRtcPeer(
            createOffer = nativePeer::createAndSetLocalOffer,
            applyRemoteAnswer = nativePeer::setRemoteAnswer,
            createRoomDataChannel = {
                nativePeer.createRoomDataChannel("fluvora.room.v1", "fluvora.v1")
            },
        )
        session = sdk.connectSfu(id, peer)
        log("SFU connected: session=${session?.sessionId}")
    }

    private fun sendRoomData(): Unit = launchOperation("send data") {
        val sdk = configuredClient()
        val id = requiredRoomId()
        val text = message.text.toString()
        sdk.sendChat(id, text)
        sdk.sendCustomData(
            roomId = id,
            namespace = "demo.android",
            schemaVersion = 1,
            payload = buildJsonObject { put("message", text) },
        )
        log("durable chat and custom event accepted")
    }

    private fun sendP2pSignal(): Unit = launchOperation("send P2P signal") {
        val recipient = remoteParticipant.text.toString().trim()
        require(recipient.isNotEmpty()) { "P2P recipient is required" }
        val sdk = configuredClient()
        val id = requiredRoomId()
        val ice = sdk.getIceConfiguration(id)
        val nativePeer = checkNotNull(WebRtcEngineProvider.factory?.invoke(ice.iceServers)) {
            "WebRtcEngineProvider.factory is not installed"
        }
        p2pEngine?.close()
        p2pEngine = nativePeer
        val offer = nativePeer.createAndSetLocalOffer()
        val payload: JsonElement = buildJsonObject {
            put("type", "offer")
            put("sdp", offer)
        }
        sdk.postSignal(id, recipient, "offer", payload)
        val page = sdk.pollSignals(requiredRoomId())
        log("P2P signal accepted; latest=${page.latestSequence}")
    }

    private fun leaveAndCleanUp(): Unit = launchOperation("leave") {
        engine?.close()
        engine = null
        p2pEngine?.close()
        p2pEngine = null
        session = null
        val sdk = client
        if (sdk != null && roomId.text.isNotBlank()) {
            runCatching { sdk.stopPublishing(requiredRoomId()) }
                .onFailure { log("stop publishing skipped: ${it.message}") }
            sdk.leave(requiredRoomId())
        }
        log("native tracks, PeerConnection, and room membership released")
    }

    private fun requiredRoomId(): String =
        roomId.text.toString().trim().also { require(it.isNotEmpty()) { "Room ID is required" } }

    private fun launchOperation(name: String, block: suspend () -> Unit) {
        scope.launch {
            runCatching { block() }
                .onFailure { log("$name failed: ${it.message}") }
        }
    }

    private fun field(hint: String, value: String = ""): EditText =
        EditText(this).apply {
            this.hint = hint
            setText(value)
            inputType = InputType.TYPE_CLASS_TEXT
        }

    private fun button(label: String, action: (View) -> Unit): Button =
        Button(this).apply {
            text = label
            setOnClickListener { view -> action(view) }
        }

    private fun log(line: String) {
        output.append("$line\n")
    }

    override fun onDestroy() {
        engine?.close()
        p2pEngine?.close()
        scope.cancel()
        super.onDestroy()
    }
}
