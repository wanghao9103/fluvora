import SwiftUI

struct ContentView: View {
    @ObservedObject var model: DemoModel

    var body: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    TextField("API URL", text: $model.baseURL)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("Short-lived access token", text: $model.token)
                    TextField("Room ID", text: $model.roomID)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                Section("Room") {
                    Button("Create SFU room", action: model.createRoom)
                    Button("Join room", action: model.joinRoom)
                    Button("Connect SFU media", action: model.connectSFU)
                    Button("Leave and clean up", role: .destructive, action: model.leaveRoom)
                }
                Section("Data") {
                    TextField("Message", text: $model.message)
                    Button("Send chat + custom event", action: model.sendRoomData)
                }
                Section("Events") {
                    if model.isWorking {
                        ProgressView(model.status)
                    } else {
                        Text(model.status)
                            .foregroundStyle(.secondary)
                    }
                    Text(model.log.isEmpty ? "No events yet" : model.log)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                }
            }
            .navigationTitle("Fluvora SDK")
            .disabled(model.isWorking)
        }
    }
}

#Preview {
    ContentView(model: DemoModel())
}
