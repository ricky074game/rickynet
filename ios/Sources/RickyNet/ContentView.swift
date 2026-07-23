//
//  ContentView.swift
//  RickyNet — Start/Stop, a status light, and byte counters.
//

import SwiftUI

struct ContentView: View {
    @StateObject private var core = RickyNetCore()
    @ObservedObject private var logs = LogStore.shared
    @State private var showLogs = false

    var body: some View {
        VStack(spacing: 28) {
            header

            statusLight

            Button(action: { core.toggle() }) {
                Text(core.isRunning ? "Stop" : "Start")
                    .font(.title2.bold())
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 16)
            }
            .buttonStyle(.borderedProminent)
            .tint(core.isRunning ? .red : .green)
            .padding(.horizontal)

            Toggle("Accept over Wi-Fi (LAN)", isOn: $core.allowWifi)
                .disabled(core.isRunning)
                .padding(.horizontal)

            counters

            Button(action: { showLogs = true }) {
                Label("Logs (\(logs.lines.count))", systemImage: "doc.text.magnifyingglass")
                    .font(.callout)
            }
            .buttonStyle(.bordered)

            Spacer()

            Text("Keep this screen open. RickyNet is foreground-only — if you\nswitch apps or lock the phone, the link drops.")
                .font(.footnote)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal)
                .padding(.bottom, 8)
        }
        .padding(.top, 40)
        .sheet(isPresented: $showLogs) {
            LogView()
        }
    }

    private var header: some View {
        VStack(spacing: 4) {
            Text("RickyNet")
                .font(.largeTitle.bold())
            Text("iPhone cellular → desktop, without Personal Hotspot")
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
    }

    private var statusLight: some View {
        HStack(spacing: 10) {
            Circle()
                .fill(statusColor)
                .frame(width: 16, height: 16)
                .shadow(color: statusColor.opacity(0.7), radius: core.isRunning ? 6 : 0)
            Text(statusText)
                .font(.headline)
        }
    }

    private var counters: some View {
        HStack(spacing: 40) {
            counter(symbol: "arrow.down", label: "Down", bytes: core.rxBytes, color: .blue)
            counter(symbol: "arrow.up", label: "Up", bytes: core.txBytes, color: .purple)
        }
    }

    private func counter(symbol: String, label: String, bytes: UInt64, color: Color) -> some View {
        VStack(spacing: 4) {
            Label(label, systemImage: symbol)
                .font(.caption)
                .foregroundStyle(color)
            Text(Self.humanBytes(bytes))
                .font(.system(.title3, design: .monospaced))
                .contentTransition(.numericText())
        }
    }

    private var statusColor: Color {
        switch core.status {
        case .stopped: return .gray
        case .running: return .green
        case .error: return .red
        }
    }

    private var statusText: String {
        switch core.status {
        case .stopped: return "Stopped"
        case .running: return "Running"
        case .error(let msg): return "Error: \(msg)"
        }
    }

    static func humanBytes(_ n: UInt64) -> String {
        let units = ["B", "KB", "MB", "GB", "TB"]
        var value = Double(n)
        var i = 0
        while value >= 1024 && i < units.count - 1 {
            value /= 1024
            i += 1
        }
        return i == 0 ? "\(n) B" : String(format: "%.1f %@", value, units[i])
    }
}

#Preview {
    ContentView()
}
