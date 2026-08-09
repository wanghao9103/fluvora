import Combine
import Fluvora
import Foundation

/// Contract implemented by the iOS application's chosen standards-compatible WebRTC engine.
///
/// The implementation owns camera/microphone capture, rendering, PeerConnection and DataChannel
/// lifecycle. Fluvora owns room-scoped ICE credentials and signaling.
protocol NativeWebRTCEngine: Sendable {
    func createRoomDataChannel(label: String, protocol: String) async throws
    func createAndSetLocalOffer() async throws -> String
    func setRemoteAnswer(sdp: String) async throws
    func close() async
}

typealias NativeWebRTCEngineFactory =
    @Sendable ([IceServer]) async throws -> any NativeWebRTCEngine

@MainActor
final class DemoModel: ObservableObject {
    @Published var baseURL = "http://127.0.0.1:8080"
    @Published var token = ""
    @Published var roomID = ""
    @Published var message = "hello"
    @Published private(set) var log = ""
    @Published private(set) var isWorking = false
    @Published private(set) var status = "Ready"

    private let engineFactory: NativeWebRTCEngineFactory?
    private var client: FluvoraClient?
    private var engine: (any NativeWebRTCEngine)?

    /// Pass the host application's native WebRTC factory to enable the SFU media button.
    init(engineFactory: NativeWebRTCEngineFactory? = nil) {
        self.engineFactory = engineFactory
        append("Inject NativeWebRTCEngineFactory to enable camera/microphone media.")
    }

    func createRoom() {
        run("create") {
            let sdk = try self.configuredClient()
            let room = try await sdk.createRoom(
                mode: .sfu,
                maxMembers: 64,
                maxPublishers: 16
            )
            self.roomID = room.roomId
            self.append("created room \(room.roomId)")
        }
    }

    func joinRoom() {
        run("join") {
            let result = try await self.configuredClient().join(roomId: self.requiredRoomID())
            self.append("joined at sequence \(result.sequence)")
        }
    }

    func connectSFU() {
        run("connect SFU") {
            let sdk = try self.configuredClient()
            let roomID = try self.requiredRoomID()
            let ice = try await sdk.getIceConfiguration(roomId: roomID)
            guard let factory = self.engineFactory else {
                throw DemoError("NativeWebRTCEngineFactory is not installed")
            }
            if let previous = self.engine {
                await previous.close()
            }
            let nativePeer = try await factory(ice.iceServers)
            self.engine = nativePeer
            _ = try await sdk.startPublishing(roomId: roomID)
            let peer = CallbackWebRTCPeer(
                createAndSetLocalOffer: {
                    try await nativePeer.createAndSetLocalOffer()
                },
                setRemoteAnswer: { answer in
                    try await nativePeer.setRemoteAnswer(sdp: answer)
                },
                prepareRoomDataChannel: {
                    try await nativePeer.createRoomDataChannel(
                        label: "fluvora.room.v1",
                        protocol: "fluvora.v1"
                    )
                }
            )
            let session = try await sdk.connectSFU(roomId: roomID, peer: peer)
            self.append("connected SFU session \(session.sessionId)")
        }
    }

    func sendRoomData() {
        run("send data") {
            let sdk = try self.configuredClient()
            let roomID = try self.requiredRoomID()
            _ = try await sdk.sendChat(roomId: roomID, text: self.message)
            _ = try await sdk.sendCustomData(
                roomId: roomID,
                namespace: "demo.ios",
                schemaVersion: 1,
                payload: .object(["message": .string(self.message)])
            )
            self.append("durable chat and custom event accepted")
        }
    }

    func leaveRoom() {
        run("leave") {
            if let nativePeer = self.engine {
                await nativePeer.close()
                self.engine = nil
            }
            guard let sdk = self.client, !self.roomID.isEmpty else {
                self.append("local media resources released")
                return
            }
            _ = try? await sdk.stopPublishing(roomId: self.roomID)
            _ = try await sdk.leave(roomId: self.roomID)
            self.append("PeerConnection, tracks, and room membership released")
        }
    }

    private func configuredClient() throws -> FluvoraClient {
        guard let endpoint = URL(string: baseURL), !token.isEmpty else {
            throw DemoError("API URL and short-lived token are required")
        }
        let sdk = try FluvoraClient(baseURL: endpoint, accessToken: token)
        client = sdk
        return sdk
    }

    private func requiredRoomID() throws -> String {
        guard !roomID.isEmpty else {
            throw DemoError("Room ID is required")
        }
        return roomID
    }

    private func run(
        _ name: String,
        operation: @escaping @MainActor () async throws -> Void
    ) {
        guard !isWorking else {
            return
        }
        isWorking = true
        status = name
        Task {
            defer {
                self.isWorking = false
            }
            do {
                try await operation()
                status = "Ready"
            } catch {
                status = "\(name) failed"
                append("\(name) failed: \(error)")
            }
        }
    }

    private func append(_ line: String) {
        log += "\(line)\n"
    }
}

private struct DemoError: Error, CustomStringConvertible, Sendable {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}
