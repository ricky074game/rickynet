//
//  LogView.swift
//  Live log viewer: every line from the Rust core and the Swift shell, with
//  Share (sends the full rickynet.log file), Copy, and Clear.
//

import SwiftUI

struct LogView: View {
    @ObservedObject private var store = LogStore.shared
    @Environment(\.dismiss) private var dismiss
    @State private var autoScroll = true

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(store.lines.enumerated()), id: \.offset) { _, line in
                            Text(line)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(color(for: line))
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                        }
                        Color.clear.frame(height: 1).id("bottom")
                    }
                    .padding(.horizontal, 8)
                }
                .onChange(of: store.lines.count) { _ in
                    if autoScroll {
                        proxy.scrollTo("bottom", anchor: .bottom)
                    }
                }
                .onAppear {
                    proxy.scrollTo("bottom", anchor: .bottom)
                }
            }
            .navigationTitle("Logs")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .navigationBarLeading) {
                    ShareLink(item: store.fileURL) {
                        Image(systemName: "square.and.arrow.up")
                    }
                    Button {
                        UIPasteboard.general.string = store.joined
                    } label: {
                        Image(systemName: "doc.on.doc")
                    }
                    Button(role: .destructive) {
                        store.clear()
                    } label: {
                        Image(systemName: "trash")
                    }
                    Toggle(isOn: $autoScroll) {
                        Image(systemName: "arrow.down.to.line")
                    }
                    .toggleStyle(.button)
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func color(for line: String) -> Color {
        if line.contains(" ERROR ") { return .red }
        if line.contains(" WARN ") { return .orange }
        if line.contains(" APP] ") { return .blue }
        return .primary
    }
}

#Preview {
    LogView()
}
