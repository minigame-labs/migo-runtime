import Foundation

/// Why a recording did not start, or ended on its own: the words after `recorderManager.start:fail` in the
/// `errMsg` content's `onError` hears.
struct MigoRecordingError: Error, Equatable {
    let reason: String

    init(_ reason: String) {
        self.reason = reason
    }

    var errMsg: String { "recorderManager.start:fail \(reason)" }
}

/// What content asked of one recording: `RecorderManager.start`'s options as the recorder service's START
/// carries them, an option content left out taking the reference's default.
struct MigoRecordingOptions: Equatable {
    enum Format: Equatable {
        /// 16-bit samples, raw.
        case pcm
        /// 16-bit samples behind a WAV header.
        case wav
        /// AAC, which "mp3" records too: the system encodes no MP3, as Android's does not.
        case aac
    }

    /// The sample rates the reference lists.
    static let sampleRates: Set<Int> = [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000]
    /// The longest recording the reference allows: ten minutes.
    static let longestMs = 600_000

    let recordingId: UInt32
    let durationMs: Int
    let sampleRate: Int
    let channels: Int
    let bitRate: Int
    let format: Format
    /// Bytes to a frame handed to `onFrameRecorded`; 0 when content asked for none.
    let frameBytes: Int
    /// `auto`, or the one microphone content named: `buildInMic`, `headsetMic`.
    let audioSource: String

    /// The recording a request names; 0 when it names none, which no recording is numbered.
    static func recordingId(of object: [String: Any]) -> UInt32 {
        (object["recordingId"] as? NSNumber)?.uint32Value ?? 0
    }

    init(_ object: [String: Any]) throws {
        func number(_ key: String, default value: Int) -> Int {
            (object[key] as? NSNumber)?.intValue ?? value
        }
        recordingId = Self.recordingId(of: object)
        durationMs = number("duration", default: 60_000)
        sampleRate = number("sampleRate", default: 8000)
        channels = number("numberOfChannels", default: 2)
        bitRate = number("encodeBitRate", default: 48_000)
        frameBytes = max(0, number("frameSize", default: 0)) * 1024
        audioSource = object["audioSource"] as? String ?? "auto"
        switch (object["format"] as? String ?? "aac").lowercased() {
        case "pcm": format = .pcm
        case "wav": format = .wav
        case "aac", "mp3": format = .aac
        default: throw MigoRecordingError("invalid audio parameters")
        }
        guard Self.sampleRates.contains(sampleRate), (1...2).contains(channels), bitRate > 0,
            (1...Self.longestMs).contains(durationMs)
        else {
            throw MigoRecordingError("invalid audio parameters")
        }
    }

    /// The most the recording holds, in frames at its own rate: its duration, to the sample.
    var frameLimit: Int64 {
        Int64(durationMs) * Int64(sampleRate) / 1000
    }
}
