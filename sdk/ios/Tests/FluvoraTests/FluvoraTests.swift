import XCTest
@testable import Fluvora

private actor CallRecorder {
    private var calls: [String] = []

    func append(_ call: String) {
        calls.append(call)
    }

    func snapshot() -> [String] {
        calls
    }
}

final class FluvoraTests: XCTestCase {
    func testRoomModeWireValues() throws {
        XCTAssertEqual(RoomMode.sfu.rawValue, "sfu")
        XCTAssertEqual(RoomMode.p2p.rawValue, "p2p")
    }

    func testRejectsAmbiguousBaseURLsAndUnsafeTokens() throws {
        for rawURL in [
            "file:///tmp/fluvora",
            "https://token@api.example.com",
            "https://api.example.com?redirect=true",
            "https://api.example.com#fragment",
        ] {
            let url = try XCTUnwrap(URL(string: rawURL))
            XCTAssertThrowsError(try FluvoraClient(baseURL: url, accessToken: "token")) { error in
                XCTAssertEqual((error as? FluvoraAPIError)?.code, "invalid_configuration")
            }
        }
        for token in ["", "line\nbreak", String(repeating: "x", count: 4_097)] {
            XCTAssertThrowsError(
                try FluvoraClient(
                    baseURL: XCTUnwrap(URL(string: "https://api.example.com")),
                    accessToken: token
                )
            ) { error in
                XCTAssertEqual((error as? FluvoraAPIError)?.code, "invalid_configuration")
            }
        }
    }

    func testSignalJSONValueRoundTrip() throws {
        let payload = JSONValue.object([
            "sdp": .string("v=0"),
            "restart": .bool(true),
            "candidates": .array([.number(2), .null]),
        ])
        let encoded = try JSONEncoder().encode(payload)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: encoded), payload)
    }

    func testGiftReceiptWireContract() throws {
        let gift = VerifiedGift(
            provider: "payment-provider",
            providerTimestampMillis: 1_800_000_000_000,
            providerSignature: "base64url-signature",
            senderId: "00000000000000000000000000000001",
            recipientId: "00000000000000000000000000000002",
            transactionId: "transaction-42",
            giftId: "rocket",
            quantity: 2,
            unitValue: 500,
            currency: "CNY"
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(gift)) as? [String: Any]
        )
        XCTAssertEqual(
            (object["provider_timestamp_millis"] as? NSNumber)?.uint64Value,
            1_800_000_000_000
        )
        XCTAssertEqual(object["provider_signature"] as? String, "base64url-signature")
        XCTAssertEqual(object["recipient_id"] as? String, gift.recipientId)
    }

    func testRejectsUnsafeRoomIdentifierBeforeTransport() async throws {
        let client = try FluvoraClient(
            baseURL: try XCTUnwrap(URL(string: "https://api.example.com")),
            accessToken: "token"
        )
        do {
            _ = try await client.getRoom(roomId: "../escape")
            XCTFail("unsafe identifier should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_identifier")
        }
    }

    func testRejectsOversizedControlPayloadsBeforeTransport() async throws {
        let client = try FluvoraClient(
            baseURL: try XCTUnwrap(URL(string: "http://127.0.0.1:1")),
            accessToken: "token"
        )
        do {
            _ = try await client.sendChat(roomId: "01", text: String(repeating: "x", count: 4_097))
            XCTFail("oversized chat should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_argument")
        }
        do {
            _ = try await client.sendCustomData(
                roomId: "01",
                namespace: ".invalid",
                schemaVersion: 1,
                payload: .bool(true)
            )
            XCTFail("unsafe namespace should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_argument")
        }
        do {
            _ = try await client.postSignal(
                roomId: "01",
                recipientId: nil,
                kind: "offer",
                payload: .string(String(repeating: "x", count: 64 * 1_024))
            )
            XCTFail("oversized signal should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_argument")
        }
        do {
            _ = try await client.uploadAssetChunk(
                assetId: "asset",
                offset: 0,
                data: Data(count: 8 * 1_024 * 1_024 + 1)
            )
            XCTFail("oversized media upload should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_argument")
        }
        do {
            try await client.uploadLiveInit(streamId: "stream", data: Data())
            XCTFail("empty media upload should be rejected")
        } catch let error as FluvoraAPIError {
            XCTAssertEqual(error.code, "invalid_argument")
        }
    }

    func testCallbackWebRTCAdapterPreservesNegotiationOrder() async throws {
        let recorder = CallRecorder()
        let peer = CallbackWebRTCPeer(
            createAndSetLocalOffer: {
                await recorder.append("offer")
                return "v=0"
            },
            setRemoteAnswer: {
                await recorder.append("answer:\($0)")
            },
            prepareRoomDataChannel: {
                await recorder.append("data-channel")
            }
        )
        try await peer.prepareRoomDataChannel()
        let offer = try await peer.createAndSetLocalOffer()
        XCTAssertEqual(offer, "v=0")
        try await peer.setRemoteAnswer(sdp: "v=0 answer")
        let calls = await recorder.snapshot()
        XCTAssertEqual(
            calls,
            ["data-channel", "offer", "answer:v=0 answer"]
        )
    }
}
