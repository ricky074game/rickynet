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
    /// Live transfer rate in bytes/sec, refreshed every second by the poll timer.
    @Published private(set) var rxRate: Double = 0 // download (internet → PC)
    @Published private(set) var txRate: Double = 0 // upload   (PC → internet)

    // Previous sample, for computing the per-second rate.
    private var lastRx: UInt64 = 0
    private var lastTx: UInt64 = 0
    private var lastSample: Date?

    /// Accept connections over Wi-Fi/LAN as well as USB. When false we bind
    /// loopback only (usbmux still reaches us at 127.0.0.1), which is the more
    /// private default; when true we bind 0.0.0.0 so a desktop on the same LAN
    /// can connect directly.
    @Published var allowWifi: Bool = false

    /// Listener port. Mirrors rickynet_wire::DEFAULT_PORT.
    private let port: UInt16 = 27600
    private var timer: Timer?
    private let keepAlive = KeepAlive()

    var isRunning: Bool { status == .running }

    init() {
        // Route every Rust core log line into LogStore (and its file) before
        // anything else can run.
        LogStore.installCoreLogHook()
    }

    func start() {
        // transport: 0 = bind 127.0.0.1 (USB/loopback only), 1 = bind 0.0.0.0.
        let transport: UInt32 = allowWifi ? 1 : 0
        LogStore.shared.app("start tapped (port \(port), transport \(transport == 0 ? "USB/loopback" : "USB+Wi-Fi"))")
        let rc = rn_start(port, transport)
        if rc == 0 {
            LogStore.shared.app("core started OK")
            status = .running
            // Silent-audio keep-alive lets the app survive backgrounding and
            // screen-lock, so we DON'T force the screen on (the display was a
            // major heat/battery drain). The friend can lock the phone.
            keepAlive.start()
            startPolling()
        } else {
            LogStore.shared.app("core start FAILED: rc=\(rc) (\(Self.describe(rc)))")
            status = .error(Self.describe(rc))
        }
    }

    func stop() {
        let rc = rn_stop()
        LogStore.shared.app("stop tapped (rn_stop rc=\(rc); session rx \(rxBytes) B, tx \(txBytes) B)")
        keepAlive.stop()
        stopPolling()
        rxBytes = 0
        txBytes = 0
        rxRate = 0
        txRate = 0
        lastSample = nil
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
        lastRx = 0
        lastTx = 0
        lastSample = nil
        let t = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            var rx: UInt64 = 0
            var tx: UInt64 = 0
            rn_stats(&rx, &tx)
            let now = Date()
            Task { @MainActor in
                guard let self else { return }
                // Per-second rate = bytes since last sample / elapsed seconds.
                if let last = self.lastSample {
                    let dt = now.timeIntervalSince(last)
                    if dt > 0 {
                        self.rxRate = Double(rx &- self.lastRx) / dt
                        self.txRate = Double(tx &- self.lastTx) / dt
                    }
                }
                self.lastRx = rx
                self.lastTx = tx
                self.lastSample = now
                self.rxBytes = rx
                self.txBytes = tx
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
