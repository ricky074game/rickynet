//
//  KeepAlive.swift
//  Keeps RickyNet running in the background while the tunnel is up, and turns
//  OFF completely (releasing the audio session) when it isn't.
//
//  iOS suspends a foreground app the moment it's backgrounded or the screen
//  locks — which killed the tunnel (see the ~37 s freeze in early logs). To
//  survive that, we play a SILENT, looping track under the `audio` background
//  mode (declared in Info.plist). iOS then treats RickyNet like a music app and
//  keeps it (and its socket server) alive across app-switching and screen-lock.
//  `.mixWithOthers` means it never interrupts the user's own music, and the
//  samples are zero + volume 0 so nothing is heard.
//
//  "Prevent shutdown": background audio can be silently torn down by the system
//  — an audio interruption (phone call), an output route change (headphones
//  unplugged), or a media-services reset. Each of those would let iOS suspend
//  the app. We observe all three and re-establish playback, so the tunnel stays
//  up. `start()`/`stop()` are the complete on/off — `stop()` releases the
//  session so, when off, the phone is fully back to normal (no background work,
//  no held audio session, screen sleeps as usual).
//

import AVFoundation
import Foundation

final class KeepAlive: NSObject {
    private var player: AVAudioPlayer?
    private(set) var active = false

    /// Turn background survival ON. Idempotent.
    func start() {
        guard !active else { return }
        active = true
        activateSession()
        buildAndPlay()
        registerObservers()
        LogStore.shared.app("keep-alive: ON — background survival engaged")
    }

    /// Turn background survival OFF completely: stop playback, drop observers,
    /// and deactivate the audio session so nothing lingers in the background.
    func stop() {
        guard active else { return }
        active = false
        NotificationCenter.default.removeObserver(self)
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        LogStore.shared.app("keep-alive: OFF — audio session released, fully stopped")
    }

    // MARK: - Audio session / player

    private func activateSession() {
        do {
            let s = AVAudioSession.sharedInstance()
            try s.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try s.setActive(true)
        } catch {
            LogStore.shared.app("keep-alive: session activate failed: \(error.localizedDescription)")
        }
    }

    private func buildAndPlay() {
        do {
            let p = try AVAudioPlayer(contentsOf: Self.silentClipURL())
            p.numberOfLoops = -1 // loop forever
            p.volume = 0
            p.delegate = self
            p.prepareToPlay()
            p.play()
            player = p
        } catch {
            LogStore.shared.app("keep-alive: player failed: \(error.localizedDescription); app is foreground-only")
        }
    }

    // MARK: - Robustness: re-establish on the events that kill background audio

    private func registerObservers() {
        let nc = NotificationCenter.default
        nc.addObserver(self, selector: #selector(onInterruption(_:)),
                       name: AVAudioSession.interruptionNotification, object: nil)
        nc.addObserver(self, selector: #selector(onRouteChange(_:)),
                       name: AVAudioSession.routeChangeNotification, object: nil)
        nc.addObserver(self, selector: #selector(onMediaReset(_:)),
                       name: AVAudioSession.mediaServicesWereResetNotification, object: nil)
    }

    /// Phone call etc.: resume when it ends so we don't lose background time.
    @objc private func onInterruption(_ note: Notification) {
        guard active,
              let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        if type == .ended {
            activateSession()
            player?.play()
            LogStore.shared.app("keep-alive: resumed after interruption")
        } else {
            LogStore.shared.app("keep-alive: audio interrupted (will resume)")
        }
    }

    /// Route changes (e.g. headphones unplugged) can pause playback — re-assert.
    @objc private func onRouteChange(_ note: Notification) {
        guard active else { return }
        if player?.isPlaying == false {
            activateSession()
            player?.play()
            LogStore.shared.app("keep-alive: re-asserted playback after route change")
        }
    }

    /// The media server died: session + player are now invalid — rebuild them,
    /// otherwise iOS would suspend the app.
    @objc private func onMediaReset(_ note: Notification) {
        guard active else { return }
        LogStore.shared.app("keep-alive: media services reset — rebuilding audio")
        player = nil
        activateSession()
        buildAndPlay()
    }

    // MARK: - Silent clip

    /// A tiny silent 16-bit PCM WAV (mono, 8 kHz, 0.5 s), written to a temp file
    /// once. Built by hand so no audio asset needs bundling.
    private static func silentClipURL() -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("rn-silence.wav")
        if FileManager.default.fileExists(atPath: url.path) { return url }

        let sampleRate: UInt32 = 8000
        let channels: UInt16 = 1
        let bits: UInt16 = 16
        let frames = UInt32(Double(sampleRate) * 0.5)
        let dataSize = frames * UInt32(channels) * UInt32(bits / 8)
        let byteRate = sampleRate * UInt32(channels) * UInt32(bits / 8)
        let blockAlign = channels * (bits / 8)

        var d = Data()
        func ascii(_ s: String) { d.append(s.data(using: .ascii)!) }
        func u32(_ v: UInt32) { var x = v.littleEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { var x = v.littleEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }

        ascii("RIFF"); u32(36 + dataSize); ascii("WAVE")
        ascii("fmt "); u32(16); u16(1); u16(channels); u32(sampleRate); u32(byteRate); u16(blockAlign); u16(bits)
        ascii("data"); u32(dataSize)
        d.append(Data(count: Int(dataSize))) // zero PCM = silence

        try? d.write(to: url)
        return url
    }
}

extension KeepAlive: AVAudioPlayerDelegate {
    /// Shouldn't happen with an infinite loop, but if playback ever ends,
    /// restart so background survival isn't lost.
    func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        guard active else { return }
        buildAndPlay()
    }

    func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        guard active else { return }
        LogStore.shared.app("keep-alive: decode error — restarting playback")
        buildAndPlay()
    }
}
