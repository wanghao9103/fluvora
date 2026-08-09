package com.fluvora.sdk

import java.net.ServerSocket
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

public class FluvoraContractTest {
    @Test
    public fun serializesTrustedGiftReceiptContract() {
        val encoded = Json.encodeToString(
            VerifiedGift(
                provider = "payment-provider",
                providerTimestampMillis = 1_800_000_000_000,
                providerSignature = "base64url-signature",
                senderId = "00000000000000000000000000000001",
                recipientId = "00000000000000000000000000000002",
                transactionId = "transaction-42",
                giftId = "rocket",
                quantity = 2,
                unitValue = 500,
                currency = "CNY",
            ),
        )
        assertTrue(encoded.contains("\"provider_timestamp_millis\":1800000000000"))
        assertTrue(encoded.contains("\"provider_signature\":\"base64url-signature\""))
        assertTrue(encoded.contains("\"recipient_id\""))
    }

    @Test
    public fun preservesWireEnumsAndRejectsInvalidConfiguration() {
        assertEquals("\"sfu\"", Json.encodeToString(RoomMode.SFU))
        assertEquals("\"publisher\"", Json.encodeToString(MemberRole.PUBLISHER))
        for (baseUrl in listOf(
            "file:///tmp/fluvora",
            "https://token@api.example.com",
            "https://api.example.com?redirect=true",
            "https://api.example.com#fragment",
            "https://",
        )) {
            assertThrows(IllegalArgumentException::class.java) {
                FluvoraClient(baseUrl, "token")
            }
        }
        for (token in listOf("", "line\nbreak", "x".repeat(4_097))) {
            assertThrows(IllegalArgumentException::class.java) {
                FluvoraClient("https://api.example.com", token)
            }
        }
    }

    @Test
    public fun disablesRedirects(): Unit = withHttpResponse(
        "HTTP/1.1 302 Found\r\n" +
            "Location: http://127.0.0.1:9/\r\n" +
            "Content-Length: 0\r\n" +
            "Connection: close\r\n\r\n",
    ) { baseUrl ->
        val error = runCatching {
            FluvoraClient(baseUrl, "token").getRoom("01")
        }.exceptionOrNull()
        assertTrue(error is FluvoraException)
        assertEquals(302, (error as FluvoraException).status)
    }

    @Test
    public fun rejectsOversizedResponsesBeforeReadingTheBody(): Unit = withHttpResponse(
        "HTTP/1.1 200 OK\r\n" +
            "Content-Type: application/json\r\n" +
            "Content-Length: 33554433\r\n" +
            "Connection: close\r\n\r\n{}",
    ) { baseUrl ->
        val error = runCatching {
            FluvoraClient(baseUrl, "token").getRoom("01")
        }.exceptionOrNull()
        assertTrue(error is FluvoraException)
        assertEquals("response_too_large", (error as FluvoraException).code)
    }

    @Test
    public fun preservesBaseUrlPathPrefix(): Unit = withHttpResponse(
        "HTTP/1.1 200 OK\r\n" +
            "Content-Type: application/json\r\n" +
            "Content-Length: 93\r\n" +
            "Connection: close\r\n\r\n" +
            "{\"room_id\":\"01\",\"mode\":\"sfu\",\"sequence\":1,\"ended\":false," +
            "\"member_count\":0,\"publisher_count\":0}",
        expectedRequestTarget = "/control/v1/rooms/01",
    ) { baseUrl ->
        assertEquals("01", FluvoraClient("$baseUrl/control/", "token").getRoom("01").roomId)
    }

    @Test
    public fun callbackWebRtcAdapterPreservesNegotiationOrder(): Unit = runBlocking {
        val calls = mutableListOf<String>()
        val peer = CallbackWebRtcPeer(
            createOffer = {
                calls += "offer"
                "v=0"
            },
            applyRemoteAnswer = { calls += "answer:$it" },
            createRoomDataChannel = { calls += "data-channel" },
        )
        peer.prepareRoomDataChannel()
        assertEquals("v=0", peer.createAndSetLocalOffer())
        peer.setRemoteAnswer("v=0 answer")
        assertEquals(listOf("data-channel", "offer", "answer:v=0 answer"), calls)
    }

    @Test
    public fun rejectsOversizedControlPayloadsBeforeNetwork(): Unit = runBlocking {
        val client = FluvoraClient("http://127.0.0.1:1", "token")
        assertTrue(runCatching { client.sendChat("01", "x".repeat(4_097)) }.exceptionOrNull() is IllegalArgumentException)
        assertTrue(
            runCatching { client.sendCustomData("01", ".invalid", 1, JsonPrimitive(true)) }
                .exceptionOrNull() is IllegalArgumentException,
        )
        assertTrue(
            runCatching {
                client.sendCustomData("01", "com.example.state", 1, JsonPrimitive("x".repeat(60 * 1_024)))
            }.exceptionOrNull() is IllegalArgumentException,
        )
        assertTrue(
            runCatching {
                client.postSignal("01", null, "offer", JsonPrimitive("x".repeat(64 * 1_024)))
            }.exceptionOrNull() is IllegalArgumentException,
        )
        assertTrue(
            runCatching {
                client.uploadAssetChunk("asset", 0, ByteArray(8 * 1_024 * 1_024 + 1))
            }.exceptionOrNull() is IllegalArgumentException,
        )
        assertTrue(
            runCatching { client.uploadLiveInit("stream", ByteArray(0)) }
                .exceptionOrNull() is IllegalArgumentException,
        )
    }

    @Test
    public fun pollsBoundedSignalPages(): Unit = withHttpResponse(
        "HTTP/1.1 200 OK\r\n" +
            "Content-Type: application/json\r\n" +
            "Content-Length: 34\r\n" +
            "Connection: close\r\n\r\n" +
            "{\"signals\":[],\"latest_sequence\":0}",
        expectedRequestTarget = "/v1/rooms/01/signals?after=0&limit=128",
    ) { baseUrl ->
        assertEquals(0, FluvoraClient(baseUrl, "token").pollSignals("01").signals.size)
    }

    private fun withHttpResponse(
        response: String,
        expectedRequestTarget: String? = null,
        block: suspend (String) -> Unit,
    ) {
        ServerSocket(0).use { server ->
            val requestLine = AtomicReference<String>()
            val handler = thread(name = "fluvora-sdk-test-server") {
                server.accept().use { socket ->
                    val reader = socket.getInputStream().bufferedReader(Charsets.US_ASCII)
                    requestLine.set(reader.readLine())
                    while (!reader.readLine().isNullOrEmpty()) {
                        // Consume request headers before writing the deterministic response.
                    }
                    socket.getOutputStream().use { output ->
                        output.write(response.toByteArray(Charsets.US_ASCII))
                    }
                }
            }
            try {
                runBlocking { block("http://127.0.0.1:${server.localPort}") }
            } finally {
                handler.join(5_000)
            }
            if (expectedRequestTarget != null) {
                val line = requestLine.get()
                assertTrue(line != null && line.startsWith("GET $expectedRequestTarget HTTP/"))
            }
        }
    }
}
