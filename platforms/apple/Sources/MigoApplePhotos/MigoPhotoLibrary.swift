import Foundation
import MigoAppleRenderer
import MigoEngine
import Photos

/// The photo library for games, through PhotoKit: `saveImageToPhotosAlbum`,
/// the app's photo-library grant (`getAppAuthorizeSetting().albumAuthorized`),
/// and `scope.writePhotosAlbum` decided by the system's own prompt.
///
/// It asks only to add to the library, never to read it -- the pickers need no
/// access at all. An app opts in by linking this product, passing a
/// `MigoPhotoLibrary()` among its game view's capabilities, and declaring
/// `NSPhotoLibraryAddUsageDescription`; without it the system shows no prompt,
/// so the scope is refused without asking.
///
/// Create it on the main thread; it is used there.
public final class MigoPhotoLibrary: MigoPhotoLibrarySaving {
    /// It answers no whole service: the SDK's image service hands it
    /// `saveImageToPhotosAlbum`.
    public var services: Set<MigoHostServiceID> { [] }
    public var scopes: Set<MigoScope> { [MigoScope(MIGO_SCOPE_WRITE_PHOTOS_ALBUM)] }

    private var reports: MigoCapabilityReports?
    private var scopeWaiters: [(Bool) -> Void] = []

    public init() {}

    public func handle(_ request: MigoHostServiceRequest) {
        request.reply?.fail("not supported")
    }

    // MARK: - reports

    public func report(to reports: MigoCapabilityReports?) {
        self.reports = reports
        guard reports != nil else {
            let waiters = scopeWaiters
            scopeWaiters = []
            waiters.forEach { $0(false) }
            return
        }
        reportState()
    }

    private func reportState() {
        guard let reports else { return }
        let status = PHPhotoLibrary.authorizationStatus(for: .addOnly)
        reports.updateAppAuthorization { $0.album = Self.authorization(status) }
        reports.setScopeState(MigoScope(MIGO_SCOPE_WRITE_PHOTOS_ALBUM), Self.scopeState(status))
    }

    static func isAuthorized(_ status: PHAuthorizationStatus) -> Bool {
        status == .authorized || status == .limited
    }

    static func authorization(_ status: PHAuthorizationStatus) -> UInt8 {
        if isAuthorized(status) { return UInt8(MIGO_AUTHORIZATION_AUTHORIZED) }
        switch status {
        case .denied, .restricted: return UInt8(MIGO_AUTHORIZATION_DENIED)
        default: return UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED)
        }
    }

    static func scopeState(_ status: PHAuthorizationStatus) -> MigoScopeState {
        if isAuthorized(status) { return MigoScopeState(MIGO_SCOPE_STATE_GRANTED) }
        switch status {
        case .denied, .restricted: return MigoScopeState(MIGO_SCOPE_STATE_DENIED)
        default: return MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN)
        }
    }

    // MARK: - the scope

    public func requestScope(_ scope: MigoScope, completion: @escaping (Bool) -> Void) {
        guard scope == MigoScope(MIGO_SCOPE_WRITE_PHOTOS_ALBUM) else {
            completion(false)
            return
        }
        let status = PHPhotoLibrary.authorizationStatus(for: .addOnly)
        guard status == .notDetermined else {
            // Decided already: the player changes it in Settings, not here.
            reportState()
            completion(Self.isAuthorized(status))
            return
        }
        guard Self.declaresPurpose else {
            NSLog(
                "MigoPhotoLibrary: the app declares no NSPhotoLibraryAddUsageDescription in its Info.plist, so the "
                    + "system shows no prompt and scope.writePhotosAlbum is refused")
            completion(false)
            return
        }
        scopeWaiters.append(completion)
        guard scopeWaiters.count == 1 else { return }
        PHPhotoLibrary.requestAuthorization(for: .addOnly) { [weak self] status in
            DispatchQueue.main.async {
                guard let self else { return }
                self.reportState()
                let waiters = self.scopeWaiters
                self.scopeWaiters = []
                waiters.forEach { $0(Self.isAuthorized(status)) }
            }
        }
    }

    static var declaresPurpose: Bool {
        (Bundle.main.object(forInfoDictionaryKey: "NSPhotoLibraryAddUsageDescription") as? String)?.isEmpty == false
    }

    // MARK: - saving

    public func saveImageToPhotoLibrary(_ file: URL, completion: @escaping (String?) -> Void) {
        guard Self.isAuthorized(PHPhotoLibrary.authorizationStatus(for: .addOnly)) else {
            completion("system permission denied")
            return
        }
        // A copy of the file goes into the library before the call is
        // answered, which is all the engine lends it for.
        PHPhotoLibrary.shared().performChanges({
            PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL: file, options: nil)
        }) { saved, error in
            DispatchQueue.main.async {
                completion(saved ? nil : (error?.localizedDescription ?? "the image could not be saved"))
            }
        }
    }
}
