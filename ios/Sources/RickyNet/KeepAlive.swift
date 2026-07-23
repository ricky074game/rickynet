//
//  KeepAlive.swift
//  Keeps RickyNet running in the background while the tunnel is up.
//
//  iOS suspends a foreground app the moment it's backgrounded or the screen
//  locks — which killed the tunnel (see the ~37 s freeze in early logs). To
//  survive that, we play a SILENT, looping audio track under the `audio`
//  background mode (declared in Info.plist). iOS then treats RickyNet like a
//  music app and keeps it (and its socket server) alive across app-switching
//  and screen-lock. `.mixWithOthers` means it never interrupts the user's own
//  music or podcasts, and the samples are zero + volume 0 so nothing is heard.
//
//  This is a deliberate keep-alive hack, appropriate for a sideloaded personal
//  tool. It is not bulletproof: under heavy memory pressure iOS can still
//  reclaim the app, and an audio interruption (e.g. a phone call) is handled by
//  resuming playback when it ends.
//

import AVFoundation
import Foundation

final class KeepAlive {
    private var player: AVAudioPlayer?
    private var observing = false

    /// Begin background survival. Safe to call repeatedly.
    func start() {
        guard player == nil else { return }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try session.setActive(true)

            let p = try AVAudioPlayer(contentsOf: Self.silentClipURL())
            p.numberOfLoops = -1 // loop forever
            p.volume = 0
            p.prepareToPlay()
            p.play()
            player = p

            if !observing {
                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(handleInterruption(_:)),
                    name: AVAudioSession.interruptionNotification,
                    object: session
                )
                observing = true
            }
            LogStore.shared.app("keep-alive: silent audio started — background survival ON")
        } catch {
            LogStore.shared.app("keep-alive: FAILED to start (\(error.localizedDescription)); app is foreground-only")
        }
    }

    /// End background survival and release the audio session.
    func stop() {
        guard player != nil else { return }
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        LogStore.shared.app("keep-alive: silent audio stopped — background survival OFF")
    }

    /// A phone call etc. interrupts playback; resume when it ends so we don't
    /// silently lose background time.
    @objc private func handleInterruption(_ note: Notification) {
        guard
            let info = note.userInfo,
            let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        switch type {
        case .began:
            LogStore.shared.app("keep-alive: audio interrupted (background survival paused)")
        case .ended:
            guard player != nil else { return }
            try? AVAudioSession.sharedInstance().setActive(true)
            player?.play()
            LogStore.shared.app("keep-alive: audio resumed after interruption")
        @unknown default:
            break
        }
    }

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
