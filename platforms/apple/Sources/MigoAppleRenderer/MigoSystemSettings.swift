import Foundation

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// The settings pages content can send the player to, answered with the
/// system's own: this app's page in Settings on iOS, Privacy & Security in
/// System Settings on a Mac -- where the grants behind getAppAuthorizeSetting
/// are changed.
///
/// `openAppAuthorizeSetting` always opens it. `openSetting` -- the page for
/// content's scopes -- opens it only while the SDK decides those scopes with the
/// system's prompts (a linked capability does and the app answers no permission
/// service): the system's page is then where they change. Otherwise the scopes
/// are nobody's here, or the app's, whose own page this is not. The platform
/// documents `openSystemBluetoothSetting` for Android only, and iOS offers apps
/// no link to its Bluetooth settings.
///
/// A call is answered when the player comes back to the app. The capabilities
/// report first, so content's settings read what the player left.
final class MigoSystemSettings {
    /// Opens a URL, answering whether it did.
    typealias Opener = (URL, @escaping (Bool) -> Void) -> Void

    private let decidesScopes: Bool
    private let open: Opener
    private let returned: Notification.Name
    private var waiting: [MigoHostServiceReply] = []
    private var observer: NSObjectProtocol?

    /// Report the capabilities' state again: the player may have changed a
    /// grant while away.
    var refresh: (() -> Void)?

    init(decidesScopes: Bool, open: @escaping Opener = MigoSystemSettings.systemOpen,
        returned: Notification.Name = MigoSystemSettings.didBecomeActive)
    {
        self.decidesScopes = decidesScopes
        self.open = open
        self.returned = returned
    }

    deinit {
        if let observer { NotificationCenter.default.removeObserver(observer) }
    }

    func handle(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        switch request.method {
        case MigoHostServiceID.Setting.openAppAuthorizeSetting:
            openAppSettings(reply)
        case MigoHostServiceID.Setting.openSetting where decidesScopes:
            openAppSettings(reply)
        default:
            reply.fail("not supported")
        }
    }

    private func openAppSettings(_ reply: MigoHostServiceReply) {
        guard let url = Self.appSettings else {
            reply.fail("not supported")
            return
        }
        open(url) { [weak self] opened in
            guard let self else { return }
            guard opened else {
                reply.fail("the settings could not be opened")
                return
            }
            self.wait(reply)
        }
    }

    private func wait(_ reply: MigoHostServiceReply) {
        waiting.append(reply)
        guard observer == nil else { return }
        observer = NotificationCenter.default.addObserver(forName: returned, object: nil, queue: .main) {
            [weak self] _ in self?.playerReturned()
        }
    }

    private func playerReturned() {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        refresh?()
        let replies = waiting
        waiting = []
        replies.forEach { $0.succeed() }
    }

    /// This app's settings in the system's.
    static var appSettings: URL? {
        #if os(iOS)
            return URL(string: UIApplication.openSettingsURLString)
        #elseif os(macOS)
            return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy")
        #else
            return nil
        #endif
    }

    static let systemOpen: Opener = { url, completion in
        #if os(iOS)
            UIApplication.shared.open(url, options: [:], completionHandler: completion)
        #elseif os(macOS)
            completion(NSWorkspace.shared.open(url))
        #else
            completion(false)
        #endif
    }

    #if os(iOS)
        static let didBecomeActive = UIApplication.didBecomeActiveNotification
    #elseif os(macOS)
        static let didBecomeActive = NSApplication.didBecomeActiveNotification
    #else
        static let didBecomeActive = Notification.Name("MigoSystemSettingsNeverReturns")
    #endif
}
