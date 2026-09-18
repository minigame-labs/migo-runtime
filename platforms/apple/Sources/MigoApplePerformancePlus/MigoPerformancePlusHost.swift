import Foundation
import MigoAppleCore
import MigoAppleWebKit
import WebKit
import os

#if os(iOS)
    import UIKit

    /// The host half of the Performance+ lane: a web view whose only job is to run
    /// the producer, and a frame channel the producer connects back to.
    ///
    /// It owns three things and composes the rest. The two it owns outright are the
    /// `WKWebView` and the configuration handed to the page; the third is the
    /// decision of *where the producer's code comes from*, which is the reserved
    /// `__migo/` prefix on the content origin (`MigoWebKitOriginRules`). The frame
    /// channel is passed in rather than built here, because a channel is bound to an
    /// engine session and this type must be constructible against a closure in a
    /// test that stands up no renderer -- the same arrangement `MigoFrameChannel` and
    /// `MigoSessionFrameClock` already use, for the same reason.
    ///
    /// ## Why the producer is served from the same origin as the content
    ///
    /// A module worker's script and every module it imports are fetched from the
    /// origin the page was loaded from; there is no second origin available to put
    /// engine code on. So one origin serves two roots -- the game's package at `/`
    /// and the engine's modules at `/__migo/` -- and the prefix is reserved in the
    /// rules rather than by convention, so a game that ships a `__migo` directory
    /// cannot replace the code that owns the frame channel.
    ///
    /// ## Why the configuration is injected rather than served
    ///
    /// The socket's port is ephemeral. It exists only after the channel is
    /// listening, which is after the producer was packaged and before this page is
    /// loaded, so it cannot be a file and must not be guessed: a producer that
    /// guessed the port would be a producer that connects to whatever else is
    /// listening.
    ///
    /// ## iOS only, and stated rather than implied
    ///
    /// The shipping lane is iOS. A macOS arm here would be a second arm no lane
    /// compiles, which this repository has paid for twice -- see
    /// `MigoWebKitSession`, which is iOS-only for the same reason and records it.
    public final class MigoPerformancePlusHost: NSObject {

        /// What this host's origin did with the frames posted to it.
        public var originActivity: MigoPerformancePlusOrigin.Activity { origin.activity }

        /// What the producer said. The dictionary is content-shaped JSON: `type` is
        /// always present and is one of `connected`, `engine-ready`, `ready`,
        /// `verdict`, `generation-lost`, `failed`.
        ///
        /// The engine's console is not a report. Its lines go to the platform log
        /// under the `dev.migo` subsystem, at the level the engine gave them, which
        /// is where the engine's own log goes on every platform that runs it in
        /// process -- an app that wants them reads the log, and an app that does
        /// not is not handed a main-thread callback per line.
        public typealias Report = [String: Any]

        /// Where content's console lines go.
        private static let contentLog = Logger(subsystem: "dev.migo", category: "content")

        /// The session the engine's own JavaScript API layer answers for.
        ///
        /// Given, the producer loads the engine's WebGL, Canvas2D and `migo.*`
        /// layer before content, as every other Migo platform evaluates it before
        /// a game's first line, and content draws through `migo.createCanvas()`.
        /// Absent, content talks to the frame channel directly, which is what a
        /// lane bring-up test wants and what a product build never does.
        ///
        /// The values are the ones the host gave the engine: the launch nonce from
        /// `MigoSessionConfig`, the generation of the attached surface and its
        /// size in pixels. The producer stamps them on every packet, and ingress
        /// refuses a packet whose identity is not this session's.
        public struct EngineSession: Equatable, Sendable {
            /// The 16 bytes of `MigoSessionConfig.launch_nonce`, in that order.
            public var launchNonce: [UInt8]
            public var runtimeGeneration: UInt64
            public var surfaceGeneration: UInt64
            public var resourceEpoch: UInt64
            public var surfaceWidthPixels: Int
            public var surfaceHeightPixels: Int
            /// What `migo.getWindowInfo()` and `getSystemInfoSync()` answer
            /// with; see `MigoPerformancePlusHost.DeviceProfile`. `nil` leaves
            /// those calls failing as they do on a platform with no device
            /// services.
            public var device: DeviceProfile?

            public init(
                launchNonce: [UInt8], runtimeGeneration: UInt64 = 1, surfaceGeneration: UInt64,
                resourceEpoch: UInt64 = 0, surfaceWidthPixels: Int, surfaceHeightPixels: Int,
                device: DeviceProfile? = nil
            ) {
                self.launchNonce = launchNonce
                self.runtimeGeneration = runtimeGeneration
                self.surfaceGeneration = surfaceGeneration
                self.resourceEpoch = resourceEpoch
                self.surfaceWidthPixels = surfaceWidthPixels
                self.surfaceHeightPixels = surfaceHeightPixels
                self.device = device
            }

            /// What the page is handed. The 64- and 128-bit fields are strings,
            /// because a JSON number is a double and these are identities: a
            /// nonce rounded to 53 bits is a foreign session.
            var injected: [String: Any] {
                // The nonce is little-endian bytes; the string is the number.
                let hex = launchNonce.reversed().map { String(format: "%02x", $0) }.joined()
                var fields: [String: Any] = [
                    "launchNonce": "0x" + hex,
                    "runtimeGeneration": String(runtimeGeneration),
                    "surfaceGeneration": String(surfaceGeneration),
                    "resourceEpoch": String(resourceEpoch),
                    "surfaceWidth": surfaceWidthPixels,
                    "surfaceHeight": surfaceHeightPixels,
                ]
                if let device { fields["device"] = device.injected }
                return fields
            }

            /// Why this description cannot be handed to a producer, or nil.
            var problem: String? {
                if launchNonce.count != 16 { return "the launch nonce is \(launchNonce.count) bytes, not 16" }
                if launchNonce.allSatisfy({ $0 == 0 }) { return "the launch nonce is all zero" }
                if !(1...0xffff).contains(surfaceWidthPixels) || !(1...0xffff).contains(surfaceHeightPixels) {
                    return "the surface size \(surfaceWidthPixels)x\(surfaceHeightPixels) is not a pixel size"
                }
                return nil
            }
        }

        /// What the host knows about the device, for the `migo.*` calls a game
        /// makes before its first frame.
        ///
        /// `migo.getWindowInfo()` and `getSystemInfoSync()` are synchronous and
        /// a game reads them to lay itself out. None of it changes during a
        /// session, so it is handed to the producer at startup and answered
        /// there without crossing -- which is what the op boundary calls the
        /// `local` lane.
        ///
        /// Absent, those calls fail with the message the engine raises on a
        /// platform with no device services. That is deliberate: a screen size
        /// nobody measured is worse than a failure, because a game lays itself
        /// out to it.
        public struct DeviceProfile: Equatable, Sendable {
            /// Points, as the mini-game API reports them: the physical pixels
            /// divided by `pixelRatio`.
            public var screenWidth: Double
            public var screenHeight: Double
            /// The drawable area, which on a phone is the screen minus nothing
            /// and in a split window is less.
            public var windowWidth: Double
            public var windowHeight: Double
            public var pixelRatio: Double
            public var statusBarHeight: Double
            /// Insets from each edge, which is the shape the engine converts
            /// into absolute positions.
            public var safeAreaInsets: SafeAreaInsets
            public var brand: String
            public var model: String
            public var system: String
            /// `"ios"` or `"macos"`; the engine passes it through to content.
            public var platform: String

            public struct SafeAreaInsets: Equatable, Sendable {
                public var left: Double
                public var top: Double
                public var right: Double
                public var bottom: Double

                public init(left: Double = 0, top: Double = 0, right: Double = 0, bottom: Double = 0) {
                    self.left = left
                    self.top = top
                    self.right = right
                    self.bottom = bottom
                }
            }

            public init(
                screenWidth: Double, screenHeight: Double, windowWidth: Double,
                windowHeight: Double, pixelRatio: Double, statusBarHeight: Double = 0,
                safeAreaInsets: SafeAreaInsets = SafeAreaInsets(), brand: String = "Apple",
                model: String = "unknown", system: String = "", platform: String = "ios"
            ) {
                self.screenWidth = screenWidth
                self.screenHeight = screenHeight
                self.windowWidth = windowWidth
                self.windowHeight = windowHeight
                self.pixelRatio = pixelRatio
                self.statusBarHeight = statusBarHeight
                self.safeAreaInsets = safeAreaInsets
                self.brand = brand
                self.model = model
                self.system = system
                self.platform = platform
            }

            /// The JSON each op answers with, as the engine's JavaScript parses
            /// it: snake_case for the window, camelCase for the device, because
            /// that is what `system/03_window_info.js` and `10_system_info.js`
            /// read. One shape, not a translation on either side.
            var injected: [String: Any] {
                let window: [String: Any] = [
                    "pixel_ratio": pixelRatio,
                    "screen_width": screenWidth,
                    "screen_height": screenHeight,
                    "window_width": windowWidth,
                    "window_height": windowHeight,
                    "status_bar_height": statusBarHeight,
                    "screen_top": 0,
                    "safe_area": [
                        "left": safeAreaInsets.left,
                        "top": safeAreaInsets.top,
                        "right": safeAreaInsets.right,
                        "bottom": safeAreaInsets.bottom,
                    ],
                ]
                let device: [String: Any] = [
                    "brand": brand,
                    "model": model,
                    "system": system,
                    "platform": platform,
                    // -1 is what the API reports for "not benchmarked", which is
                    // the truth here: nothing has measured this device.
                    "benchmarkLevel": -1,
                ]
                return [
                    "windowInfo": Self.json(window),
                    "deviceInfo": Self.json(device),
                ]
            }

            private static func json(_ value: [String: Any]) -> String {
                guard let data = try? JSONSerialization.data(withJSONObject: value),
                    let text = String(data: data, encoding: .utf8)
                else {
                    // Unreachable for the value types above; an empty object is
                    // still valid JSON, so content sees defaults rather than a
                    // parse error it cannot act on.
                    return "{}"
                }
                return text
            }
        }

        /// Everything the page needs that is not in its own bundle.
        public struct Configuration {
            /// Where the game's package is. Served at `/`. For a game, the
            /// directory `migo_session_copy_content_root` answers after
            /// `migo_session_load_content`: the package the engine mounted.
            public var contentRoot: URL
            /// The game's entry file in its package -- `"/game.js"` -- evaluated
            /// after the engine's API layer the way the embedded runtime
            /// evaluates one: as a module whose source, and every script it
            /// imports, the engine serves with its own module rules, so a
            /// CommonJS entry runs wrapped and `require` resolves inside the
            /// package. Needs `engineSession`, and content loaded into the
            /// engine's session.
            public var gameEntry: String?
            /// A harness module, imported after the engine if there is one, whose
            /// `start({ session, sync, report })` is called with the producer's
            /// own session: how a lane test drives frames and calls below the
            /// engine's API. Served as its file. Not a game entry -- a product
            /// build names `gameEntry`.
            public var harnessEntry: String?
            /// Where the engine's own modules are. Defaults to the ones this package
            /// ships.
            public var engineRoot: URL?
            /// Relay every frame verdict to the host.
            ///
            /// Off by default and it is not a courtesy: one verdict per submitted
            /// frame, each a structured clone to the page and a hop onto the main
            /// thread, is 120 crossings a second on the latency path this lane
            /// exists to shorten -- to deliver a credit level the producer has
            /// already applied.
            public var reportVerdicts: Bool
            /// The session the engine's API layer answers for; see `EngineSession`.
            public var engineSession: EngineSession?

            public init(
                contentRoot: URL, gameEntry: String? = nil, harnessEntry: String? = nil,
                engineRoot: URL? = nil, reportVerdicts: Bool = false,
                engineSession: EngineSession? = nil
            ) {
                self.contentRoot = contentRoot
                self.gameEntry = gameEntry
                self.harnessEntry = harnessEntry
                self.engineRoot = engineRoot
                self.reportVerdicts = reportVerdicts
                self.engineSession = engineSession
            }
        }

        /// Why a start did not happen.
        public enum StartFailure: Error, CustomStringConvertible {
            /// The engine modules are not where they should be. Named rather than
            /// discovered at load time, because the symptom otherwise is a page that
            /// loads and a worker that never connects -- which reads like a
            /// transport fault and is a packaging one.
            case engineModulesMissing(URL)
            /// The frame channel could not start: it could not listen, or the
            /// engine would not install its downlink waker.
            case transport(Error)
            /// The engine session cannot be handed to a producer; the reason is
            /// named. Refused here because a producer given it would have every
            /// packet refused by ingress, which reads as a black screen.
            case invalidEngineSession(String)
            /// A game entry was named without the engine session its API layer
            /// answers for: the game's first call would have nothing to call.
            case gameWithoutEngine

            public var description: String {
                switch self {
                case .engineModulesMissing(let url):
                    return
                        "the producer's modules are not at \(url.path). They are generated, not"
                        + " committed: scripts/build-apple-sdk.sh copies"
                        + " platforms/apple/WebContent/PerformancePlus/src into the package's"
                        + " resources, and a build that skipped it produces exactly this."
                case .transport(let error):
                    return "the frame channel could not start: \(error)"
                case .invalidEngineSession(let reason):
                    return "the engine session cannot be used: \(reason)"
                case .gameWithoutEngine:
                    return
                        "a game entry was named without an engine session; the game runs on the"
                        + " engine's API layer, which answers for one"
                }
            }
        }

        /// The engine modules this package ships.
        ///
        /// `nil` when the resource bundle carries none, which is a build that did not
        /// run the packaging step rather than a state the product can be in.
        ///
        /// Derived from where the producer page actually is rather than from a path
        /// built out of the target's resource declaration, because those two are not
        /// the same thing and the difference is silent. `resources: [.copy("Resources")]`
        /// names a directory in the SOURCE tree; where its contents land in the built
        /// bundle is the bundle format's business, and on iOS a bundle's resource root
        /// is itself called `Resources`, so the obvious
        /// `resourceURL.appendingPathComponent("Resources")` asks for `Resources/Resources`
        /// and finds nothing. Measured on the simulator, where it threw
        /// `engineModulesMissing` with exactly that doubled path.
        ///
        /// Both lookups are tried because the two bundle layouts differ and this type
        /// must not care which one it is in.
        public static var bundledEngineRoot: URL? {
            let page = Bundle.module.url(forResource: "producer-page", withExtension: "html")
                ?? Bundle.module.url(
                    forResource: "producer-page", withExtension: "html", subdirectory: "Resources")
            return page?.deletingLastPathComponent()
        }

        /// The document the lane loads. Under the reserved prefix, so it is the
        /// engine's page and not a file a content package could provide.
        public static var producerPageURL: URL {
            MigoWebKitContentOrigin.baseURL
                .appendingPathComponent(
                    String(MigoWebKitOriginRules.engineAssetPrefix.dropFirst())
                        + "producer-page.html")
        }

        /// The name of the one script message handler this lane installs.
        ///
        /// One, and it carries no arguments the host acts on -- the page reports and
        /// the host listens. A bridge in the other direction would be a native
        /// surface reachable from the agent content runs in, which is the thing this
        /// lane is arranged to avoid.
        static let reportHandlerName = "migoPerformancePlus"

        /// The container the app installs. Attached and off-screen: an unattached
        /// web view is killed since iOS 16, and an occluded one stops executing
        /// JavaScript.
        public let view: UIView

        /// The channel the producer connects to.
        public let channel: MigoFrameChannel

        public private(set) var webView: WKWebView?

        /// Called on the main queue for every report the producer makes.
        public var onReport: ((Report) -> Void)?

        private let configuration: Configuration
        private let origin: MigoPerformancePlusOrigin
        private let engineRoot: URL

        public init(
            configuration: Configuration, channel: MigoFrameChannel,
            onReport: ((Report) -> Void)? = nil
        ) throws {
            guard
                let engineRoot = configuration.engineRoot
                    ?? MigoPerformancePlusHost.bundledEngineRoot
            else {
                throw StartFailure.engineModulesMissing(
                    Bundle.module.resourceURL ?? Bundle.module.bundleURL)
            }
            // Checked here rather than at load time: a missing module presents as a
            // worker that never connects, which is indistinguishable from a
            // transport fault and sends whoever reads it to the wrong half.
            guard
                FileManager.default.fileExists(
                    atPath: engineRoot.appendingPathComponent("producer-page.html").path)
            else {
                throw StartFailure.engineModulesMissing(engineRoot)
            }
            if configuration.gameEntry != nil && configuration.engineSession == nil {
                throw StartFailure.gameWithoutEngine
            }
            if let session = configuration.engineSession {
                if let problem = session.problem { throw StartFailure.invalidEngineSession(problem) }
                // The engine's own modules are generated into the bundle by the SDK
                // build; a producer told to load them from a bundle without them
                // fails in WebContent, where nobody sees why.
                guard
                    FileManager.default.fileExists(
                        atPath: engineRoot.appendingPathComponent("engine/boot.mjs").path)
                else {
                    throw StartFailure.engineModulesMissing(engineRoot.appendingPathComponent("engine"))
                }
            }
            self.configuration = configuration
            self.channel = channel
            self.engineRoot = engineRoot
            // The frame endpoint in front, content serving behind: one handler per
            // scheme is all a `WKWebViewConfiguration` accepts, and the producer's
            // large frames have to arrive on the origin the page was loaded from.
            // A game's scripts are the engine's to serve: it resolves them
            // through the package it mounted and evaluates them with its module
            // rules. Weak for the reason `deliver` is.
            let moduleSource: MigoWebKitContentOrigin.ModuleSource? =
                configuration.gameEntry == nil
                ? nil
                : { [weak channel] path in
                    channel?.contentModule(path: path)
                        ?? .unavailable("the frame channel has gone")
                }
            self.origin = MigoPerformancePlusOrigin(
                content: MigoWebKitContentOrigin(
                    root: configuration.contentRoot, engineRoot: engineRoot,
                    moduleSource: moduleSource),
                deliver: { [weak channel] packet in channel?.submitFromOrigin(packet) },
                // Weak for the reason `deliver` is, and `nil` once the channel is
                // gone: the origin answers that as "no answer", which the
                // producer reports rather than reading as a verdict.
                answer: { [weak channel] call in channel?.answerSyncCall(call) },
                // A service message too large for the socket, and an answer too
                // large for it: the same channel, reached through the origin.
                submitService: { [weak channel] message in
                    channel?.submitServiceFromOrigin(message) ?? .unavailable
                },
                takeParked: { [weak channel] generation, requestId in
                    channel?.takeParkedReply(generation: generation, requestId: requestId)
                })
            self.onReport = onReport
            self.view = UIView(frame: .zero)
            super.init()
            self.view.backgroundColor = .black
            self.view.isUserInteractionEnabled = false
        }

        deinit {
            // The handler holds this object; leaving it installed on a controller
            // that outlives the host is a retain cycle that survives the page.
            webView?.configuration.userContentController
                .removeScriptMessageHandler(forName: Self.reportHandlerName)
        }

        /// Listen, then load. In that order, because the page is handed the port.
        @discardableResult
        public func start() throws -> MigoFrameTransport.Endpoint {
            let endpoint: MigoFrameTransport.Endpoint
            do {
                endpoint = try channel.start()
            } catch {
                throw StartFailure.transport(error)
            }
            build(endpoint: endpoint)
            return endpoint
        }

        public func stop() {
            channel.stop()
            webView?.stopLoading()
            webView?.configuration.userContentController
                .removeScriptMessageHandler(forName: Self.reportHandlerName)
            webView?.removeFromSuperview()
            webView = nil
        }

        private func build(endpoint: MigoFrameTransport.Endpoint) {
            let webConfiguration = WKWebViewConfiguration()
            webConfiguration.setURLSchemeHandler(
                origin, forURLScheme: MigoWebKitContentOrigin.scheme)

            let controller = WKUserContentController()
            controller.addUserScript(
                WKUserScript(
                    source: configurationScript(endpoint: endpoint),
                    injectionTime: .atDocumentStart, forMainFrameOnly: true))
            controller.add(self, name: Self.reportHandlerName)
            webConfiguration.userContentController = controller
            webConfiguration.defaultWebpagePreferences.allowsContentJavaScript = true

            let webView = WKWebView(frame: view.bounds, configuration: webConfiguration)
            webView.navigationDelegate = self
            webView.isOpaque = true
            webView.backgroundColor = .black
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
            webView.load(URLRequest(url: Self.producerPageURL))
        }

        /// The one global the page reads, serialised rather than escaped by hand.
        private func configurationScript(endpoint: MigoFrameTransport.Endpoint) -> String {
            var fields: [String: Any] = [
                "frameChannelUrl": endpoint.url.absoluteString,
                "frameSchemeUrl": MigoPerformancePlusOrigin.frameURL.absoluteString,
                // Where a synchronous call goes. Same origin as the page, so a
                // Worker's blocking request needs no CORS and no second listener.
                "syncCallUrl": MigoPerformancePlusOrigin.syncURL.absoluteString,
                // The service stream's two scheme endpoints: a message too large
                // for the socket goes to the first, an answer too large for it
                // is taken from the second. Same origin, for the reason the
                // sync endpoint is.
                "serviceUrl": MigoPerformancePlusOrigin.serviceURL.absoluteString,
                "replyUrl": MigoPerformancePlusOrigin.replyURL.absoluteString,
                // From `MigoFrameChannelPolicy`, so the producer has no copy of a
                // measured number. A constant in both languages would drift the
                // first time the measurement is redone on new hardware --
                // silently, because each half would still be self-consistent.
                "socketCeilingBytes": MigoFrameChannelPolicy.socketCeilingBytes,
                "reportVerdicts": configuration.reportVerdicts,
            ]
            if let entry = configuration.gameEntry { fields["gameEntry"] = entry }
            if let entry = configuration.harnessEntry { fields["harnessEntry"] = entry }
            if let session = configuration.engineSession { fields["engineSession"] = session.injected }
            guard let data = try? JSONSerialization.data(withJSONObject: fields),
                let json = String(data: data, encoding: .utf8)
            else {
                // Unreachable with the value types above, and a crash here would be
                // a crash in an app rather than in a test, so the page is told
                // instead and reports it the way it reports every other bad config.
                return "globalThis.__migoPerformancePlusConfig = null;"
            }
            return "globalThis.__migoPerformancePlusConfig = \(json);"
        }
    }

    // MARK: - what the producer says

    extension MigoPerformancePlusHost: WKScriptMessageHandler {

        public func userContentController(
            _ userContentController: WKUserContentController, didReceive message: WKScriptMessage
        ) {
            // The fence. A page being torn down can still have a message in flight,
            // and a report from a page that is no longer the producer would be
            // attributed to the one that is.
            guard let webView, message.webView === webView else { return }
            guard let body = message.body as? Report else { return }
            if body["type"] as? String == "console" {
                Self.log(consoleLine: body)
                return
            }
            onReport?(body)
        }

        /// The engine's `op_console` levels, as the embedded op maps them: 1 info,
        /// 2 warn, 3 error, anything else debug. The message keeps the platform
        /// log's default privacy -- content's console can carry a player's data,
        /// and the log redacts dynamic strings unless a debugger is attached.
        private static func log(consoleLine body: Report) {
            let message = body["message"] as? String ?? ""
            switch body["level"] as? Int {
            case 1: contentLog.info("\(message)")
            case 2: contentLog.warning("\(message)")
            case 3: contentLog.error("\(message)")
            default: contentLog.debug("\(message)")
            }
        }
    }

    // MARK: - navigation

    extension MigoPerformancePlusHost: WKNavigationDelegate {

        /// Nothing navigates. The producer page is the only document this lane ever
        /// loads, and content has no way to ask for another -- it runs in a worker,
        /// which has no navigation. Refusing by policy rather than relying on that
        /// is what makes it true of the *page* as well.
        public func webView(
            _ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            // Compared by scheme, host and path rather than by whole URL: WebKit
            // hands back a URL it has normalised, and an equality that a
            // normalisation can break is a lane that refuses its own page.
            let url = navigationAction.request.url
            let allowed =
                url?.scheme == MigoWebKitContentOrigin.scheme
                && url?.host == MigoWebKitContentOrigin.host
                && url?.path == Self.producerPageURL.path
            decisionHandler(allowed ? .allow : .cancel)
        }

        public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            // Named, not handled. Recovery retires the generation, drops
            // unacknowledged packets and resumes from a checkpoint, and doing any
            // of that before the engine can retire a generation would leave the
            // rebuilt producer crediting against a runtime that is gone.
            onReport?(["type": "content-process-terminated"])
        }
    }
#endif
