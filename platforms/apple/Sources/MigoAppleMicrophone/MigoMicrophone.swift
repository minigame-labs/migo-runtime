import AVFoundation
import Foundation
import MigoAppleRenderer
import MigoEngine

/// The microphone for games, recorded with AVFoundation: `getRecorderManager`'s recordings -- one at a time, into
/// the file and the frames content asked for -- content's `scope.record`, and the microphone's grant for
/// `getAppAuthorizeSetting`.
///
/// An app opts in by linking this product, passing a `MigoMicrophone()` among its game view's capabilities, and
/// declaring why it uses the microphone: `NSMicrophoneUsageDescription` (a sandboxed or hardened Mac app also
/// needs the audio-input entitlement). Without the purpose string the system shows no prompt and `scope.record`
/// is refused. It is a product of its own because App Store review requires that purpose string of any app whose
/// binary uses the microphone.
///
/// On iOS a process has one audio session, and it is the engine's: the engine lends it for each recording, from
/// the START it passes on until the STOP or ERROR reported here, and puts its own category back afterwards. So
/// this sets the category it records with -- play and record, the game's sound staying on the speaker or the
/// headphones it was on -- and never restores one.
///
/// `audioSource` picks the built-in or the wired headset's microphone on iOS (`auto`, and every source on a Mac,
/// is the system's choice). `audioMode` asks for processing the system decides by itself here, and is not acted on.
///
/// Create it on the main thread; it is used there.
public final class MigoMicrophone: MigoSystemCapability {
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count these APIs as provided on the Apple
    /// platforms: one service per line, platform-specific ones inside `#if os(...)`.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        builtIn.insert(.recorder)
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    public var scopes: Set<MigoScope> { [MigoScope(MIGO_SCOPE_RECORD)] }

    /// One recording under way.
    private final class Capture {
        let id: UInt32
        let engine: AVAudioEngine
        let writer: MigoRecordingWriter
        /// What the writer is used on: the microphone's buffers arrive off the main thread.
        let work = DispatchQueue(label: "com.migo.microphone.recording")
        /// Whether buffers are still written. The work queue's.
        var open = true
        var paused = false
        /// Paused by the system, which took the microphone.
        var interrupted = false
        /// Let go of with nothing to report: replaced, or the session closed.
        var cancelled = false

        init(id: UInt32, engine: AVAudioEngine, writer: MigoRecordingWriter) {
            self.id = id
            self.engine = engine
            self.writer = writer
        }
    }

    private var reports: MigoCapabilityReports?
    private var capture: Capture?
    /// A START waiting on the system's prompt; a later request for a recording takes its place.
    private var waiting: MigoRecordingOptions?
    private var observers: [NSObjectProtocol] = []

    public init() {
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: .AVAudioEngineConfigurationChange, object: nil, queue: .main) {
                [weak self] note in
                self?.inputChanged(note.object as AnyObject?)
            })
        #if os(iOS)
            observers.append(
                center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) {
                    [weak self] note in
                    let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
                    guard let kind = raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) else { return }
                    self?.interrupted(began: kind == .began)
                })
        #endif
    }

    deinit {
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    // MARK: - what the system says

    public func report(to reports: MigoCapabilityReports?) {
        self.reports = reports
        guard reports != nil else {
            // The session closed: the recording under way is nobody's.
            waiting = nil
            if let capture { cancel(capture) }
            return
        }
        reportState()
    }

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        guard scope == MigoScope(MIGO_SCOPE_RECORD) else {
            completion(false)
            return
        }
        askForTheMicrophone(completion)
    }

    /// Whether the app may record, asking the player when the system has not decided.
    private func askForTheMicrophone(_ completion: @escaping (Bool) -> Void) {
        let status = AVCaptureDevice.authorizationStatus(for: .audio)
        guard status == .notDetermined else {
            // Decided already: the player changes it in Settings, not here.
            reportState()
            completion(status == .authorized)
            return
        }
        guard Self.declaresPurpose else {
            NSLog(
                "MigoMicrophone: the app declares no NSMicrophoneUsageDescription in its Info.plist, so the system "
                    + "shows no prompt and scope.record is refused")
            completion(false)
            return
        }
        AVCaptureDevice.requestAccess(for: .audio) { granted in
            DispatchQueue.main.async { [weak self] in
                self?.reportState()
                completion(granted)
            }
        }
    }

    static var declaresPurpose: Bool {
        (Bundle.main.object(forInfoDictionaryKey: "NSMicrophoneUsageDescription") as? String)?.isEmpty == false
    }

    private func reportState() {
        guard let reports else { return }
        let status = AVCaptureDevice.authorizationStatus(for: .audio)
        reports.updateAppAuthorization { $0.microphone = Self.authorization(status) }
        reports.setScopeState(MigoScope(MIGO_SCOPE_RECORD), Self.scopeState(status))
    }

    static func authorization(_ status: AVAuthorizationStatus) -> UInt8 {
        switch status {
        case .authorized: return UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
        case .denied, .restricted: return UInt8(MIGO_AUTHORIZATION_DENIED)
        default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
        }
    }

    static func scopeState(_ status: AVAuthorizationStatus) -> MigoScopeState {
        switch status {
        case .authorized: return MigoScopeState(MIGO_SCOPE_STATE_GRANTED)
        case .denied, .restricted: return MigoScopeState(MIGO_SCOPE_STATE_DENIED)
        default: return MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN)
        }
    }

    // MARK: - the recorder service

    public func handle(_ request: MigoHostServiceRequest) {
        if let reply = request.reply {
            // The recorder's methods are commands; a call is not one of them.
            reply.fail("not supported")
            return
        }
        guard request.service == .recorder else { return }
        let object = request.object
        let id = MigoRecordingOptions.recordingId(of: object)
        switch request.method {
        case MigoHostServiceID.Recorder.start:
            start(id, object)
        case MigoHostServiceID.Recorder.pause:
            guard let capture, capture.id == id, !capture.paused else { return }
            capture.engine.pause()
            capture.paused = true
            post(MigoHostServiceID.Recorder.Event.pause, ["recordingId": id])
        case MigoHostServiceID.Recorder.resume:
            guard let capture, capture.id == id, capture.paused else { return }
            do {
                try listen(to: capture)
                capture.paused = false
                capture.interrupted = false
                post(MigoHostServiceID.Recorder.Event.resume, ["recordingId": id])
            } catch {
                fail(capture, error)
            }
        case MigoHostServiceID.Recorder.stop:
            if waiting?.recordingId == id { waiting = nil }
            if let capture, capture.id == id { finish(capture) }
        default:
            return
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
            report(error, of: id)
            return
        }
        // scope.record was the engine's to check; the system's own grant may still be undecided when the app
        // decides content's scopes itself.
        waiting = options
        askForTheMicrophone { [weak self] granted in
            guard let self, self.waiting == options else { return }
            self.waiting = nil
            guard granted else {
                self.report(MigoRecordingError("auth deny, the app may not use the microphone"), of: id)
                return
            }
            self.record(options)
        }
    }

    private func record(_ options: MigoRecordingOptions) {
        do {
            try Self.takeTheSession(for: options)
            // Made once the session records, so its input is the microphone the recording uses.
            let engine = AVAudioEngine()
            let writer = try MigoRecordingWriter(
                options: options, input: engine.inputNode.outputFormat(forBus: 0),
                directory: FileManager.default.temporaryDirectory)
            let capture = Capture(id: options.recordingId, engine: engine, writer: writer)
            do {
                try listen(to: capture)
            } catch {
                writer.discard()
                throw error
            }
            self.capture = capture
            post(MigoHostServiceID.Recorder.Event.start, ["recordingId": options.recordingId])
        } catch {
            report(error, of: options.recordingId)
        }
    }

    /// The process's audio session, set up to record: the engine lent it when it passed START on.
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
        let input = capture.engine.inputNode
        capture.engine.stop()
        input.removeTap(onBus: 0)
        let format = input.outputFormat(forBus: 0)
        try capture.work.sync { try capture.writer.retarget(input: format) }
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self, weak capture] buffer, _ in
            guard let capture else { return }
            // Written before the tap returns: the buffer is the engine's again afterwards.
            let step: Result<MigoRecordingWriter.Step, Error>? = capture.work.sync {
                guard capture.open else { return nil }
                let step = Result { try capture.writer.append(buffer) }
                if case .success(let taken) = step, taken.frames.isEmpty, !taken.full { return nil }
                return step
            }
            guard let step else { return }
            DispatchQueue.main.async { self?.took(step, of: capture) }
        }
        capture.engine.prepare()
        #if os(iOS)
            try AVAudioSession.sharedInstance().setActive(true)
        #endif
        do {
            try capture.engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
    }

    /// What one buffer came to, on the main thread, in the order the buffers came.
    private func took(_ step: Result<MigoRecordingWriter.Step, Error>, of capture: Capture) {
        guard !capture.cancelled else { return }
        switch step {
        case .failure(let error):
            if self.capture === capture { fail(capture, error) }
        case .success(let step):
            for frame in step.frames {
                reports?.postRecorderFrame(recordingId: capture.id, data: frame, isLastFrame: false)
            }
            if step.full, self.capture === capture { finish(capture) }
        }
    }

    /// The recording ends with its file: asked to, or its duration ran out.
    private func finish(_ capture: Capture) {
        let ending = close(capture) { try $0.finish() }
        // After the frames already on their way to the main thread, so the last frame is the last one heard.
        DispatchQueue.main.async { [weak self] in
            guard let self, !capture.cancelled else { return }
            switch ending {
            case .failure(let error):
                self.report(error, of: capture.id)
            case .success(let ending):
                for (index, frame) in ending.frames.enumerated() {
                    self.reports?.postRecorderFrame(
                        recordingId: capture.id, data: frame, isLastFrame: index == ending.frames.count - 1)
                }
                self.post(
                    MigoHostServiceID.Recorder.Event.stop,
                    [
                        "recordingId": capture.id, "tempFilePath": ending.url.path, "duration": ending.durationMs,
                        "fileSize": ending.fileSize,
                    ])
            }
        }
    }

    /// The recording ends with an error, and no file.
    private func fail(_ capture: Capture, _ error: Error) {
        _ = close(capture) { $0.discard() }
        report(error, of: capture.id)
    }

    /// The recording goes with nothing said of it.
    private func cancel(_ capture: Capture) {
        capture.cancelled = true
        _ = close(capture) { $0.discard() }
    }

    /// The microphone let go of and the writer closed with `end`, which no buffer follows.
    private func close<T>(_ capture: Capture, with end: (MigoRecordingWriter) throws -> T) -> Result<T, Error> {
        if self.capture === capture { self.capture = nil }
        capture.engine.stop()
        capture.engine.inputNode.removeTap(onBus: 0)
        return capture.work.sync {
            capture.open = false
            return Result { try end(capture.writer) }
        }
    }

    // MARK: - what happens to the microphone

    /// The engine's input changed -- a headset plugged in, another device chosen -- and the engine stopped.
    private func inputChanged(_ engine: AnyObject?) {
        guard let capture, capture.engine === engine, !capture.paused else { return }
        do {
            try listen(to: capture)
        } catch {
            fail(capture, error)
        }
    }

    /// The system took the microphone -- a call -- or gave it back. Taken, the recording is paused, and content
    /// hears the interruption and then the pause; given back, it is content's to resume.
    private func interrupted(began: Bool) {
        guard let capture else { return }
        if began {
            guard !capture.paused else { return }
            capture.engine.pause()
            capture.paused = true
            capture.interrupted = true
            post(MigoHostServiceID.Recorder.Event.interruptionBegin, ["recordingId": capture.id])
            post(MigoHostServiceID.Recorder.Event.pause, ["recordingId": capture.id])
        } else if capture.interrupted {
            capture.interrupted = false
            post(MigoHostServiceID.Recorder.Event.interruptionEnd, ["recordingId": capture.id])
        }
    }

    // MARK: - reporting

    private func report(_ error: Error, of id: UInt32) {
        let errMsg =
            (error as? MigoRecordingError)?.errMsg
            ?? MigoRecordingError("the microphone could not record: \(error.localizedDescription)").errMsg
        post(MigoHostServiceID.Recorder.Event.error, ["recordingId": id, "errMsg": errMsg])
    }

    private func post(_ event: UInt32, _ object: [String: Any]) {
        reports?.postEvent(.recorder, event: event, object: object)
    }
}
