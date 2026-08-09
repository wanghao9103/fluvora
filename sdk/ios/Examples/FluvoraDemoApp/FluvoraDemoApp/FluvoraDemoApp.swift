import SwiftUI

@main
struct FluvoraDemoApp: App {
    @StateObject private var model = DemoModel()

    var body: some Scene {
        WindowGroup {
            ContentView(model: model)
        }
    }
}
