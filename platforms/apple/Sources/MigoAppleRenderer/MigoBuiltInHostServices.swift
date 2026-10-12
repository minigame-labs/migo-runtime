import Foundation

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// What the SDK supplies itself of the host-service channel: the services the
/// system already offers the app -- the pasteboard, its dialogs and
/// indicators, its screen, its settings pages, its pickers and image encoder,
/// and on iOS its motion sensors --
/// answered by the system's own, for every service the app does not name. An app that names a
/// service answers it instead, and one that wants a service refused names it
/// and refuses.
///
/// `scripts/gen-api-platform-matrix.py` reads `services` below to count these
/// APIs as provided on Apple: one service per line, platform-specific ones
/// inside `#if os(...)`.
final class MigoBuiltInHostServices: MigoHostServiceProvider {
    static let services: Set<MigoHostServiceID> = {
        var builtIn: Set<MigoHostServiceID> = []
        builtIn.insert(.clipboard)
        builtIn.insert(.interaction)
        builtIn.insert(.screen)
        builtIn.insert(.setting)
        builtIn.insert(.image)
        #if os(iOS)
            builtIn.insert(.motion)
        #endif
        return builtIn
    }()

    /// The permission service, for the scopes the linked capabilities decide;
    /// `nil` when none does.
    private let permission: MigoSystemPermission?

    let services: Set<MigoHostServiceID>

    /// `appServices` are the services the app answers itself: when they
    /// include permission, content's scopes are the app's to decide.
    init(capabilities: [MigoSystemCapability] = [], appServices: Set<MigoHostServiceID> = []) {
        self.capabilities = capabilities
        permission = MigoSystemPermission(capabilities: capabilities)
        services = permission == nil ? Self.services : Self.services.union([.permission])
        settings = MigoSystemSettings(decidesScopes: permission != nil && !appServices.contains(.permission))
    }

    /// Kept for the session: a settings page is answered when the player
    /// returns.
    private let settings: MigoSystemSettings

    /// Has the linked capabilities report again; the session sets it.
    var refreshCapabilities: (() -> Void)? {
        get { settings.refresh }
        set { settings.refresh = newValue }
    }

    /// Kept for the session: which indicator is up is state.
    private lazy var interaction = MigoSystemInteraction(presenter: MigoSystemInteractionPresenter())
    /// Kept for the session: a picker up is state.
    private lazy var images = MigoSystemImages(
        photoLibrary: capabilities.lazy.compactMap { $0 as? MigoPhotoLibrarySaving }.first)
    private let capabilities: [MigoSystemCapability]
    /// Kept for the session: what content listens for is state.
    private lazy var screen = MigoSystemScreen(post: { [weak self] event, object in
        self?.post?(.screen, event, object)
    })

    /// Where a service's events go: the session's channel, once it is open.
    var post: ((MigoHostServiceID, UInt32, [String: Any]) -> Void)?
    #if os(iOS)
        /// Where the motion sensors' readings go: typed, up to fifty a second.
        var postSample: MigoSystemMotion.Sample?
        /// Kept for the session: which sensors run is state.
        private lazy var motion = MigoSystemMotion(post: { [weak self] kind, values, accuracy in
            self?.postSample?(kind, values, accuracy)
        })
    #endif

    func handle(_ request: MigoHostServiceRequest) {
        switch request.service {
        case .clipboard:
            MigoSystemClipboard.handle(request)
        case .interaction:
            interaction.handle(request)
        case .screen:
            screen.handle(request)
        case .permission where permission != nil:
            permission?.handle(request)
        case .setting:
            settings.handle(request)
        case .image:
            images.handle(request)
        #if os(iOS)
            case .motion:
                motion.handle(request)
        #endif
        default:
            request.reply?.fail("not supported")
        }
    }
}

/// The app's provider, the capabilities it linked and the SDK's own, as the
/// session sees them: a service the app names is the app's; then the first
/// capability that names it; any other the SDK answers if it can.
final class MigoComposedHostServices: MigoHostServiceProvider {
    private let app: MigoHostServiceProvider?
    private let appServices: Set<MigoHostServiceID>
    private let capabilities: [MigoSystemCapability]
    /// The capability the Bluetooth service's beacon methods go to, if one
    /// ranges beacons.
    private let beaconRanging: MigoBeaconRanging?
    let builtIn: MigoBuiltInHostServices
    let services: Set<MigoHostServiceID>

    init(app: MigoHostServiceProvider?, capabilities: [MigoSystemCapability] = []) {
        self.app = app
        self.capabilities = capabilities
        appServices = app?.services ?? []
        beaconRanging = capabilities.lazy.compactMap { $0 as? MigoBeaconRanging }.first
        builtIn = MigoBuiltInHostServices(capabilities: capabilities, appServices: appServices)
        var services = capabilities.reduce(appServices.union(builtIn.services)) { $0.union($1.services) }
        // Ranging is offered through the Bluetooth service even where no
        // capability answers the rest of it.
        if beaconRanging != nil { services.insert(.bluetooth) }
        self.services = services
    }

    /// The Bluetooth service's beacon methods.
    private static let beaconMethods: Set<UInt32> = [
        MigoHostServiceID.Bluetooth.startBeaconDiscovery,
        MigoHostServiceID.Bluetooth.stopBeaconDiscovery,
        MigoHostServiceID.Bluetooth.getBeacons,
    ]

    func handle(_ request: MigoHostServiceRequest) {
        if let app, appServices.contains(request.service) {
            app.handle(request)
        } else if let beaconRanging, request.service == .bluetooth,
            Self.beaconMethods.contains(request.method)
        {
            beaconRanging.handleBeacons(request)
        } else if let capability = capabilities.first(where: { $0.services.contains(request.service) }) {
            capability.handle(request)
        } else {
            builtIn.handle(request)
        }
    }
}

/// The system pasteboard: `setClipboardData` writes plain text to it,
/// `getClipboardData` reads it back.
enum MigoSystemClipboard {
    static func handle(_ request: MigoHostServiceRequest) {
        switch request.method {
        case MigoHostServiceID.Clipboard.setClipboardData:
            guard let text = request.object["data"] as? String else {
                request.reply?.fail("data is not a string")
                return
            }
            write(text)
            request.reply?.succeed()
        case MigoHostServiceID.Clipboard.getClipboardData:
            request.reply?.succeed(["data": read()])
        default:
            request.reply?.fail("not supported")
        }
    }

    static func write(_ text: String) {
        #if os(iOS)
            UIPasteboard.general.string = text
        #elseif os(macOS)
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(text, forType: .string)
        #endif
    }

    static func read() -> String {
        #if os(iOS)
            return UIPasteboard.general.string ?? ""
        #elseif os(macOS)
            return NSPasteboard.general.string(forType: .string) ?? ""
        #else
            return ""
        #endif
    }
}
