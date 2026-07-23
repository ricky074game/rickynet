//
//  LogStore.swift
//  Collects every log line — from the Rust core (via rn_log_set_callback) and
//  from the Swift shell — into an in-memory buffer for the live Logs view, and
//  appends them to Documents/rickynet.log. That file is visible in the Files
//  app (UIFileSharingEnabled) and shareable from the Logs screen, so a remote
//  tester can send the full log back.
//

import Foundation
import UIKit

final class LogStore: ObservableObject {
    static let shared = LogStore()

    /// Lines shown in the UI (bounded; the file keeps everything).
    @Published private(set) var lines: [String] = []
    private let maxLines = 2000

    let fileURL: URL
    private let queue = DispatchQueue(label: "net.ricky.rickynet.logstore")
    private var handle: FileHandle?
    // UI updates are coalesced: lines land in `uiBuffer` on `queue` and are
    // flushed to the @Published array at most ~5x/sec, so even a burst of log
    // lines can never flood the main thread (that flooding was what made the
    // app crawl). File writes still happen per line on `queue`.
    private var uiBuffer: [String] = []
    private var flushScheduled = false

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss.SSS"
        f.locale = Locale(identifier: "en_US_POSIX")
        return f
    }()

    private init() {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        fileURL = docs.appendingPathComponent("rickynet.log")
        queue.sync { openFile() }
        logSessionHeader()
    }

    /// The Rust core is registered exactly once, before the first start.
    static func installCoreLogHook() {
        rn_log_set_callback { cstr in
            guard let cstr else { return }
            LogStore.shared.append(String(cString: cstr))
        }
    }

    /// A Swift-side event; prefixed so core lines and app lines are
    /// distinguishable in the merged log.
    func app(_ message: String) {
        append("[\(Self.timeFormatter.string(from: Date())) APP] \(message)")
    }

    /// Thread-safe: called from arbitrary Rust threads and from the main actor.
    func append(_ line: String) {
        queue.async { [weak self] in
            guard let self else { return }
            if let data = (line + "\n").data(using: .utf8) {
                try? self.handle?.write(contentsOf: data)
            }
            self.uiBuffer.append(line)
            if !self.flushScheduled {
                self.flushScheduled = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in
                    self?.flushUI()
                }
            }
        }
    }

    /// Publish the buffered lines to the UI in one batch.
    private func flushUI() {
        queue.async { [weak self] in
            guard let self else { return }
            self.flushScheduled = false
            let batch = self.uiBuffer
            self.uiBuffer.removeAll(keepingCapacity: true)
            guard !batch.isEmpty else { return }
            DispatchQueue.main.async {
                self.lines.append(contentsOf: batch)
                if self.lines.count > self.maxLines {
                    self.lines.removeFirst(self.lines.count - self.maxLines)
                }
            }
        }
    }

    func clear() {
        queue.async { [weak self] in
            guard let self else { return }
            self.uiBuffer.removeAll(keepingCapacity: true)
            try? self.handle?.truncate(atOffset: 0)
            try? self.handle?.seek(toOffset: 0)
        }
        DispatchQueue.main.async { [weak self] in
            self?.lines.removeAll()
        }
        logSessionHeader()
        app("log cleared by user")
    }

    /// Everything currently in the UI buffer, for copy/share as text.
    var joined: String { lines.joined(separator: "\n") }

    // MARK: - File plumbing

    private func openFile() {
        let fm = FileManager.default
        // Rotate at ~4 MB so the file a tester shares stays sendable.
        if let attrs = try? fm.attributesOfItem(atPath: fileURL.path),
           let size = attrs[.size] as? UInt64, size > 4_000_000 {
            let old = fileURL.deletingLastPathComponent().appendingPathComponent("rickynet.old.log")
            try? fm.removeItem(at: old)
            try? fm.moveItem(at: fileURL, to: old)
        }
        if !fm.fileExists(atPath: fileURL.path) {
            fm.createFile(atPath: fileURL.path, contents: nil)
        }
        handle = try? FileHandle(forWritingTo: fileURL)
        _ = try? handle?.seekToEnd()
    }

    private func logSessionHeader() {
        let bundle = Bundle.main
        let version = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        let build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"
        let dev = UIDevice.current
        var sys = utsname()
        uname(&sys)
        let model = withUnsafePointer(to: &sys.machine) {
            $0.withMemoryRebound(to: CChar.self, capacity: 1) { String(cString: $0) }
        }
        app("=== RickyNet iOS v\(version) (\(build)) session start ===")
        app("device: \(model), iOS \(dev.systemVersion)")
        app("log file: \(fileURL.path)")
    }
}
