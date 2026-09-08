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

        public var description: String {
            switch self {
            case .resourcesMissing(let name): return "the probe resource \(name) is not in the bundle"
            case .pageFailed(let reason): return "the probe page failed to load: \(reason)"
            case .noReport(let reason): return "the probe page produced no report: \(reason)"
            }
        }
    }

    private let attestation: Attestation
    private let listener: MigoLoopbackListener
    private let schemeHandler: MigoProbeSchemeHandler
    private var pending: ((Result<[String: Any], Error>) -> Void)?
    private var webView: WKWebView?

    public init(attestation: Attestation = Attestation()) throws {
        self.attestation = attestation
        let resources = try Self.loadResources()
        self.listener = MigoLoopbackListener(resources: resources)
        self.schemeHandler = MigoProbeSchemeHandler(resources: resources)
        super.init()
    }

    // MARK: - resources

    static func loadResources() throws -> [String: (mime: String, body: Data)] {
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
                let url = Bundle.module.url(forResource: stem, withExtension: suffix),
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

        let view = WKWebView(frame: .zero, configuration: configuration)
        webView = view
        return view
    }

    // MARK: - the run

    /// Runs both origins and hands back one record each.
    ///
    /// Sequential rather than concurrent. Two pages measuring JIT warmup at the
    /// same time on the same device measure each other.
    public func run(
        in webView: WKWebView, completion: @escaping (Result<[MigoCapabilityRecord], Error>) -> Void
    ) {
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
        let config: [String: Any] = [
            "workerUrl": "capability-probe-worker.js",
            "loopbackOrigin": loopbackOrigin as Any,
            "schemeOrigin": MigoProbeSchemeHandler.origin,
        ]
        let json =
            (try? JSONSerialization.data(withJSONObject: config))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"

        let controller = webView.configuration.userContentController
        controller.removeAllUserScripts()
        controller.addUserScript(
            WKUserScript(
                source: "window.__migoProbeConfig = \(json);",
                injectionTime: .atDocumentStart,
                forMainFrameOnly: true))

        pending = { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                completion(.failure(error))
            case .success(let report):
                completion(.success(self.assemble(report: report, origin: origin, webView: webView)))
            }
        }
        webView.load(URLRequest(url: pageURL))
    }

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

extension MigoCapabilityGate: WKScriptMessageHandler {
    public func userContentController(
        _ controller: WKUserContentController, didReceive message: WKScriptMessage
    ) {
        guard let handler = pending else { return }
        pending = nil
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
