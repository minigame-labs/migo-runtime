import Foundation
import MigoAppleRenderer
import MigoEngine

#if os(iOS)
    import AVFoundation
#endif

/// The camera for games, through AVFoundation, on iOS: `createCamera`'s cameras -- a preview over the game where
/// content put it, its frames, photos, video and zoom (`MigoCameraSessions`, `MigoCaptureCamera`) -- `scanCode`'s
/// scanner -- the back camera's picture under a frame, the code in it read as it comes, or a picture from the photo
/// library read by Vision unless content asked for the camera only -- content's `scope.camera`, and the camera's
/// grant for `getAppAuthorizeSetting`.
///
/// A camera content creates hands it the camera's picture, so it needs content's `scope.camera`, which this decides
/// with the system's prompt. The scanner is the SDK's own screen, as a system picker is the system's: content hears
/// only the code the player chose to scan, never the camera's picture, so it uses the app's camera grant without the
/// scope.
///
/// An app opts in by linking this product, passing a `MigoCamera()` among its game view's capabilities, and declaring
/// why it uses the camera: `NSCameraUsageDescription`. Without the purpose string the system shows no prompt: the
/// scope is refused, and the scanner reads the photo library only. It is a product of its own because App Store
/// review requires that purpose string of any app whose binary uses the camera. A recording carries the app's sound
/// only when the app may already use the microphone; the camera asks for none.
///
/// Create it on the main thread; it is used there.
public final class MigoCamera: MigoSystemCapability {
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count these APIs as provided on the Apple
    /// platforms: one service per line, platform-specific ones inside `#if os(...)`.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        #if os(iOS)
            builtIn.insert(.scanCode)
            builtIn.insert(.camera)
        #endif
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    public var scopes: Set<MigoScope> {
        #if os(iOS)
            return [MigoScope(MIGO_SCOPE_CAMERA)]
        #else
            return []
        #endif
    }

    #if os(iOS)
        /// The scanner up, if one is.
        private weak var scanner: MigoScannerViewController?
        private var reports: MigoCapabilityReports?
        /// `createCamera`'s cameras, each shown in the game's view.
        private lazy var cameras = MigoCameraSessions(
            grant: { [weak self] answer in self?.askForTheCamera(answer) ?? answer(false) },
            device: { [weak self] in MigoCaptureCamera(stage: self?.reports?.stage) },
            post: { [weak self] event, object in
                self?.reports?.postEvent(.camera, event: event.rawValue, object: object)
            },
            frame: { [weak self] camera, picture in
                self?.reports?.postCameraFrame(
                    cameraId: UInt32(camera), width: picture.width, height: picture.height, rowStride: picture.rowStride,
                    format: MigoPixelFormat(picture.bgra ? MIGO_PIXEL_FORMAT_BGRA8 : MIGO_PIXEL_FORMAT_RGBA8),
                    pixels: picture.pixels)
            })
    #endif

    public init() {}

    public func report(to reports: MigoCapabilityReports?) {
        #if os(iOS)
            self.reports = reports
            guard reports != nil else {
                // The session closed: the scanner up answers nobody, and goes; so do the cameras.
                scanner?.finish(.cancelled)
                cameras.close()
                return
            }
            reportState()
        #endif
    }

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        #if os(iOS)
            guard scope == MigoScope(MIGO_SCOPE_CAMERA) else {
                completion(false)
                return
            }
            askForTheCamera(completion)
        #else
            completion(false)
        #endif
    }

    #if os(iOS)
        /// Whether the app may use the camera, asking the player when the system has not decided; the grant is
        /// reported before the answer, so content that acts on it finds it recorded. Answers on the main thread.
        private func askForTheCamera(_ completion: @escaping (Bool) -> Void) {
            let status = AVCaptureDevice.authorizationStatus(for: .video)
            guard status == .notDetermined else {
                // Decided already: the player changes it in Settings, not here.
                reportState()
                completion(status == .authorized)
                return
            }
            guard (Bundle.main.object(forInfoDictionaryKey: "NSCameraUsageDescription") as? String)?.isEmpty == false else {
                NSLog(
                    "MigoCamera: the app declares no NSCameraUsageDescription in its Info.plist, so the system shows "
                        + "no prompt and scope.camera is refused")
                completion(false)
                return
            }
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async { [weak self] in
                    self?.reportState()
                    completion(granted)
                }
            }
        }

        private func reportState() {
            guard let reports else { return }
            let status = AVCaptureDevice.authorizationStatus(for: .video)
            reports.updateAppAuthorization { $0.camera = Self.authorization(status) }
            reports.setScopeState(MigoScope(MIGO_SCOPE_CAMERA), Self.scopeState(status))
        }

        static func scopeState(_ status: AVAuthorizationStatus) -> MigoScopeState {
            switch status {
            case .authorized: return MigoScopeState(MIGO_SCOPE_STATE_GRANTED)
            case .denied, .restricted: return MigoScopeState(MIGO_SCOPE_STATE_DENIED)
            default: return MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN)
            }
        }
    #endif

    public func handle(_ request: MigoHostServiceRequest) {
        #if os(iOS)
            if request.service == .camera {
                // Calls and commands both: a command has no reply to answer.
                guard let method = MigoCameraSessions.Method(rawValue: request.method) else {
                    request.reply?.fail("not supported")
                    return
                }
                let reply = request.reply
                cameras.handle(
                    method, request.object,
                    answer: reply.map { reply in
                        { outcome in
                            switch outcome {
                            case .success(let result): reply.succeed(result)
                            case .failure(let failure): reply.fail(failure.reason)
                            }
                        }
                    })
                return
            }
        #endif
        guard let reply = request.reply else { return }
        #if os(iOS)
            guard request.service == .scanCode, request.method == MigoHostServiceID.ScanCode.scanCode else {
                reply.fail("not supported")
                return
            }
            guard scanner == nil else {
                reply.fail("a scanner is showing")
                return
            }
            let options = request.object
            let shown = MigoScannerViewController(
                kinds: MigoCodeKinds(scanType: options["scanType"] as? [Any]),
                cameraOnly: options["onlyFromCamera"] as? Bool ?? false
            ) { outcome in
                switch outcome {
                case .code(let answer): reply.succeed(answer)
                case .cancelled: reply.fail("cancel")
                case .failed(let reason): reply.fail(reason)
                }
            }
            scanner = shown
            MigoSystemPresentation.present(shown, shown: { up in
                if !up { shown.finish(.failed("there is no window to show it in")) }
            })
        #else
            reply.fail("not supported")
        #endif
    }

    #if os(iOS)
        static func authorization(_ status: AVAuthorizationStatus) -> UInt8 {
            switch status {
            case .authorized: return UInt8(MIGO_AUTHORIZATION_AUTHORIZED)
            case .denied, .restricted: return UInt8(MIGO_AUTHORIZATION_DENIED)
            default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
            }
        }
    #endif
}
