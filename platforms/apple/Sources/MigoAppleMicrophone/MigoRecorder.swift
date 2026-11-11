import AVFoundation
import Foundation

/// The app's grant for the microphone, as the system holds it.
enum MigoMicrophoneGrant {
    static var status: AVAuthorizationStatus { AVCaptureDevice.authorizationStatus(for: .audio) }

    /// Whether the app says why it records: without it the system shows no prompt.
    static var declaresPurpose: Bool {
        (Bundle.main.object(forInfoDictionaryKey: "NSMicrophoneUsageDescription") as? String)?.isEmpty == false
    }

    /// Whether the app may record, asking the player when the system has not decided. `completion` is called on
    /// whatever thread the answer arrives on -- this one, when it is already known.
    static func ask(_ completion: @escaping (Bool) -> Void) {
        switch status {
        case .authorized:
            completion(true)
        case .notDetermined where declaresPurpose:
            AVCaptureDevice.requestAccess(for: .audio, completionHandler: completion)
        case .notDetermined:
            NSLog(
                "MigoMicrophone: the app declares no NSMicrophoneUsageDescription in its Info.plist, so the system "
                    + "shows no prompt and the microphone is refused")
            completion(false)
        default:
            // Decided already: the player changes it in Settings, not here.
            completion(false)
        }
    }
}

/// Where a recording's sound comes from: the system's input, and in tests a tone.
protocol MigoMicrophoneInput: AnyObject {
    /// The format its buffers arrive in now.
    var format: AVAudioFormat { get }
    /// Called, on any thread, when the input changed under a recording -- another device, another format -- and
    /// stopped: it is to be started again.
    var changed: (() -> Void)? { get set }
    /// Buffers to `hear` from now, on the input's own thread; each is the input's again once `hear` returns.
    func start(_ hear: @escaping (AVAudioPCMBuffer) -> Void) throws
    /// No buffers until it is started again.
    func pause()
    /// No buffers any more: none arrives once this returns.
    func stop()
}

/// The system's input, through an audio engine's input node.
final class MigoSystemInput: MigoMicrophoneInput {
    private let engine = AVAudioEngine()
    private var observer: NSObjectProtocol?
    var changed: (() -> Void)?

    init() {
        // A headset plugged in, another device chosen: the engine stops and says so.
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
        ) { [weak self] _ in self?.changed?() }
    }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }

    var format: AVAudioFormat { engine.inputNode.outputFormat(forBus: 0) }

    func start(_ hear: @escaping (AVAudioPCMBuffer) -> Void) throws {
        stop()
        let input = engine.inputNode
        input.installTap(onBus: 0, bufferSize: 4096, format: input.outputFormat(forBus: 0)) { buffer, _ in hear(buffer) }
        engine.prepare()
        #if os(iOS)
            try AVAudioSession.sharedInstance().setActive(true)
        #endif
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
    }

    func pause() {
        engine.pause()
    }

    func stop() {
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
    }
}

/// The recorder: one recording at a time from the system's input, as the recorder service asks for it and
/// reports it.
///
/// It works on a queue of its own. Opening the microphone waits on the system -- the audio hardware, and on a
/// Mac sometimes the player's answer to a prompt -- and finishing a recording writes its file; neither is done on
/// the game's main thread. Requests are taken from any thread and carried out in the order they came; what
/// happens is told on the main thread, in order: a recording's frames before its last, its last before its STOP.
final class MigoRecorder {
    enum Request {
        case start, pause, resume, stop
    }

    enum Event {
        case started(UInt32)
        case paused(UInt32)
        case resumed(UInt32)
        case stopped(UInt32, file: URL, durationMs: Int, fileSize: Int)
        case failed(UInt32, errMsg: String)
        case interruptionBegan(UInt32)
        case interruptionEnded(UInt32)
        case frame(UInt32, Data, isLast: Bool)
        /// The player may just have answered the system's prompt: the grant is to be read again.
        case grantAsked
    }

    /// One recording under way. The recorder's queue's, but for `open`.
    private final class Capture {
        let id: UInt32
        let input: MigoMicrophoneInput
        let writer: MigoRecordingWriter
        /// What the writer is used on: the microphone's buffers arrive on the input's own thread, and are
        /// written before it takes them back.
        let work = DispatchQueue(label: "com.migo.microphone.writer")
        /// Whether buffers are still written. The writer's queue's.
        var open = true
        var paused = false
        /// Paused by the system, which took the microphone.
        var interrupted = false
        /// Let go of with nothing to report: replaced, or the session closed.
        var cancelled = false

        init(id: UInt32, input: MigoMicrophoneInput, writer: MigoRecordingWriter) {
            self.id = id
            self.input = input
            self.writer = writer
        }
    }

    private let queue = DispatchQueue(label: "com.migo.microphone")
    private let tell: (Event) -> Void
    /// Whether the app may record, asked of the system.
    private let ask: (@escaping (Bool) -> Void) -> Void
    /// The microphone a recording of these options records from, opened.
    private let open: (MigoRecordingOptions) throws -> MigoMicrophoneInput
    private var capture: Capture?
    /// A START waiting on the system's prompt; a later request for a recording takes its place.
    private var waiting: MigoRecordingOptions?
    private var observers: [NSObjectProtocol] = []

    /// `events` hears what happens, on the main thread. The system's grant and the system's input, unless a test
    /// gives its own.
    init(
        grant: @escaping (@escaping (Bool) -> Void) -> Void = MigoMicrophoneGrant.ask,
        input: @escaping (MigoRecordingOptions) throws -> MigoMicrophoneInput = MigoRecorder.systemInput,
        events: @escaping (Event) -> Void
    ) {
        ask = grant
        open = input
        tell = events
        #if os(iOS)
            observers.append(
                NotificationCenter.default.addObserver(
                    forName: AVAudioSession.interruptionNotification, object: nil, queue: nil
                ) {
                    [weak self] note in
                    let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
                    guard let kind = raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) else { return }
                    self?.interrupted(began: kind == .began)
                })
        #endif
    }

    deinit {
        observers.forEach { NotificationCenter.default.removeObserver($0) }
        // Nothing is queued that still refers to this recorder: its recording goes with it.
        if let capture { _ = letGo(of: capture) { $0.discard() } }
    }

    /// One of the recorder service's requests, with the options the service documents for it.
    func handle(_ request: Request, _ object: [String: Any]) {
        queue.async { self.perform(request, object) }
    }

    /// The session closed: the recording under way, or waiting to start, is nobody's.
    func close() {
        queue.async {
            self.waiting = nil
            if let capture = self.capture { self.cancel(capture) }
        }
    }

    /// The system took the microphone -- a call -- or gave it back. Taken, the recording is paused, and content
    /// hears the interruption and then the pause; given back, it is content's to resume.
    func interrupted(began: Bool) {
        queue.async {
            guard let capture = self.capture else { return }
            if began {
                guard !capture.paused else { return }
                capture.input.pause()
                capture.paused = true
                capture.interrupted = true
                self.send(.interruptionBegan(capture.id))
                self.send(.paused(capture.id))
            } else if capture.interrupted {
                capture.interrupted = false
                self.send(.interruptionEnded(capture.id))
            }
        }
    }

    // MARK: - requests

    private func perform(_ request: Request, _ object: [String: Any]) {
        let id = MigoRecordingOptions.recordingId(of: object)
        switch request {
        case .start:
            start(id, object)
        case .pause:
            guard let capture, capture.id == id, !capture.paused else { return }
            capture.input.pause()
            capture.paused = true
            send(.paused(id))
        case .resume:
            guard let capture, capture.id == id, capture.paused else { return }
            do {
                try listen(to: capture)
                capture.paused = false
                capture.interrupted = false
                send(.resumed(id))
            } catch {
                fail(capture, error)
            }
        case .stop:
            if waiting?.recordingId == id { waiting = nil }
            if let capture, capture.id == id { finish(capture) }
        }
    }

    private func start(_ id: UInt32, _ object: [String: Any]) {
        // A recording under way stops first, with no STOP: the new one takes its place.
        waiting = nil
        if let capture { cancel(capture) }
        let options: MigoRecordingOptions
        do {
            options = try MigoRecordingOptions(object)
        } catch {
            send(.failed(id, errMsg: Self.errMsg(error)))
            return
        }
        // Content's scope.record was the engine's to check; the system's own grant may still be undecided when
        // the app decides content's scopes itself.
        waiting = options
        ask { [weak self] granted in
            self?.queue.async { self?.asked(options, granted: granted) }
        }
    }

    private func asked(_ options: MigoRecordingOptions, granted: Bool) {
        send(.grantAsked)
        guard waiting == options else { return }
        waiting = nil
        guard granted else {
            send(.failed(options.recordingId, errMsg: MigoRecordingError("auth deny, the app may not use the microphone").errMsg))
            return
        }
        do {
            let input = try open(options)
            let writer = try MigoRecordingWriter(
                options: options, input: input.format, directory: FileManager.default.temporaryDirectory)
            let capture = Capture(id: options.recordingId, input: input, writer: writer)
            input.changed = { [weak self, weak capture] in
                self?.queue.async {
                    guard let self, let capture, self.capture === capture, !capture.paused else { return }
                    self.relisten(to: capture)
                }
            }
            do {
                try listen(to: capture)
            } catch {
                writer.discard()
                throw error
            }
            self.capture = capture
            send(.started(options.recordingId))
        } catch {
            send(.failed(options.recordingId, errMsg: Self.errMsg(error)))
        }
    }

    /// The system's input for a recording of `options`, the process's audio session set up for it first: made
    /// once the session records, its input is the microphone the recording uses.
    static func systemInput(for options: MigoRecordingOptions) throws -> MigoMicrophoneInput {
        try takeTheSession(for: options)
        return MigoSystemInput()
    }

    /// The process's audio session, set up to record. On iOS it is the engine's, which lent it when it passed
    /// START on and puts its own category back when the recording is reported over: nothing is restored here.
    private static func takeTheSession(for options: MigoRecordingOptions) throws {
        #if os(iOS)
            let session = AVAudioSession.sharedInstance()
            // The game's sound stays where it plays: the speaker, not the receiver this category defaults to, or
            // the headphones it was on.
            try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetoothA2DP])
            try session.setActive(true)
            let ports: [String: AVAudioSession.Port] = ["buildInMic": .builtInMic, "headsetMic": .headsetMic]
            guard let port = ports[options.audioSource] else {
                try session.setPreferredInput(nil)
                return
            }
            guard let input = session.availableInputs?.first(where: { $0.portType == port }) else {
                throw MigoRecordingError("the audio source \(options.audioSource) is not available")
            }
            try session.setPreferredInput(input)
        #endif
    }

    /// The microphone's buffers to the recording's writer, from now: as the recording starts, as it resumes, and
    /// again when the microphone changes under it -- each time in the format the microphone hands over then.
    private func listen(to capture: Capture) throws {
        capture.input.stop()
        let format = capture.input.format
        try capture.work.sync { try capture.writer.retarget(input: format) }
        try capture.input.start { [weak self, weak capture] buffer in
            guard let capture else { return }
            // Written before this returns: the buffer is the input's again afterwards.
            let step: Result<MigoRecordingWriter.Step, Error>? = capture.work.sync {
                guard capture.open else { return nil }
                let step = Result { try capture.writer.append(buffer) }
                if case .success(let taken) = step, taken.frames.isEmpty, !taken.full { return nil }
                return step
            }
            guard let step else { return }
            self?.queue.async { self?.took(step, of: capture) }
        }
    }

    /// The microphone changed under the recording and stopped: listened to again, as it is now.
    private func relisten(to capture: Capture) {
        do {
            try listen(to: capture)
        } catch {
            fail(capture, error)
        }
    }

    /// What one buffer came to, in the order the buffers came.
    private func took(_ step: Result<MigoRecordingWriter.Step, Error>, of capture: Capture) {
        guard !capture.cancelled else { return }
        switch step {
        case .failure(let error):
            if self.capture === capture { fail(capture, error) }
        case .success(let step):
            for frame in step.frames { send(.frame(capture.id, frame, isLast: false)) }
            if step.full, self.capture === capture { finish(capture) }
        }
    }

    /// The recording ends with its file: asked to, or its duration ran out.
    private func finish(_ capture: Capture) {
        let ending = letGo(of: capture) { try $0.finish() }
        // Behind the buffers already on their way here, so the last frame is the last one told.
        queue.async {
            guard !capture.cancelled else { return }
            switch ending {
            case .failure(let error):
                self.send(.failed(capture.id, errMsg: Self.errMsg(error)))
            case .success(let ending):
                for (index, frame) in ending.frames.enumerated() {
                    self.send(.frame(capture.id, frame, isLast: index == ending.frames.count - 1))
                }
                self.send(
                    .stopped(capture.id, file: ending.url, durationMs: ending.durationMs, fileSize: ending.fileSize))
            }
        }
    }

    /// The recording ends with an error, and no file.
    private func fail(_ capture: Capture, _ error: Error) {
        _ = letGo(of: capture) { $0.discard() }
        send(.failed(capture.id, errMsg: Self.errMsg(error)))
    }

    /// The recording goes with nothing said of it.
    private func cancel(_ capture: Capture) {
        capture.cancelled = true
        _ = letGo(of: capture) { $0.discard() }
    }

    /// The microphone let go of and the writer closed with `end`, which no buffer follows.
    private func letGo<T>(of capture: Capture, with end: (MigoRecordingWriter) throws -> T) -> Result<T, Error> {
        if self.capture === capture { self.capture = nil }
        capture.input.changed = nil
        capture.input.stop()
        return capture.work.sync {
            capture.open = false
            return Result { try end(capture.writer) }
        }
    }

    // MARK: - telling

    private func send(_ event: Event) {
        DispatchQueue.main.async { [tell] in tell(event) }
    }

    private static func errMsg(_ error: Error) -> String {
        (error as? MigoRecordingError)?.errMsg
            ?? MigoRecordingError("the microphone could not record: \(error.localizedDescription)").errMsg
    }
}
