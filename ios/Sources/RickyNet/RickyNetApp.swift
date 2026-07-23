//
//  RickyNetApp.swift
//  SwiftUI entry point for the RickyNet iOS app.
//

import SwiftUI

@main
struct RickyNetApp: App {
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        // The app is foreground-only, so lifecycle transitions are the #1 cause
        // of "it stopped working" — make every one of them visible in the log.
        .onChange(of: scenePhase) { phase in
            LogStore.shared.app("scene phase → \(String(describing: phase))")
        }
    }
}
