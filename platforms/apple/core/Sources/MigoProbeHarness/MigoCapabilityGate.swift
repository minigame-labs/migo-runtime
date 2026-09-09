import Foundation
import MigoProbeCore
import WebKit

#if canImport(UIKit)
    import UIKit
#endif

/// Measurement gate 1, the capability gate.
///
/// It runs the probe page at both candidate origins, one after the other, and
/// produces one record per origin. Half the questions are answered by the
/// origin rather than by the device -- secure context, cross-origin isolation,
/// and therefore whether `SharedArrayBuffer` constructs at all -- so a single
/// record per device would have to hold two answers under one key.
///
/// WHAT THIS DELIBERATELY DOES NOT DO. It carries no renderer and no validator.
/// The measurement order in the plan puts this gate first precisely because a
/// candidate the device cannot host has to leave the matrix before it is
/// benchmarked: a benchmark of an unavailable arm is a benchmark of whatever
/// ran instead. Adding a renderer here to "get more out of one run" is how that
/// ordering gets lost.
///
/// The web view must be in a view hierarchy that is on screen. A4 records that
/// an unmounted or hidden WKWebView is killed or throttled, and a gate run
/// against a throttled WebContent measures the throttling.
public final class MigoCapabilityGate: NSObject {

    /// What the person holding the device declares, because no API reports it.
    public struct Attestation: Sendable {
        /// A24. iOS exposes no public query for Lockdown Mode. `unknown` is the
        /// honest default and is not the same answer as `off`.
        public var lockdownMode: MigoLockdownMode
        /// Whether a local-network permission alert was presented during the
        /// run. `nil` means nobody was watching, which is recorded as such: a
        /// run that flowed traffic after the operator tapped Allow and a run
        /// that was never prompted are the same to this process and different
        /// to App Review.
        public var localNetworkPromptObserved: Bool?
        public var runId: String

        public init(
            lockdownMode: MigoLockdownMode = .unknown,
            localNetworkPromptObserved: Bool? = nil,
            runId: String = UUID().uuidString
        ) {
            self.lockdownMode = lockdownMode
            self.localNetworkPromptObserved = localNetworkPromptObserved
            self.runId = runId
        }
    }

    public enum GateError: Error, CustomStringConvertible {
        case resourcesMissing(String)
        case pageFailed(String)
        case noReport(String)
        case timedOut(String)

        public var description: String {
            switch self {
            case .resourcesMissing(let name): return "the probe resource \(name) is not in the bundle"
            case .pageFailed(let reason): return "the probe page failed to load: \(reason)"
            case .noReport(let reason): return "the probe page produced no report: \(reason)"
            case .timedOut(let reason): return "the gate gave up waiting: \(reason)"
            }
        }
    }

    /// How long one origin may take before the run is reported as a hang.
    ///
    /// The page's own probes are bounded -- thirty seconds for the Worker, ten
    /// for the socket -- so anything past this is the page not running at all,
    /// which is a different failure and has to be reported as one. Without it a
    /// page that never loads leaves the gate waiting forever and an operator
    /// watching a label that says "running...", which is the least actionable
    /// thing a lab tool can do.
    public static let originTimeout: TimeInterval = 120

    /// The attestation belongs to a RUN, not to the gate.
    ///
    /// It was an initialiser parameter, which forced a caller who wanted fresh
    /// attestations to build a fresh gate -- and a fresh gate does not own the
    /// web view the first one made, so its message handlers were never
    /// registered and its `pending` was never called. The page ran, answered
    /// nine capabilities, displayed them, and the run timed out at 120 s
    /// reporting that the page had said nothing. Making the lifetime match the
    /// thing's actual scope removes the whole class rather than the instance.
    private var attestation: Attestation
    private let listener: MigoLoopbackListener
    private let schemeHandler: MigoProbeSchemeHandler
    private var pending: ((Result<[String: Any], Error>) -> Void)?
    private var webView: WKWebView?
    private var watchdog: DispatchWorkItem?
    /// Everything the page logged, kept so a failure can say what the page said
    /// rather than only that it said nothing.
    private var consoleLines: [String] = []

    /// Takes the bundle rather than nothing.
    ///
    /// A bare `init() throws` cannot exist on an `NSObject` subclass: it
    /// collides with the inherited non-throwing `init()`. The parameter is not
    /// a workaround for that -- it is the honest signature, because the
    /// resources are an input and a test that wants to run this against a
    /// different set of probe scripts should not have to rebuild the package.
    /// `Bundle?` and not `Bundle = .module`: SwiftPM's generated `Bundle.module`
    /// is internal to the target, so it cannot appear in a public default
    /// argument. `nil` means the target's own bundle.
    public init(bundle: Bundle? = nil) throws {
        self.attestation = Attestation()
        let resources = try Self.loadResources(from: bundle ?? .module)
        self.listener = MigoLoopbackListener(resources: resources)
        self.schemeHandler = MigoProbeSchemeHandler(resources: resources)
        super.init()
    }

    // MARK: - resources

    static func loadResources(from bundle: Bundle = .module) throws
        -> [String: (mime: String, body: Data)]
    {
        let files: [(String, String)] = [
            ("capability-probe.html", "text/html; charset=utf-8"),
            ("capability-probe.js", "text/javascript; charset=utf-8"),
            ("capability-probe-worker.js", "text/javascript; charset=utf-8"),
        ]
        var loaded: [String: (mime: String, body: Data)] = [:]
        for (name, mime) in files {
            let stem = (name as NSString).deletingPathExtension
            let suffix = (name as NSString).pathExtension
            guard
                let url = bundle.url(forResource: stem, withExtension: suffix),
                let body = try? Data(contentsOf: url)
            else {
                throw GateError.resourcesMissing(name)
            }
            loaded[name] = (mime, body)
        }
        return loaded
    }

    // MARK: - the web view

    /// The configured web view. The caller owns mounting it; see A4.
    public func makeWebView() -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.setURLSchemeHandler(schemeHandler, forURLScheme: MigoProbeSchemeHandler.scheme)
        // Non-persistent on purpose: a second run must not read a first run's
        // service worker, cache or storage, or an answer becomes a fact about
        // the previous run.
        configuration.websiteDataStore = .nonPersistent()
        configuration.userContentController.add(self, name: "migoProbe")
        configuration.userContentController.add(self, name: "migoConsole")

        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = self
        webView = view
        return view
    }

    // MARK: - the run

    /// Runs both origins and hands back one record each.
    ///
    /// Sequential rather than concurrent. Two pages measuring JIT warmup at the
    /// same time on the same device measure each other.
    public func run(
        in webView: WKWebView,
        attestation: Attestation = Attestation(),
        completion: @escaping (Result<[MigoCapabilityRecord], Error>) -> Void
    ) {
        // Reported rather than a `precondition`. It is a programming error, and
        // the idiomatic answer to one of those is a trap -- but this runs on a
        // bench at the far end of a lab session, and an app that vanishes tells
        // the operator less than a line of text does. The message is the same
        // either way; only the person reading it changes.
        guard webView === self.webView else {
            completion(
                .failure(
                    GateError.pageFailed(
                        "the gate must run in the web view it configured: the message handlers "
                            + "and the scheme handler are registered on that view's "
                            + "configuration, so a run in any other one waits for a report that "
                            + "reaches a different object")))
            return
        }
        self.attestation = attestation
        do {
            try listener.start()
        } catch {
            // A gate that cannot bring up its listener still has answers to
            // give at the custom-scheme origin, and reporting nothing would
            // lose them. The loopback arm records why it could not run.
            listener.stop()
        }

        let loopbackOrigin = listener.origin
        var records: [MigoCapabilityRecord] = []

        func finish(_ result: Result<[MigoCapabilityRecord], Error>) {
            listener.stop()
            completion(result)
        }

        let runScheme: () -> Void = { [weak self] in
            guard let self else { return }
            self.load(
                webView, origin: .customScheme,
                pageURL: URL(string: MigoProbeSchemeHandler.origin + "/")!,
                loopbackOrigin: loopbackOrigin
            ) { result in
                switch result {
                case .failure(let error):
                    finish(.failure(error))
                case .success(let record):
                    records.append(record)
                    finish(.success(records))
                }
            }
        }

        guard let loopbackOrigin, let loopbackURL = URL(string: loopbackOrigin + "/") else {
            runScheme()
            return
        }
        load(
            webView, origin: .loopback, pageURL: loopbackURL, loopbackOrigin: loopbackOrigin
        ) { result in
            switch result {
            case .failure(let error):
                finish(.failure(error))
            case .success(let record):
                records.append(record)
                runScheme()
            }
        }
    }

    private func load(
        _ webView: WKWebView,
        origin: MigoProbeOrigin,
        pageURL: URL,
        loopbackOrigin: String?,
        completion: @escaping (Result<MigoCapabilityRecord, Error>) -> Void
    ) {
        // The page cannot guess which port the listener took, so the config is
        // injected before any document script runs rather than substituted into
        // the HTML: the same bytes are served at both origins, and a page that
        // differed between them would be a second variable in a gate whose
        // whole purpose is to vary one.
        // `NSNull()` and not `loopbackOrigin as Any`. A nil String boxed as Any
        // is an `Optional`, which JSONSerialization refuses -- so on the one
        // path where the listener failed to come up, the whole config would
        // have failed to serialise and the page would have received `{}`. The
        // arm that was supposed to degrade to "no loopback listener was
        // supplied" would instead have lost its Worker URL as well, and the
        // custom-scheme origin would have answered nothing for a reason that
        // has nothing to do with the custom scheme.
        let config: [String: Any] = [
            "workerUrl": "capability-probe-worker.js",
            "loopbackOrigin": loopbackOrigin ?? NSNull(),
            "schemeOrigin": MigoProbeSchemeHandler.origin,
        ]
        guard
            let configData = try? JSONSerialization.data(withJSONObject: config),
            let json = String(data: configData, encoding: .utf8)
        else {
            // Not a silent `{}` fallback: a page with no config runs a
            // different experiment, and reporting that as a set of capability
            // answers is worse than reporting the failure.
            pending = nil
            completion(
                .failure(GateError.pageFailed("the probe configuration could not be encoded")))
            return
        }

        let controller = webView.configuration.userContentController
        controller.removeAllUserScripts()
        controller.addUserScript(
            WKUserScript(
                source: "window.__migoProbeConfig = \(json);",
                injectionTime: .atDocumentStart,
                forMainFrameOnly: true))
        // Console forwarding, injected rather than relied upon. A page that
        // throws before it reaches its own error handler leaves nothing behind
        // otherwise, and "the gate hung" is a diagnosis nobody can act on.
        controller.addUserScript(
            WKUserScript(
                source: Self.consoleBridge,
                injectionTime: .atDocumentStart,
                forMainFrameOnly: false))

        consoleLines.removeAll()
        pending = { [weak self] result in
            guard let self else { return }
            self.watchdog?.cancel()
            self.watchdog = nil
            switch result {
            case .failure(let error):
                completion(.failure(error))
            case .success(let report):
                completion(.success(self.assemble(report: report, origin: origin, webView: webView)))
            }
        }

        // The page's own probes are bounded, so past this the page is not
        // running. Reported as a distinct failure, carrying whatever the page
        // managed to log, because "no report" and "no page" need different
        // fixes.
        let watchdog = DispatchWorkItem { [weak self] in
            guard let self, let handler = self.pending else { return }
            self.pending = nil
            let tail = self.consoleLines.suffix(8).joined(separator: " | ")
            handler(
                .failure(
                    GateError.timedOut(
                        "\(origin.rawValue) produced no report within "
                            + "\(Int(Self.originTimeout)) s"
                            + (tail.isEmpty ? "; the page logged nothing" : "; the page logged: \(tail)"))))
        }
        self.watchdog = watchdog
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.originTimeout, execute: watchdog)

        webView.load(URLRequest(url: pageURL))
    }

    /// Forwards console output and uncaught errors to the native side.
    ///
    /// `forMainFrameOnly: false` so a Worker's parent document is covered too;
    /// a Worker's own scope is not reachable from a user script, which is why
    /// the Worker reports its failures as capability answers instead.
    static let consoleBridge = """
        (function () {
          function send(kind, args) {
            try {
              var text = Array.prototype.map.call(args, function (a) {
                try { return typeof a === 'string' ? a : JSON.stringify(a); }
                catch (e) { return String(a); }
              }).join(' ');
              window.webkit.messageHandlers.migoConsole.postMessage(kind + ': ' + text);
            } catch (e) { /* a console bridge that throws must not break the page */ }
          }
          ['log', 'warn', 'error'].forEach(function (kind) {
            var original = console[kind];
            console[kind] = function () { send(kind, arguments); return original.apply(console, arguments); };
          });
          window.addEventListener('error', function (event) {
            send('uncaught', [event.message + ' at ' + event.filename + ':' + event.lineno]);
          });
          window.addEventListener('unhandledrejection', function (event) {
            send('unhandled-rejection', [String(event.reason)]);
          });
        })();
        """

    private func assemble(
        report: [String: Any], origin: MigoProbeOrigin, webView: WKWebView
    ) -> MigoCapabilityRecord {
        var answers: [MigoProbeCapability: MigoCapabilityAnswer] = [:]
        if let raw = report["capabilities"] as? [String: [String: Any]] {
            for (name, body) in raw {
                guard
                    let capability = MigoProbeCapability(rawValue: name),
                    let stateName = body["state"] as? String,
                    let state = MigoCapabilityState(rawValue: stateName),
                    let evidence = body["evidence"] as? String
                else {
                    // A malformed answer is dropped here and reappears as
                    // `not_probed` when the record encodes, which says nobody
                    // answered rather than inventing a no.
                    continue
                }
                answers[capability] = MigoCapabilityAnswer(
                    state: state, evidence: evidence, value: body["value"] as? String)
            }
        }
        answers[.noLocalNetworkPrompt] = localNetworkAnswer(for: origin)

        let environment = MigoProbeEnvironment.capture(webView: webView)
        return MigoCapabilityRecord(
            runId: attestation.runId,
            capturedAt: ISO8601DateFormatter().string(from: Date()),
            deviceClass: environment.deviceClass,
            hardwareIdentifier: environment.hardwareIdentifier,
            ramBytes: environment.ramBytes,
            osVersion: environment.osVersion,
            osBuild: environment.osBuild,
            webkitBuild: environment.webkitBuild,
            appBuild: environment.appBuild,
            lockdownMode: attestation.lockdownMode,
            origin: origin,
            capabilities: answers)
    }

    /// A6/G0.3, and the one answer no code can take alone.
    ///
    /// There is no API that reports whether the system presented the
    /// local-network alert. What the process can see is whether traffic flowed,
    /// and traffic flows both when nobody was asked and when the operator
    /// tapped Allow. So the observation is an input, and when nobody made it
    /// the evidence says exactly what was and was not established.
    private func localNetworkAnswer(for origin: MigoProbeOrigin) -> MigoCapabilityAnswer {
        if origin == .customScheme {
            return .unsupported(evidence: "this origin opens no socket, so no alert can apply")
        }
        guard listener.origin != nil else {
            return .unavailable(evidence: "the loopback listener did not come up")
        }
        switch attestation.localNetworkPromptObserved {
        case .some(true):
            return .unavailable(
                evidence: "the operator recorded a local-network permission alert during the run")
        case .some(false):
            return .available(
                evidence: "the operator watched the run and recorded no local-network alert")
        case .none:
            return .notProbed(
                evidence: "traffic flowed over 127.0.0.1, which is also what a run looks like "
                    + "after the operator taps Allow; no API reports the alert, and nobody "
                    + "attested")
        }
    }
}

/// A page that fails to load must fail the run.
///
/// Without this the gate waited on a message handler that a failed navigation
/// can never reach, and an operator watched a label that said "running..." for
/// as long as they were willing to.
extension MigoCapabilityGate: WKNavigationDelegate {
    public func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        failLoad("the navigation never started: \(error.localizedDescription)")
    }

    public func webView(
        _ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error
    ) {
        failLoad("the page load failed: \(error.localizedDescription)")
    }

    public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        // A3/A4: WebContent has its own memory budget and its own reasons to be
        // killed. A run that lost the process did not measure a slow device.
        failLoad("WebContent was terminated during the run")
    }

    private func failLoad(_ reason: String) {
        guard let handler = pending else { return }
        pending = nil
        watchdog?.cancel()
        watchdog = nil
        let tail = consoleLines.suffix(8).joined(separator: " | ")
        handler(
            .failure(
                GateError.pageFailed(reason + (tail.isEmpty ? "" : "; the page logged: \(tail)"))))
    }
}

extension MigoCapabilityGate: WKScriptMessageHandler {
    public func userContentController(
        _ controller: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        if message.name == "migoConsole" {
            if let line = message.body as? String {
                // Bounded: a page in a logging loop must not become the run's
                // memory profile.
                if consoleLines.count < 200 { consoleLines.append(line) }
            }
            return
        }
        guard let handler = pending else { return }
        pending = nil
        watchdog?.cancel()
        watchdog = nil
        guard
            let text = message.body as? String,
            let data = text.data(using: .utf8),
            let report = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            handler(.failure(GateError.noReport("the page posted a body that was not JSON")))
            return
        }
        if let harnessError = report["harness_error"] as? String {
            handler(.failure(GateError.noReport(harnessError)))
            return
        }
        handler(.success(report))
    }
}
