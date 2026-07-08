//
//  RickyNetCore.swift
//  Thin Swift wrapper over the rickynet-core C ABI. Owns the run state and a
//  1 Hz timer that polls rn_stats for the byte counters.
//

import Foundation
import SwiftUI
import UIKit

@MainActor
final class RickyNetCore: ObservableObject {
    enum Status: Equatable {
        case stopped
        case running
        case error(String)
    }

    @Published private(set) var status: Status = .stopped
    @Published private(set) var rxBytes: UInt64 = 0
    @Published private(set) var txBytes: UInt64 = 0

    /// Accept connections over Wi-Fi/LAN as well as USB. When false we bind
    /// loopback only (usbmux still reaches us at 127.0.0.1), which is the more
    /// private default; when true we bind 0.0.0.0 so a desktop on the same LAN
    /// can connect directly.
    @Published var allowWifi: Bool = false

    /// Listener port. Mirrors rickynet_wire::DEFAULT_PORT.
    private let port: UInt16 = 27600
    private var timer: Timer?

    var isRunning: Bool { status == .running }

    func start() {
        // transport: 0 = bind 127.0.0.1 (USB/loopback only), 1 = bind 0.0.0.0.
        let transport: UInt32 = allowWifi ? 1 : 0
        let rc = rn_start(port, transport)
        if rc == 0 {
            status = .running
            // Keep the screen (and thus the app + its socket server) awake.
            UIApplication.shared.isIdleTimerDisabled = true
            startPolling()
        } else {
            status = .error(Self.describe(rc))
        }
    }

    func stop() {
        _ = rn_stop()
        UIApplication.shared.isIdleTimerDisabled = false
        stopPolling()
        rxBytes = 0
        txBytes = 0
        status = .stopped
    }

    func toggle() {
        if isRunning {
            stop()
        } else {
            start()
        }
    }

    private func startPolling() {
        stopPolling()
        let t = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            var rx: UInt64 = 0
            var tx: UInt64 = 0
            rn_stats(&rx, &tx)
            Task { @MainActor in
                self?.rxBytes = rx
                self?.txBytes = tx
            }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func stopPolling() {
        timer?.invalidate()
        timer = nil
    }

    private static func describe(_ rc: Int32) -> String {
        switch rc {
        case -2: return "already running"
        case -3: return "could not bind port (in use?)"
        case -4: return "internal runtime error"
        default: return "start failed (code \(rc))"
        }
    }
}
