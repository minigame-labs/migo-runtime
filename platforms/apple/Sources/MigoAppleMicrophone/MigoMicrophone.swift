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
/// the recorder sets the category it records with -- play and record, the game's sound staying on the speaker or
/// the headphones it was on -- and never restores one.
///
/// `audioSource` picks the built-in or the wired headset's microphone on iOS (`auto`, and every source on a Mac,
/// is the system's choice). `audioMode` asks for processing the system decides by itself here, and is not acted on.
///
/// Create it on the main thread; it is used there. The recording itself -- opening the microphone, writing the
/// file -- happens off it (`MigoRecorder`), so a game's frames do not wait on either.
public final class MigoMicrophone: MigoSystemCapability {
    /// The services it answers, in the form `scripts/gen-api-platform-matrix.py` reads a capability's: one service
    /// per line, platform-specific ones inside `#if os(...)`. The matrix counts this product once a conformance
    /// lane has recorded with it on a device's microphone.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        builtIn.insert(.recorder)
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    public var scopes: Set<MigoScope> { [MigoScope(MIGO_SCOPE_RECORD)] }

    private var reports: MigoCapabilityReports?
    private lazy var recorder = MigoRecorder { [weak self] event in self?.heard(event) }

    public init() {}

    // MARK: - what the system says

    public func report(to reports: MigoCapabilityReports?) {
        self.reports = reports
        guard reports != nil else {
            // The session closed: the recording under way is nobody's.
            recorder.close()
            return
        }
        reportState()
    }

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        guard scope == MigoScope(MIGO_SCOPE_RECORD) else {
            completion(false)
            return
        }
        MigoMicrophoneGrant.ask { granted in
            DispatchQueue.main.async { [weak self] in
                // Reported before the answer, so content that acts on the grant finds it recorded.
                self?.reportState()
                completion(granted)
            }
        }
    }

    private func reportState() {
        guard let reports else { return }
        let status = MigoMicrophoneGrant.status
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
        let asked: MigoRecorder.Request
        switch request.method {
        case MigoHostServiceID.Recorder.start: asked = .start
        case MigoHostServiceID.Recorder.pause: asked = .pause
        case MigoHostServiceID.Recorder.resume: asked = .resume
        case MigoHostServiceID.Recorder.stop: asked = .stop
        default: return
        }
        recorder.handle(asked, request.object)
    }

    /// What the recorder did, as the service's events and frames.
    private func heard(_ event: MigoRecorder.Event) {
        typealias Recorded = MigoHostServiceID.Recorder.Event
        switch event {
        case .started(let id):
            post(Recorded.start, ["recordingId": id])
        case .paused(let id):
            post(Recorded.pause, ["recordingId": id])
        case .resumed(let id):
            post(Recorded.resume, ["recordingId": id])
        case .stopped(let id, let file, let durationMs, let fileSize):
            post(
                Recorded.stop,
                ["recordingId": id, "tempFilePath": file.path, "duration": durationMs, "fileSize": fileSize])
        case .failed(let id, let errMsg):
            post(Recorded.error, ["recordingId": id, "errMsg": errMsg])
        case .interruptionBegan(let id):
            post(Recorded.interruptionBegin, ["recordingId": id])
        case .interruptionEnded(let id):
            post(Recorded.interruptionEnd, ["recordingId": id])
        case .frame(let id, let data, let isLast):
            reports?.postRecorderFrame(recordingId: id, data: data, isLastFrame: isLast)
        case .grantAsked:
            reportState()
        }
    }

    private func post(_ event: UInt32, _ object: [String: Any]) {
        reports?.postEvent(.recorder, event: event, object: object)
    }
}
