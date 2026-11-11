import Foundation
import MigoAppleRenderer
import MigoEngine

#if os(iOS)
    import AVFoundation
#endif

/// The camera for games, read with AVFoundation: on iOS `scanCode`'s scanner -- the back camera's picture under a
/// frame, the code in it read as it comes, or a picture from the photo library read by Vision unless content asked
/// for the camera only -- and the camera's grant for `getAppAuthorizeSetting`.
///
/// The scanner is the SDK's own screen, as a system picker is the system's: content hears only the code the player
/// chose to scan, never the camera's picture, so it uses the app's camera grant without content's `scope.camera`.
///
/// An app opts in by linking this product, passing a `MigoCamera()` among its game view's capabilities, and declaring
/// why it uses the camera: `NSCameraUsageDescription`. Without the purpose string the system shows no prompt, and the
/// scanner reads the photo library only. It is a product of its own because App Store review requires that purpose
/// string of any app whose binary uses the camera.
///
/// Create it on the main thread; it is used there.
public final class MigoCamera: MigoSystemCapability {
    /// `scripts/gen-api-platform-matrix.py` reads `services` below to count these APIs as provided on the Apple
    /// platforms: one service per line, platform-specific ones inside `#if os(...)`.
    public static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        #if os(iOS)
            builtIn.insert(.scanCode)
        #endif
        return builtIn
    }()

    public var services: Set<MigoHostServiceID> { Self.services }
    /// None yet: `scope.camera` is `createCamera`'s, which this product does not offer.
    public var scopes: Set<MigoScope> { [] }

    #if os(iOS)
        /// The scanner up, if one is.
        private weak var scanner: MigoScannerViewController?
    #endif

    public init() {}

    public func report(to reports: MigoCapabilityReports?) {
        guard let reports else {
            #if os(iOS)
                // The session closed: the scanner up answers nobody, and goes.
                scanner?.finish(.cancelled)
            #endif
            return
        }
        #if os(iOS)
            let status = AVCaptureDevice.authorizationStatus(for: .video)
            reports.updateAppAuthorization { $0.camera = Self.authorization(status) }
        #endif
    }

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        completion(false)
    }

    public func handle(_ request: MigoHostServiceRequest) {
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
