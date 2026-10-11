import Foundation

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// What the SDK supplies itself of the host-service channel: the services the
/// system already offers the app -- the pasteboard first -- answered by the
/// system's own, for every service the app does not name. An app that names a
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
        return builtIn
    }()

    var services: Set<MigoHostServiceID> { Self.services }

    func handle(_ request: MigoHostServiceRequest) {
        switch request.service {
        case .clipboard:
            MigoSystemClipboard.handle(request)
        default:
            request.reply?.fail("not supported")
        }
    }
}

/// The app's provider and the SDK's, as the session sees them: a service the
/// app names is the app's; any other the SDK answers if it can.
final class MigoComposedHostServices: MigoHostServiceProvider {
    private let app: MigoHostServiceProvider?
    private let builtIn = MigoBuiltInHostServices()
    private let appServices: Set<MigoHostServiceID>
    let services: Set<MigoHostServiceID>

    init(app: MigoHostServiceProvider?) {
        self.app = app
        appServices = app?.services ?? []
        services = appServices.union(builtIn.services)
    }

    func handle(_ request: MigoHostServiceRequest) {
        if let app, appServices.contains(request.service) {
            app.handle(request)
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
