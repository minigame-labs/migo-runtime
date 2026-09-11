import Foundation
import MigoAppleCore
import MigoAppleWebKit
import WebKit

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

        /// What the producer said. The dictionary is content-shaped JSON: `type` is
        /// always present and is one of `connected`, `ready`, `verdict`,
        /// `generation-lost`, `failed`.
        public typealias Report = [String: Any]

        /// Everything the page needs that is not in its own bundle.
        public struct Configuration {
            /// Where the game's package is. Served at `/`.
            public var contentRoot: URL
            /// The content module the producer imports, as a path on this origin --
            /// `"/game/main.mjs"`. `nil` runs the producer with no content, which is
            /// what a lane bring-up test wants and what a product build never does.
            public var contentEntry: String?
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

            public init(
                contentRoot: URL, contentEntry: String? = nil, engineRoot: URL? = nil,
                reportVerdicts: Bool = false
            ) {
                self.contentRoot = contentRoot
                self.contentEntry = contentEntry
                self.engineRoot = engineRoot
                self.reportVerdicts = reportVerdicts
            }
        }

        /// Why a start did not happen.
        public enum StartFailure: Error, CustomStringConvertible {
            /// The engine modules are not where they should be. Named rather than
            /// discovered at load time, because the symptom otherwise is a page that
            /// loads and a worker that never connects -- which reads like a
            /// transport fault and is a packaging one.
            case engineModulesMissing(URL)
            /// The frame channel could not listen.
            case transport(Error)

            public var description: String {
                switch self {
                case .engineModulesMissing(let url):
                    return
                        "the producer's modules are not at \(url.path). They are generated, not"
                        + " committed: scripts/build-apple-sdk.sh copies"
                        + " platforms/apple/WebContent/PerformancePlus/src into the package's"
                        + " resources, and a build that skipped it produces exactly this."
                case .transport(let error):
                    return "the frame channel could not listen: \(error)"
                }
            }
        }

        /// The engine modules this package ships.
        ///
        /// `nil` when the resource bundle carries none, which is a build that did not
        /// run the packaging step rather than a state the product can be in.
        public static var bundledEngineRoot: URL? {
            Bundle.module.resourceURL?.appendingPathComponent("Resources")
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
        private let origin: MigoWebKitContentOrigin
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
                    URL(fileURLWithPath: "<this package has no resource bundle>"))
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
            self.configuration = configuration
            self.channel = channel
            self.engineRoot = engineRoot
            self.origin = MigoWebKitContentOrigin(
                root: configuration.contentRoot, engineRoot: engineRoot)
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
                "reportVerdicts": configuration.reportVerdicts,
            ]
            if let entry = configuration.contentEntry { fields["contentEntry"] = entry }
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
            onReport?(body)
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
