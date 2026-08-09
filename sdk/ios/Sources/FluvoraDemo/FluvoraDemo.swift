import Foundation
import Fluvora
#if canImport(Darwin)
import Darwin
#endif

private actor AnswerBox {
    private var answer: String?

    func set(_ value: String) {
        answer = value
    }

    func take() -> String? {
        defer { answer = nil }
        return answer
    }
}

@main
private enum FluvoraDemo {
    static func main() async {
        do {
            try await run()
        } catch {
            FileHandle.standardError.write(Data("error: \(error)\n".utf8))
            usage()
            exit(EXIT_FAILURE)
        }
    }

    private static func run() async throws {
        let arguments = CommandLine.arguments
        guard arguments.count >= 2 else {
            throw DemoError("missing command")
        }
        let environment = ProcessInfo.processInfo.environment
        let baseURLValue = environment["FLUVORA_BASE_URL"] ?? "http://127.0.0.1:8080"
        guard let baseURL = URL(string: baseURLValue),
              let token = environment["FLUVORA_ACCESS_TOKEN"],
              !token.isEmpty
        else {
            throw DemoError("FLUVORA_ACCESS_TOKEN and a valid FLUVORA_BASE_URL are required")
        }
        let client = try FluvoraClient(baseURL: baseURL, accessToken: token)

        switch arguments[1] {
        case "create":
            let rawMode = try argument(arguments, 2, "mode")
            guard let mode = RoomMode(rawValue: rawMode) else {
                throw DemoError("mode must be sfu, p2p, live, or vod")
            }
            let room = try await client.createRoom(
                mode: mode,
                maxMembers: 64,
                maxPublishers: 16
            )
            try printJSON(room)
        case "join":
            try await printJSON(client.join(roomId: argument(arguments, 2, "room-id")))
        case "chat":
            let result = try await client.sendChat(
                roomId: argument(arguments, 2, "room-id"),
                text: argument(arguments, 3, "text")
            )
            try printJSON(result)
        case "custom":
            let result = try await client.sendCustomData(
                roomId: argument(arguments, 2, "room-id"),
                namespace: "demo.swift",
                schemaVersion: 1,
                payload: .object(["message": .string(argument(arguments, 3, "text"))])
            )
            try printJSON(result)
        case "ice":
            try await printJSON(
                client.getIceConfiguration(roomId: argument(arguments, 2, "room-id"))
            )
        case "sfu-offer":
            let roomId = try argument(arguments, 2, "room-id")
            let offerURL = URL(fileURLWithPath: try argument(arguments, 3, "offer.sdp"))
            let answerURL = URL(fileURLWithPath: try argument(arguments, 4, "answer.sdp"))
            let offer = try String(contentsOf: offerURL, encoding: .utf8)
            let answerBox = AnswerBox()
            let peer = CallbackWebRTCPeer(
                createAndSetLocalOffer: { offer },
                setRemoteAnswer: { answer in await answerBox.set(answer) },
                prepareRoomDataChannel: {
                    // Create reliable/ordered `fluvora.room.v1` in the host engine here.
                }
            )
            let session = try await client.connectSFU(roomId: roomId, peer: peer)
            guard let answer = await answerBox.take() else {
                throw DemoError("server answer callback was not invoked")
            }
            try answer.write(to: answerURL, atomically: true, encoding: .utf8)
            try printJSON(session)
        case "p2p-signal":
            let roomId = try argument(arguments, 2, "room-id")
            let recipient = try argument(arguments, 3, "recipient-id")
            let kind = try argument(arguments, 4, "kind")
            let payload = try decodeJSONValue(argument(arguments, 5, "payload-json"))
            try await printJSON(
                client.postSignal(
                    roomId: roomId,
                    recipientId: recipient == "-" ? nil : recipient,
                    kind: kind,
                    payload: payload
                )
            )
        case "poll":
            let roomId = try argument(arguments, 2, "room-id")
            let after = arguments.count > 3 ? UInt64(arguments[3]) ?? 0 : 0
            try await printJSON(client.pollSignals(roomId: roomId, after: after))
        case "leave":
            try await printJSON(client.leave(roomId: argument(arguments, 2, "room-id")))
        default:
            throw DemoError("unknown command \(arguments[1])")
        }
    }

    private static func argument(
        _ arguments: [String],
        _ index: Int,
        _ name: String
    ) throws -> String {
        guard arguments.indices.contains(index) else {
            throw DemoError("missing \(name)")
        }
        return arguments[index]
    }

    private static func decodeJSONValue(_ source: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(source.utf8))
    }

    private static func printJSON<T: Encodable>(_ value: T) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(value)
        guard let json = String(data: data, encoding: .utf8) else {
            throw DemoError("response was not UTF-8")
        }
        print(json)
    }

    private static func usage() {
        FileHandle.standardError.write(
            Data(
                """
                usage: fluvora-swift-demo <command> [arguments]
                  create <sfu|p2p|live|vod>
                  join <room-id>
                  chat <room-id> <text>
                  custom <room-id> <text>
                  ice <room-id>
                  sfu-offer <room-id> <offer.sdp> <answer.sdp>
                  p2p-signal <room-id> <recipient-id|-> <kind> <payload-json>
                  poll <room-id> [after]
                  leave <room-id>
                environment: FLUVORA_BASE_URL and required FLUVORA_ACCESS_TOKEN

                """.utf8
            )
        )
    }
}

private struct DemoError: Error, CustomStringConvertible, Sendable {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}
