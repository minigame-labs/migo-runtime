import Foundation

#if os(iOS)
    import UIKit
#elseif os(macOS)
    import AppKit
#endif

/// The screen service, answered by the system: on iOS the screen's brightness,
/// screenshots, screen recording and the device's turning; on a Mac keeping the
/// game's window out of screenshots and recordings. What a platform has no
/// public means for is refused as not supported, and the API x platform matrix
/// records why.
final class MigoSystemScreen {
    /// Where the service's events go: the session's channel.
    private let post: (UInt32, [String: Any]) -> Void
    /// Where content's `setEnableDebug` goes: the session, which turns its
    /// debugging aids.
    var debugAids: ((Bool) -> Void)?
    private var observers: [String: NSObjectProtocol] = [:]

    #if os(macOS)
        /// The game's window: the app's key window by default.
        private let window: () -> NSWindow?

        init(
            post: @escaping (UInt32, [String: Any]) -> Void,
            window: @escaping () -> NSWindow? = { NSApp.keyWindow ?? NSApp.mainWindow }
        ) {
            self.post = post
            self.window = window
        }
    #else
        /// The screen turning is the game view's to report: the screen's
        /// orientation is its scene's, which a turn of the device need not
        /// change.
        init(post: @escaping (UInt32, [String: Any]) -> Void) {
            self.post = post
        }
    #endif

    deinit {
        observers.values.forEach(NotificationCenter.default.removeObserver)
    }

    func handle(_ request: MigoHostServiceRequest) {
        typealias Method = MigoHostServiceID.Screen
        switch request.method {
        case Method.startCaptureObserver:
            #if os(iOS)
                observe("capture", UIApplication.userDidTakeScreenshotNotification) { [weak self] in
                    self?.post(MigoHostServiceID.Screen.Event.userCaptureScreen, [:])
                }
            #endif
        case Method.stopCaptureObserver:
            stopObserving("capture")
        case Method.startRecordingObserver:
            #if os(iOS)
                observe("recording", UIScreen.capturedDidChangeNotification) { [weak self] in
                    self?.post(MigoHostServiceID.Screen.Event.recordingStateChange, ["state": Self.recordingState()])
                }
            #endif
        case Method.stopRecordingObserver:
            stopObserving("recording")
        case Method.setEnableDebug:
            debugAids?(request.object["enableDebug"] as? Bool == true)
        default:
            answer(request)
        }
    }

    /// The calls, each answered once.
    private func answer(_ request: MigoHostServiceRequest) {
        guard let reply = request.reply else { return }
        typealias Method = MigoHostServiceID.Screen
        switch request.method {
        #if os(iOS)
            case Method.getBrightness:
                reply.succeed(["value": Double(UIScreen.main.brightness)])
            case Method.setBrightness:
                guard let value = (request.object["value"] as? NSNumber)?.doubleValue, value.isFinite else {
                    reply.fail("value is not a number")
                    return
                }
                UIScreen.main.brightness = CGFloat(min(max(value, 0), 1))
                reply.succeed()
            case Method.getRecordingState:
                reply.succeed(["state": Self.recordingState()])
        #elseif os(macOS)
            case Method.setVisualEffectOnCapture:
                let hidden: Bool
                switch request.object["visualEffect"] as? String {
                case "hidden": hidden = true
                case "none": hidden = false
                default:
                    reply.fail("visualEffect is neither none nor hidden")
                    return
                }
                guard let window = window() else {
                    reply.fail("there is no window to protect")
                    return
                }
                // Excluded from what other processes may read: screenshots,
                // recordings and screen sharing show the window blank.
                window.sharingType = hidden ? .none : .readOnly
                reply.succeed()
        #endif
        default:
            reply.fail("not supported")
        }
    }

    private func observe(_ name: String, _ notification: Notification.Name, _ handler: @escaping () -> Void) {
        stopObserving(name)
        observers[name] = NotificationCenter.default.addObserver(
            forName: notification, object: nil, queue: .main
        ) { _ in handler() }
    }

    private func stopObserving(_ name: String) {
        if let observer = observers.removeValue(forKey: name) {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    #if os(iOS)
        private static func recordingState() -> String {
            UIScreen.main.isCaptured ? "on" : "off"
        }
    #endif
}
