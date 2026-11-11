import AVFoundation
import XCTest

@testable import MigoAppleMicrophone

/// A microphone that hears a tone: 100 ms of it every few milliseconds, so a recording's duration -- which the
/// recorder counts in samples, not on a clock -- passes twenty times as fast as it would in a room.
private final class ToneInput: MigoMicrophoneInput {
    private let queue = DispatchQueue(label: "com.migo.tests.tone")
    private var timer: DispatchSourceTimer?
    private let lock = NSLock()
    private var current: AVAudioFormat
    private var started = 0
    var changed: (() -> Void)?
    /// A microphone the system will not open.
    var refuses = false

    init(_ format: AVAudioFormat = AVAudioFormat(standardFormatWithSampleRate: 48000, channels: 1)!) {
        current = format
    }

    var format: AVAudioFormat {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    /// How many times it was started.
    var starts: Int {
        lock.lock()
        defer { lock.unlock() }
        return started
    }

    /// Another device takes its place, as a headset plugged in does: it stops, and says it changed.
    func change(to format: AVAudioFormat) {
        stop()
        lock.lock()
        current = format
        lock.unlock()
        changed?()
    }

    func start(_ hear: @escaping (AVAudioPCMBuffer) -> Void) throws {
        if refuses { throw MigoRecordingError("there is no microphone to record from") }
        stop()
        let format = self.format
        lock.lock()
        started += 1
        lock.unlock()
        let frames = AVAudioFrameCount(format.sampleRate / 10)
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(5))
        timer.setEventHandler {
            let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
            buffer.frameLength = frames
            for channel in 0..<Int(format.channelCount) {
                let samples = buffer.floatChannelData![channel]
                for index in 0..<Int(frames) {
                    samples[index] = Float(0.5 * sin(2 * Double.pi * 440 * Double(index) / format.sampleRate))
                }
            }
            hear(buffer)
        }
        queue.sync { self.timer = timer }
        timer.resume()
    }

    func pause() {
        stop()
    }

    func stop() {
        // On the queue its buffers are handed over on: none is, once this returns.
        queue.sync {
            timer?.cancel()
            timer = nil
        }
    }
}

/// The recorder: what it tells, in what order, and the file it leaves. Its microphone here is a tone, so every
/// test runs wherever the tests do; one more records from the machine's own microphone when asked to
/// (`MIGO_TEST_MICROPHONE=1`, with the microphone granted to the process that runs the tests).
final class MigoRecorderTests: XCTestCase {
    private var events: [MigoRecorder.Event] = []
    private var waiters: [(MigoRecorder.Event) -> Bool] = []
    private var files: [URL] = []

    override func tearDown() {
        files.forEach { try? FileManager.default.removeItem(at: $0) }
    }

    private func recorder(granted: Bool = true, from input: ToneInput) -> MigoRecorder {
        MigoRecorder(grant: { $0(granted) }, input: { _ in input }, events: { [weak self] in self?.heard($0) })
    }

    private func heard(_ event: MigoRecorder.Event) {
        events.append(event)
        if case .stopped(_, let file, _, _) = event { files.append(file) }
        waiters.removeAll { $0(event) }
    }

    /// Waits for an event `matches` accepts.
    private func hear(_ what: String, _ matches: @escaping (MigoRecorder.Event) -> Bool) {
        let heard = expectation(description: what)
        waiters.append { event in
            guard matches(event) else { return false }
            heard.fulfill()
            return true
        }
        wait(for: [heard], timeout: 20)
    }

    private func pass(_ seconds: TimeInterval) {
        let passed = expectation(description: "time passes")
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { passed.fulfill() }
        wait(for: [passed], timeout: seconds + 5)
    }

    private var frames: [(data: Data, isLast: Bool)] {
        events.compactMap { event in
            if case .frame(_, let data, let isLast) = event { return (data, isLast) }
            return nil
        }
    }

    private func stop(of id: UInt32) -> (file: URL, durationMs: Int, fileSize: Int)? {
        for event in events {
            if case .stopped(id, let file, let durationMs, let fileSize) = event { return (file, durationMs, fileSize) }
        }
        return nil
    }

    private func names(_ events: [MigoRecorder.Event]) -> [String] {
        events.map { event in
            switch event {
            case .started: return "started"
            case .paused: return "paused"
            case .resumed: return "resumed"
            case .stopped: return "stopped"
            case .failed(_, let errMsg): return "failed: \(errMsg)"
            case .interruptionBegan: return "interruptionBegan"
            case .interruptionEnded: return "interruptionEnded"
            case .frame(_, _, let isLast): return isLast ? "lastFrame" : "frame"
            case .grantAsked: return "grantAsked"
            }
        }
    }

    // MARK: - starting

    func testWhatTheReferenceDoesNotListFailsInItsWordsWithNoMicrophoneAsked() {
        let input = ToneInput()
        let recorder = recorder(from: input)
        recorder.handle(.start, ["recordingId": 4, "sampleRate": 12345])
        hear("failed") { if case .failed = $0 { return true } else { return false } }
        XCTAssertEqual(names(events), ["failed: recorderManager.start:fail invalid audio parameters"])
        XCTAssertEqual(input.starts, 0)
    }

    func testARefusedMicrophoneFailsTheRecordingAndTheGrantIsReadAgain() {
        let input = ToneInput()
        let recorder = recorder(granted: false, from: input)
        recorder.handle(.start, ["recordingId": 4])
        hear("failed") { if case .failed = $0 { return true } else { return false } }
        XCTAssertEqual(
            names(events), ["grantAsked", "failed: recorderManager.start:fail auth deny, the app may not use the microphone"])
        XCTAssertEqual(input.starts, 0)
    }

    func testAMicrophoneThatDoesNotOpenFailsTheRecording() {
        let input = ToneInput()
        input.refuses = true
        let recorder = recorder(from: input)
        recorder.handle(.start, ["recordingId": 4, "format": "PCM"])
        hear("failed") { if case .failed = $0 { return true } else { return false } }
        XCTAssertEqual(names(events), ["grantAsked", "failed: recorderManager.start:fail there is no microphone to record from"])
        // And a recording after it starts as any does.
        input.refuses = false
        recorder.handle(.start, ["recordingId": 5, "duration": 200, "format": "PCM"])
        hear("stopped") { if case .stopped(5, _, _, _) = $0 { return true } else { return false } }
    }

    // MARK: - a recording

    func testARecordingStartsHandsOverFramesAndStopsWhenItsDurationRunsOut() throws {
        let recorder = recorder(from: ToneInput())
        recorder.handle(
            .start,
            ["recordingId": 5, "duration": 600, "sampleRate": 16000, "numberOfChannels": 1, "format": "PCM", "frameSize": 1])
        hear("stopped") { if case .stopped(5, _, _, _) = $0 { return true } else { return false } }
        let stopped = try XCTUnwrap(stop(of: 5))
        // 600 ms at 16 kHz, one channel, 16 bits, to the sample.
        XCTAssertEqual(stopped.durationMs, 600)
        XCTAssertEqual(stopped.fileSize, 19200)
        let file = try Data(contentsOf: stopped.file)
        XCTAssertEqual(file.count, 19200)
        XCTAssertEqual(frames.map(\.data).reduce(Data(), +), file, "the frames are the file's bytes, in order")
        XCTAssertTrue(frames.dropLast().allSatisfy { $0.data.count == 1024 })
        // Told in order: the grant read, started, the frames, the last one, the stop -- and nothing after.
        let told = names(events)
        XCTAssertEqual(Array(told.prefix(2)), ["grantAsked", "started"])
        XCTAssertEqual(Array(told.suffix(2)), ["lastFrame", "stopped"])
        XCTAssertEqual(Set(told.dropFirst(2).dropLast(2)), ["frame"])
        pass(0.1)
        XCTAssertEqual(names(events), told, "a finished recording tells nothing more")
    }

    func testARecordingPausesResumesAndStopsWhenAskedWithItsFile() throws {
        let recorder = recorder(from: ToneInput())
        recorder.handle(
            .start, ["recordingId": 6, "sampleRate": 16000, "numberOfChannels": 2, "format": "wav", "frameSize": 1])
        hear("started") { if case .started(6) = $0 { return true } else { return false } }
        // Requests for another recording, or in the wrong state, are ignored.
        recorder.handle(.resume, ["recordingId": 6])
        recorder.handle(.pause, ["recordingId": 60])
        recorder.handle(.stop, ["recordingId": 60])
        pass(0.05)
        recorder.handle(.pause, ["recordingId": 6])
        hear("paused") { if case .paused(6) = $0 { return true } else { return false } }
        recorder.handle(.pause, ["recordingId": 6])
        pass(0.05)
        let whilePaused = frames.count
        pass(0.1)
        XCTAssertEqual(frames.count, whilePaused, "a paused recording records nothing")
        recorder.handle(.resume, ["recordingId": 6])
        hear("resumed") { if case .resumed(6) = $0 { return true } else { return false } }
        pass(0.05)
        XCTAssertGreaterThan(frames.count, whilePaused, "and records again once resumed")
        recorder.handle(.stop, ["recordingId": 6])
        hear("stopped") { if case .stopped(6, _, _, _) = $0 { return true } else { return false } }
        let stopped = try XCTUnwrap(stop(of: 6))
        XCTAssertEqual(stopped.file.pathExtension, "wav")
        let size = try XCTUnwrap(FileManager.default.attributesOfItem(atPath: stopped.file.path)[.size] as? NSNumber)
        XCTAssertEqual(size.intValue, stopped.fileSize)
        // A header and two channels of 16 bits for as long as it says.
        XCTAssertGreaterThan(stopped.durationMs, 0)
        XCTAssertEqual(Double((stopped.fileSize - 44) / 4) * 1000 / 16000, Double(stopped.durationMs), accuracy: 1)
        let told = names(events)
        XCTAssertEqual(told.filter { $0 == "paused" }.count, 1)
        XCTAssertEqual(told.filter { $0 == "resumed" }.count, 1)
        XCTAssertEqual(Array(told.suffix(2)), ["lastFrame", "stopped"])
        XCTAssertFalse(told.contains { $0.hasPrefix("failed") }, "\(told)")
    }

    func testANewStartTakesTheRecordingsPlaceWithNoStop() {
        let recorder = recorder(from: ToneInput())
        recorder.handle(.start, ["recordingId": 1, "sampleRate": 16000, "format": "PCM", "frameSize": 1])
        hear("the first started") { if case .started(1) = $0 { return true } else { return false } }
        recorder.handle(.start, ["recordingId": 2, "duration": 300, "sampleRate": 16000, "format": "PCM"])
        hear("the second stopped") { if case .stopped(2, _, _, _) = $0 { return true } else { return false } }
        XCTAssertNil(stop(of: 1), "the replaced recording reports no stop")
        for event in events {
            if case .frame(1, _, true) = event { XCTFail("nor a last frame") }
        }
    }

    func testAClosedSessionsRecordingGoesWithNothingSaid() {
        let recorder = recorder(from: ToneInput())
        recorder.handle(.start, ["recordingId": 9, "sampleRate": 16000, "format": "PCM", "frameSize": 1])
        hear("started") { if case .started(9) = $0 { return true } else { return false } }
        pass(0.05)
        recorder.close()
        // A stop that arrives late finds no recording.
        recorder.handle(.stop, ["recordingId": 9])
        pass(0.2)
        let told = names(events)
        XCTAssertFalse(told.contains("stopped"), "\(told)")
        XCTAssertFalse(told.contains("lastFrame"), "\(told)")
        XCTAssertFalse(told.contains { $0.hasPrefix("failed") }, "\(told)")
    }

    // MARK: - what happens to the microphone

    func testAMicrophoneThatChangesUnderARecordingIsListenedToAgain() throws {
        let input = ToneInput()
        let recorder = recorder(from: input)
        recorder.handle(
            .start, ["recordingId": 3, "duration": 3000, "sampleRate": 16000, "numberOfChannels": 1, "format": "PCM"])
        hear("started") { if case .started(3) = $0 { return true } else { return false } }
        // A headset: another rate, two channels.
        input.change(to: AVAudioFormat(standardFormatWithSampleRate: 44100, channels: 2)!)
        hear("stopped") { if case .stopped(3, _, _, _) = $0 { return true } else { return false } }
        let stopped = try XCTUnwrap(stop(of: 3))
        XCTAssertEqual(input.starts, 2)
        XCTAssertEqual(stopped.durationMs, 3000, "the recording went on to its whole duration")
        XCTAssertEqual(stopped.fileSize, 96000)
    }

    func testAnInterruptionPausesTheRecordingAndItIsContentsToResume() {
        let recorder = recorder(from: ToneInput())
        recorder.handle(.start, ["recordingId": 8, "sampleRate": 16000, "format": "PCM"])
        hear("started") { if case .started(8) = $0 { return true } else { return false } }
        recorder.interrupted(began: true)
        hear("paused") { if case .paused(8) = $0 { return true } else { return false } }
        recorder.interrupted(began: false)
        hear("the interruption ended") { if case .interruptionEnded(8) = $0 { return true } else { return false } }
        recorder.handle(.resume, ["recordingId": 8])
        hear("resumed") { if case .resumed(8) = $0 { return true } else { return false } }
        XCTAssertEqual(
            names(events), ["grantAsked", "started", "interruptionBegan", "paused", "interruptionEnded", "resumed"])
        recorder.close()
    }

    // MARK: - the machine's own microphone

    func testTheMachinesOwnMicrophoneRecords() throws {
        try XCTSkipUnless(
            ProcessInfo.processInfo.environment["MIGO_TEST_MICROPHONE"] == "1",
            "set MIGO_TEST_MICROPHONE=1 to record from this machine's microphone")
        let recorder = MigoRecorder(events: { [weak self] in self?.heard($0) })
        recorder.handle(
            .start,
            ["recordingId": 5, "duration": 600, "sampleRate": 16000, "numberOfChannels": 1, "format": "PCM", "frameSize": 1])
        hear("stopped or failed") {
            switch $0 {
            case .stopped, .failed: return true
            default: return false
            }
        }
        let stopped = try XCTUnwrap(stop(of: 5), "\(names(events))")
        XCTAssertEqual(stopped.durationMs, 600)
        XCTAssertEqual(stopped.fileSize, 19200)
        XCTAssertEqual(frames.map(\.data).reduce(Data(), +), try Data(contentsOf: stopped.file))
    }
}
