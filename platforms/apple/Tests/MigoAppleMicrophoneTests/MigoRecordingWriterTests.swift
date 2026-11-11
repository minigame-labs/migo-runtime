import AVFoundation
import XCTest

@testable import MigoAppleMicrophone

/// The recording's writer, fed what a microphone hands over -- a tone, in the buffers and the format an engine's
/// input gives -- with no microphone: what it writes is the file and the frames content is handed.
final class MigoRecordingWriterTests: XCTestCase {
    private var directory: URL!
    /// An iPhone's microphone: 48 kHz, one channel, floats.
    private let microphone = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 1)!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("migo-recording-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
    }

    private func options(_ asked: [String: Any]) throws -> MigoRecordingOptions {
        try MigoRecordingOptions(asked.merging(["recordingId": 7]) { asked, _ in asked })
    }

    /// `frames` of a 440 Hz tone as `format` carries it.
    private func tone(_ format: AVAudioFormat, frames: Int) -> AVAudioPCMBuffer {
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames))!
        buffer.frameLength = AVAudioFrameCount(frames)
        for channel in 0..<Int(format.channelCount) {
            let samples = buffer.floatChannelData![channel]
            for index in 0..<frames {
                samples[index] = Float(0.5 * sin(2 * Double.pi * 440 * Double(index) / format.sampleRate))
            }
        }
        return buffer
    }

    /// `seconds` of the microphone into `writer`, a tenth of a second at a time, until it is full: the frames it
    /// handed over on the way, and whether it filled.
    private func feed(
        _ writer: MigoRecordingWriter, seconds: Double, from format: AVAudioFormat? = nil
    ) throws -> (frames: [Data], full: Bool) {
        let format = format ?? microphone
        let chunk = Int(format.sampleRate / 10)
        var frames: [Data] = []
        for _ in 0..<Int((seconds * 10).rounded()) {
            let step = try writer.append(tone(format, frames: chunk))
            frames += step.frames
            if step.full { return (frames, true) }
        }
        return (frames, false)
    }

    // MARK: - options

    func testAnOptionLeftOutTakesTheReferencesDefault() throws {
        let options = try MigoRecordingOptions(["recordingId": 3])
        XCTAssertEqual(options.recordingId, 3)
        XCTAssertEqual(options.durationMs, 60_000)
        XCTAssertEqual(options.sampleRate, 8000)
        XCTAssertEqual(options.channels, 2)
        XCTAssertEqual(options.bitRate, 48_000)
        XCTAssertEqual(options.format, .aac)
        XCTAssertEqual(options.frameBytes, 0)
        XCTAssertEqual(options.audioSource, "auto")
        XCTAssertEqual(options.frameLimit, 480_000)
    }

    func testFormatsAreTheReferencesAndMp3RecordsAac() throws {
        XCTAssertEqual(try options(["format": "PCM"]).format, .pcm)
        XCTAssertEqual(try options(["format": "pcm"]).format, .pcm)
        XCTAssertEqual(try options(["format": "wav"]).format, .wav)
        XCTAssertEqual(try options(["format": "mp3"]).format, .aac)
        XCTAssertEqual(try options(["frameSize": 2]).frameBytes, 2048)
    }

    func testWhatTheReferenceDoesNotListIsRefused() {
        let refused: [[String: Any]] = [
            ["sampleRate": 44000], ["numberOfChannels": 3], ["numberOfChannels": 0], ["format": "ogg"],
            ["duration": 0], ["duration": 600_001], ["encodeBitRate": 0],
        ]
        for asked in refused {
            XCTAssertThrowsError(try options(asked), "\(asked)") { error in
                XCTAssertEqual(error as? MigoRecordingError, MigoRecordingError("invalid audio parameters"))
            }
        }
        XCTAssertEqual(
            MigoRecordingError("invalid audio parameters").errMsg, "recorderManager.start:fail invalid audio parameters")
    }

    // MARK: - samples

    func testPcmIsTheSamplesAndEndsOnTheSampleItsDurationEndsOn() throws {
        let writer = try MigoRecordingWriter(
            options: options(["duration": 500, "sampleRate": 16000, "numberOfChannels": 1, "format": "PCM", "frameSize": 1]),
            input: microphone, directory: directory)
        let fed = try feed(writer, seconds: 2)
        XCTAssertTrue(fed.full, "half a second of a two-second feed fills it")
        let ending = try writer.finish()
        // 500 ms at 16 kHz, one channel, 16 bits: 8000 samples, to the sample.
        XCTAssertEqual(ending.durationMs, 500)
        XCTAssertEqual(ending.fileSize, 16000)
        XCTAssertEqual(ending.url.pathExtension, "pcm")
        let file = try Data(contentsOf: ending.url)
        XCTAssertEqual(file.count, 16000)
        let frames = fed.frames + ending.frames
        XCTAssertEqual(frames.reduce(Data(), +), file, "the frames are the file's bytes, in order")
        XCTAssertTrue(frames.dropLast().allSatisfy { $0.count == 1024 }, "each frame is the size asked for")
        XCTAssertEqual(frames.last?.count, 16000 % 1024, "and the last is what is left")
        // A tone, not silence: the samples made it through the resampler.
        let loud = file.withUnsafeBytes { $0.bindMemory(to: Int16.self).contains { abs(Int($0)) > 8000 } }
        XCTAssertTrue(loud)
    }

    func testARecordingStoppedEarlyKeepsWhatItRecorded() throws {
        let writer = try MigoRecordingWriter(
            options: options(["sampleRate": 16000, "numberOfChannels": 2, "format": "PCM"]),
            input: microphone, directory: directory)
        let fed = try feed(writer, seconds: 0.3)
        XCTAssertFalse(fed.full)
        XCTAssertTrue(fed.frames.isEmpty, "no frameSize, no frames")
        let ending = try writer.finish()
        XCTAssertTrue(ending.frames.isEmpty)
        XCTAssertTrue((290...300).contains(ending.durationMs), "\(ending.durationMs) ms of 300")
        // Two channels of 16 bits a frame.
        XCTAssertEqual(ending.fileSize % 4, 0)
        XCTAssertEqual(ending.fileSize / 4 * 1000 / 16000, ending.durationMs)
    }

    func testWavIsTheSamplesBehindAHeaderThatAddsUp() throws {
        let writer = try MigoRecordingWriter(
            options: options(["duration": 800, "sampleRate": 16000, "numberOfChannels": 1, "format": "wav", "frameSize": 1]),
            input: microphone, directory: directory)
        let fed = try feed(writer, seconds: 1)
        let ending = try writer.finish()
        let file = try Data(contentsOf: ending.url)
        XCTAssertEqual(ending.url.pathExtension, "wav")
        XCTAssertEqual(ending.fileSize, file.count)
        XCTAssertEqual(file.count, 44 + 800 * 16 * 2)
        func u32(_ at: Int) -> Int { Int(file[at]) | Int(file[at + 1]) << 8 | Int(file[at + 2]) << 16 | Int(file[at + 3]) << 24 }
        func u16(_ at: Int) -> Int { Int(file[at]) | Int(file[at + 1]) << 8 }
        XCTAssertEqual(String(decoding: file[0..<4], as: UTF8.self), "RIFF")
        XCTAssertEqual(u32(4), file.count - 8)
        XCTAssertEqual(String(decoding: file[8..<16], as: UTF8.self), "WAVEfmt ")
        XCTAssertEqual(u16(20), 1, "PCM")
        XCTAssertEqual(u16(22), 1, "one channel")
        XCTAssertEqual(u32(24), 16000)
        XCTAssertEqual(u32(28), 32000, "bytes a second")
        XCTAssertEqual(u16(34), 16, "bits a sample")
        XCTAssertEqual(String(decoding: file[36..<40], as: UTF8.self), "data")
        XCTAssertEqual(u32(40), file.count - 44)
        XCTAssertEqual((fed.frames + ending.frames).reduce(Data(), +), file.subdata(in: 44..<file.count), "frames are the samples")
    }

    func testTheMicrophoneCanChangeUnderARecording() throws {
        let writer = try MigoRecordingWriter(
            options: options(["sampleRate": 16000, "numberOfChannels": 1, "format": "PCM"]),
            input: microphone, directory: directory)
        _ = try feed(writer, seconds: 0.2)
        // A headset: another rate, two channels.
        let headset = AVAudioFormat(standardFormatWithSampleRate: 44100, channels: 2)!
        try writer.retarget(input: headset)
        _ = try feed(writer, seconds: 0.2, from: headset)
        let ending = try writer.finish()
        // Nothing of the first microphone's sound is lost at the change.
        XCTAssertTrue((395...400).contains(ending.durationMs), "\(ending.durationMs) ms of 400")
    }

    func testAMicrophoneThatHandsOverNothingIsRefused() {
        let none = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 0, channels: 0, interleaved: false)
        guard let none else { return }
        XCTAssertThrowsError(try MigoRecordingWriter(options: options([:]), input: none, directory: directory))
    }

    // MARK: - AAC

    func testAnAdtsHeaderSaysItsPacketsRateChannelsAndLength() {
        // 16 kHz is index 8; 100 bytes of payload and the header's own 7.
        XCTAssertEqual(
            Array(MigoAdtsSink.header(payload: 100, rateIndex: 8, channels: 1)), [0xFF, 0xF1, 0x60, 0x40, 0x0D, 0x7F, 0xFC])
    }

    func testAacWithFramesIsAnAdtsStreamOfWholePackets() throws {
        let writer = try MigoRecordingWriter(
            options: options(["duration": 1000, "sampleRate": 16000, "numberOfChannels": 1, "format": "aac", "frameSize": 1]),
            input: microphone, directory: directory)
        let fed = try feed(writer, seconds: 2)
        XCTAssertTrue(fed.full)
        let ending = try writer.finish()
        XCTAssertEqual(ending.url.pathExtension, "aac")
        XCTAssertEqual(ending.durationMs, 1000)
        let file = try Data(contentsOf: ending.url)
        XCTAssertEqual(ending.fileSize, file.count)
        XCTAssertEqual((fed.frames + ending.frames).reduce(Data(), +), file, "the frames are the stream")
        // Header to header, the stream ends where the file does.
        var at = 0
        var packets = 0
        while at + 7 <= file.count {
            XCTAssertEqual(file[at], 0xFF)
            XCTAssertEqual(file[at + 1], 0xF1)
            XCTAssertEqual((file[at + 2] >> 2) & 0xF, 8, "16 kHz")
            XCTAssertEqual((file[at + 2] & 1) << 2 | file[at + 3] >> 6, 1, "one channel")
            let length = Int(file[at + 3] & 3) << 11 | Int(file[at + 4]) << 3 | Int(file[at + 5]) >> 5
            XCTAssertGreaterThan(length, 7)
            at += length
            packets += 1
        }
        XCTAssertEqual(at, file.count)
        // A second of 1024-sample packets, and the encoder's own lead-in.
        XCTAssertTrue((15...20).contains(packets), "\(packets) packets")
    }

    func testAacWithoutFramesIsAnMpeg4FileTheSystemReads() throws {
        // The reference's defaults: 8 kHz, two channels, 48 kbps -- more than the encoder takes at that rate.
        let writer = try MigoRecordingWriter(options: options(["duration": 1000]), input: microphone, directory: directory)
        let fed = try feed(writer, seconds: 2)
        XCTAssertTrue(fed.full)
        XCTAssertTrue(fed.frames.isEmpty)
        let ending = try writer.finish()
        XCTAssertTrue(ending.frames.isEmpty)
        XCTAssertEqual(ending.url.pathExtension, "m4a")
        XCTAssertEqual(ending.durationMs, 1000)
        XCTAssertGreaterThan(ending.fileSize, 0)
        let read = try AVAudioFile(forReading: ending.url)
        XCTAssertEqual(read.fileFormat.sampleRate, 8000)
        XCTAssertEqual(read.fileFormat.channelCount, 2)
        XCTAssertTrue((7000...10000).contains(read.length), "\(read.length) frames of 8000")
    }

    func testEveryRateTheReferenceListsRecordsAac() throws {
        for rate in MigoRecordingOptions.sampleRates.sorted() {
            for channels in 1...2 {
                for frameSize in [0, 1] {
                    let asked: [String: Any] = [
                        "duration": 300, "sampleRate": rate, "numberOfChannels": channels, "frameSize": frameSize,
                    ]
                    let writer = try MigoRecordingWriter(options: options(asked), input: microphone, directory: directory)
                    XCTAssertTrue(try feed(writer, seconds: 1).full, "\(asked)")
                    let ending = try writer.finish()
                    XCTAssertEqual(ending.durationMs, 300, "\(asked)")
                    XCTAssertGreaterThan(ending.fileSize, 0, "\(asked)")
                }
            }
        }
    }
}
