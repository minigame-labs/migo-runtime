import Foundation
import MigoAppleCore
import WebKit

#if os(iOS)
    import UIKit

    /// What a host app hears from a running session.
    ///
    /// Every method has a default, so a host implements the ones it acts on. The
    /// defaults are deliberately not silent for the two failures a host cannot
    /// otherwise discover -- a terminal crash loop and a refused navigation -- because
    /// a session that fails quietly is a session whose failure the user reports
    /// instead.
    public protocol MigoWebKitSessionDelegate: AnyObject {
        /// Content reported its own error or readiness. Write-only and rate-limited:
        /// see `MigoWebKitSession.diagnosticsPerSecond`.
        func session(_ session: MigoWebKitSession, didReceiveDiagnostic diagnostic: [String: Any])

        /// The content process died and the session is rebuilding it.
        func session(_ session: MigoWebKitSession, willRebuildAfterTermination count: Int)

        /// It died more times than the budget allows. Nothing further is rebuilt; the
        /// host shows its own terminal state.
        func session(_ session: MigoWebKitSession, didFailTerminallyAfter count: Int)

        /// Fields the host adds to what `environment.get` answers. Called per request,
        /// so a host may change them between sessions.
        func sessionEnvironmentFields(_ session: MigoWebKitSession) -> [String: String]

        /// Content tried to leave its own origin and was stopped.
        func session(_ session: MigoWebKitSession, refusedNavigationTo url: URL)
    }

    extension MigoWebKitSessionDelegate {
        public func session(
            _ session: MigoWebKitSession, didReceiveDiagnostic diagnostic: [String: Any]
        ) {}
        public func session(_ session: MigoWebKitSession, willRebuildAfterTermination count: Int) {}
        public func session(_ session: MigoWebKitSession, didFailTerminallyAfter count: Int) {}
        public func sessionEnvironmentFields(_ session: MigoWebKitSession) -> [String: String] { [:] }
        public func session(_ session: MigoWebKitSession, refusedNavigationTo url: URL) {}
    }

    /// The WebKit Full lane: WebKit runs the JavaScript and WebKit renders.
    ///
    /// The lane links no engine, by contract
    /// (`scripts/test-apple-shipping-package-contract.sh` fails if anything in its
    /// closure reaches `MigoEngine`) and for a reason: putting ANGLE and Skia into an
    /// app that chose the compatibility baseline would ship a renderer nothing in it
    /// ever calls.
    ///
    /// What this class owns is a container view whose web view it may replace. The
    /// replacement is the point: WebKit's content process dies under memory pressure,
    /// and the recovery is a new web view, not a reload of a dead one. Keeping the
    /// swap inside a container the host installed once means a rebuild never touches
    /// the host's view hierarchy -- and means the host cannot be holding a reference
    /// to the web view that just died.
    ///
    /// The decisions it makes are all in `MigoAppleCore`, where a pull request tests
    /// them: which methods exist (`MigoWebKitSurface`), what content sees
    /// (`MigoWebKitBridgeShim`), what a request resolves to
    /// (`MigoWebKitOriginRules`), and what a termination means
    /// (`MigoWebContentRecovery`). This file is the wiring.
    ///
    /// **iOS only, and stated rather than implied.** The lifecycle this session
    /// delivers is UIKit's -- an app that gets suspended and may be killed while it
    /// is -- and macOS has neither that shape nor that risk. An `#else` branch here
    /// would be a second arm no lane compiles, which this repository has paid for
    /// twice: the C ABI's ILP32 assertions were written, never compiled, and wrong
    /// about two pointer widths.
    public final class MigoWebKitSession: NSObject {

        /// The compliance surface this session serves.
        public let surface: MigoWebKitSurface

        /// The container the host installs. Its web view is replaced on recovery.
        public let view: UIView

        public weak var delegate: MigoWebKitSessionDelegate?

        /// The live web view, or nil before `start()` and after a terminal failure.
        public private(set) var webView: WKWebView?

        /// How many diagnostics a second content may report.
        ///
        /// A bound rather than a courtesy: `diagnostics.report` crosses into the host
        /// and calls a delegate on the main thread, so content in a failure loop can
        /// spend the frame budget telling the host it has no frame budget. Dropped
        /// reports are counted and the count travels with the next one that gets
        /// through, so the host learns that it is being throttled rather than seeing
        /// a quiet gap.
        public static let diagnosticsPerSecond = 20

        private let origin: MigoWebKitContentOrigin
        private var recovery: MigoWebContentRecovery
        private var handlerMethods: [String: String] = [:]
        private var lifecycleSubscribed = false
        private var lifecycleObservers: [NSObjectProtocol] = []

        private var diagnosticWindowStart = Date.distantPast
        private var diagnosticsInWindow = 0
        private var diagnosticsDropped = 0

        public init(
            surface: MigoWebKitSurface = .default,
            contentRoot: URL,
            indexPath: String = "index.html",
            terminationBudget: Int = 2,
            delegate: MigoWebKitSessionDelegate? = nil
        ) {
            self.surface = surface
            self.origin = MigoWebKitContentOrigin(root: contentRoot, indexPath: indexPath)
            self.recovery = MigoWebContentRecovery(terminationBudget: terminationBudget)
            self.delegate = delegate
            self.view = UIView(frame: .zero)
            super.init()
            self.view.backgroundColor = .black
        }

        deinit {
            // Not in a `stop()` the host has to remember: an observer left registered
            // against a deallocated session is a crash at the next notification, and
            // the notification is one every app posts.
            for observer in lifecycleObservers {
                NotificationCenter.default.removeObserver(observer)
            }
        }

        // MARK: - starting

        /// Build the web view, load the content, and begin observing the lifecycle.
        public func start() {
            observeLifecycleIfNeeded()
            build()
        }

        private func build() {
            let configuration = WKWebViewConfiguration()
            configuration.setURLSchemeHandler(origin, forURLScheme: MigoWebKitContentOrigin.scheme)

            // The page world, because content has to be able to call the shim and an
            // isolated world is invisible to it. Document start, because content that
            // runs before the shim exists sees a surface that is not there yet.
            let controller = WKUserContentController()
            controller.addUserScript(
                WKUserScript(
                    source: MigoWebKitBridgeShim.script(for: surface),
                    injectionTime: .atDocumentStart, forMainFrameOnly: true))

            handlerMethods = [:]
            for method in surface.servedBridgeMethods {
                let name = MigoWebKitBridgeShim.handlerName(forMethod: method)
                handlerMethods[name] = method
                // The reply variant, so a call is a promise WebKit settles rather than
                // a message plus a callback the shim would have to correlate.
                controller.addScriptMessageHandler(self, contentWorld: .page, name: name)
            }
            configuration.userContentController = controller

            // Content is a mini game, not a browser: no back-forward gestures, no data
            // detectors, and no inline media autoplay decision made for it.
            configuration.allowsInlineMediaPlayback = true
            configuration.mediaTypesRequiringUserActionForPlayback = []
            configuration.defaultWebpagePreferences.allowsContentJavaScript = true

            let webView = WKWebView(frame: view.bounds, configuration: configuration)
            webView.navigationDelegate = self
            webView.uiDelegate = self
            webView.allowsBackForwardNavigationGestures = false
            webView.isOpaque = true
            webView.backgroundColor = .black
            webView.scrollView.bounces = false
            webView.scrollView.isScrollEnabled = false
            webView.translatesAutoresizingMaskIntoConstraints = false

            self.webView?.removeFromSuperview()
            self.webView = webView
            view.addSubview(webView)
            NSLayoutConstraint.activate([
                webView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
                webView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
                webView.topAnchor.constraint(equalTo: view.topAnchor),
                webView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            ])

            // The subscription belongs to the page that made it, and this is a new
            // page. Carrying it over would have the host delivering lifecycle events
            // to a listener that no longer exists.
            lifecycleSubscribed = false
            webView.load(URLRequest(url: MigoWebKitContentOrigin.baseURL))
        }

        // MARK: - lifecycle

        private func observeLifecycleIfNeeded() {
            guard lifecycleObservers.isEmpty else { return }
            let phases: [(Notification.Name, String)] = [
                (UIApplication.willResignActiveNotification, "willResignActive"),
                (UIApplication.didBecomeActiveNotification, "didBecomeActive"),
                (UIApplication.didEnterBackgroundNotification, "didEnterBackground"),
                (UIApplication.willEnterForegroundNotification, "willEnterForeground"),
                // The last chance content gets to persist anything. The page's own
                // visibility events never fire for it.
                (UIApplication.willTerminateNotification, "willTerminate"),
            ]
            lifecycleObservers = phases.map { name, phase in
                NotificationCenter.default.addObserver(
                    forName: name, object: nil, queue: .main
                ) { [weak self] _ in
                    self?.deliverLifecycle(phase: phase)
                }
            }
        }

        private func deliverLifecycle(phase: String) {
            // Nothing is evaluated into a page that never subscribed: an
            // `evaluateJavaScript` per app transition into content that does not care
            // is main-thread work with no consumer.
            guard lifecycleSubscribed, let webView else { return }
            let payload = "{\"phase\":\(quoted(phase))}"
            webView.evaluateJavaScript(
                MigoWebKitBridgeShim.deliveryScript(
                    channel: MigoWebKitBridgeShim.channel(forMethod: "lifecycle.observe"),
                    payloadJSON: payload))
        }

        private func quoted(_ text: String) -> String {
            let data = try? JSONSerialization.data(withJSONObject: [text], options: [])
            guard let data, let array = String(data: data, encoding: .utf8) else { return "\"\"" }
            // `["x"]` -> `"x"`. Serialising through Foundation rather than escaping by
            // hand, because the phases are constants today and the next channel's
            // payload will not be.
            return String(array.dropFirst().dropLast())
        }

        // MARK: - the bridge

        private func environmentPayload() -> [String: Any] {
            var fields: [String: Any] = [
                "lane": MigoWebKitSurface.lane,
                "locale": Locale.preferredLanguages.first ?? Locale.current.identifier,
                "devicePixelRatio": view.window?.screen.scale ?? UIScreen.main.scale,
            ]
            let insets = view.safeAreaInsets
            fields["safeArea"] = [
                "top": insets.top, "bottom": insets.bottom, "left": insets.left,
                "right": insets.right,
            ]
            for (key, value) in delegate?.sessionEnvironmentFields(self) ?? [:] {
                // Host fields cannot overwrite the ones above: a host that reported a
                // different lane than the session is running would make every
                // diagnostic it collects wrong about which lane produced it.
                if fields[key] == nil { fields[key] = value }
            }
            return fields
        }

        private func admitDiagnostic() -> Int? {
            let now = Date()
            if now.timeIntervalSince(diagnosticWindowStart) >= 1 {
                diagnosticWindowStart = now
                diagnosticsInWindow = 0
            }
            guard diagnosticsInWindow < Self.diagnosticsPerSecond else {
                diagnosticsDropped += 1
                return nil
            }
            diagnosticsInWindow += 1
            let dropped = diagnosticsDropped
            diagnosticsDropped = 0
            return dropped
        }
    }

    // MARK: - WKScriptMessageHandlerWithReply

    extension MigoWebKitSession: WKScriptMessageHandlerWithReply {

        public func userContentController(
            _ userContentController: WKUserContentController,
            didReceive message: WKScriptMessage,
            replyHandler: @escaping (Any?, String?) -> Void
        ) {
            // The fence. After a rebuild the dying page can still have a message in
            // flight, and answering it would be answering a page that no longer has a
            // listener -- or worse, letting it change state the live page owns.
            guard let webView, message.webView === webView else {
                replyHandler(
                    nil,
                    "migoHost: this message came from a web view this session has replaced")
                return
            }
            guard let method = handlerMethods[message.name] else {
                // Only reachable if a handler is installed for a name the table does
                // not carry, which is a host bug rather than a content one.
                replyHandler(nil, "migoHost: \(message.name) has no method behind it")
                return
            }

            switch surface.decision(forBridgeMethod: method) {
            case .refuseUnknown(let name):
                replyHandler(nil, "migoHost: \(name) is not a method this lane declares")
            case .refuseDisabled(let capability):
                replyHandler(
                    nil,
                    "migoHost: \(capability.rawValue) is withheld by this host's configuration "
                        + "(contracts/apple/webkit-full-surface.json)")
            case .serve(let capability):
                serve(capability, message: message, replyHandler: replyHandler)
            }
        }

        private func serve(
            _ capability: MigoWebKitSurface.Capability, message: WKScriptMessage,
            replyHandler: @escaping (Any?, String?) -> Void
        ) {
            switch capability {
            case .environment:
                replyHandler(environmentPayload(), nil)
            case .lifecycle:
                lifecycleSubscribed = true
                replyHandler(["subscribed": true], nil)
            case .diagnostics:
                guard let dropped = admitDiagnostic() else {
                    replyHandler(
                        nil,
                        "migoHost: diagnostics are rate-limited to "
                            + "\(Self.diagnosticsPerSecond) per second and this one was dropped")
                    return
                }
                var diagnostic = message.body as? [String: Any] ?? ["body": message.body]
                if dropped > 0 { diagnostic["droppedSinceLast"] = dropped }
                delegate?.session(self, didReceiveDiagnostic: diagnostic)
                replyHandler(["received": true], nil)
            default:
                // A bridge capability with no implementation is a promise nobody
                // settles, so it answers rather than falling through.
                replyHandler(
                    nil,
                    "migoHost: \(capability.rawValue) is declared as a bridge method and this "
                        + "session has no implementation for it")
            }
        }
    }

    // MARK: - WKNavigationDelegate

    extension MigoWebKitSession: WKNavigationDelegate {

        public func webView(
            _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = navigationAction.request.url else {
                decisionHandler(.cancel)
                return
            }
            // Content stays in its own origin. A mini game that navigates to a remote
            // page is a review problem before it is a security one, and the host that
            // wants a link opened can open it itself.
            if url.scheme == MigoWebKitContentOrigin.scheme || url.absoluteString == "about:blank" {
                decisionHandler(.allow)
                return
            }
            decisionHandler(.cancel)
            delegate?.session(self, refusedNavigationTo: url)
        }

        public func webView(
            _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            delegate?.session(
                self,
                didReceiveDiagnostic: [
                    "source": "host", "kind": "navigationFailed",
                    "message": error.localizedDescription,
                ])
        }

        public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            switch recovery.webContentTerminated() {
            case .rebuild:
                delegate?.session(self, willRebuildAfterTermination: recovery.consecutiveTerminations)
                build()
            case .stop(let count):
                // No rebuild, and the view goes with it: leaving a dead web view on
                // screen shows content frozen at the frame it died on, which reads as
                // a hang rather than as a failure.
                self.webView?.removeFromSuperview()
                self.webView = nil
                delegate?.session(self, didFailTerminallyAfter: count)
            }
        }
    }

    // MARK: - WKUIDelegate

    extension MigoWebKitSession: WKUIDelegate {

        public func webView(
            _ webView: WKWebView,
            requestMediaCapturePermissionFor origin: WKSecurityOrigin,
            initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType,
            decisionHandler: @escaping (WKPermissionDecision) -> Void
        ) {
            let request: MigoWebKitSurface.HostGrantedRequest
            switch type {
            case .camera: request = .camera
            case .microphone: request = .microphone
            case .cameraAndMicrophone: request = .cameraAndMicrophone
            @unknown default:
                // A capture type this build does not know about is refused. Prompting
                // for it would ask the user to grant something the compliance surface
                // has never described.
                decisionHandler(.deny)
                return
            }
            // `.deny`, never `.prompt`, when the surface says no: a prompt the user can
            // accept would hand over a capability the contract says is off.
            decisionHandler(surface.grants(request) ? .prompt : .deny)
        }

        public func webView(
            _ webView: WKWebView,
            requestDeviceOrientationAndMotionPermissionFor origin: WKSecurityOrigin,
            initiatedByFrame frame: WKFrameInfo,
            decisionHandler: @escaping (WKPermissionDecision) -> Void
        ) {
            decisionHandler(surface.grants(.motionSensors) ? .prompt : .deny)
        }
    }
#endif
