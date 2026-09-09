import MigoAppleCore
import WebKit
import XCTest

@testable import MigoAppleWebKit

#if os(iOS)
    import UIKit

    /// The WebKit Full lane, end to end in a real web view.
    ///
    /// Everything the lane *decides* is tested in the engine-free package, where a
    /// pull request runs it in seconds. What cannot be tested there is whether the
    /// pieces are wired to each other: whether the shim WebKit injects reaches the
    /// handlers the session installed, whether the origin answers the requests
    /// WebKit actually makes, and whether a refusal is a refusal in WebKit's own
    /// loader rather than in a unit test's idea of one.
    ///
    /// That needs a `WKWebView`, which needs a simulator -- and a simulator is enough,
    /// because none of this is a measurement. The capability gate's rule that
    /// simulator answers are not evidence is about what a *device* can host; whether
    /// a message handler is reachable is not a device question.
    final class MigoWebKitSessionTests: XCTestCase {

        private var root: URL!
        private var window: UIWindow!

        override func setUpWithError() throws {
            root = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("migo-lane-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        }

        override func tearDownWithError() throws {
            window?.isHidden = true
            window = nil
            try? FileManager.default.removeItem(at: root)
        }

        private func write(_ text: String, to name: String) throws {
            try Data(text.utf8).write(to: root.appendingPathComponent(name))
        }

        /// Mount the session, because an unmounted web view is one of the host shapes
        /// the plan measures separately and is not the one this lane ships.
        private func mount(_ session: MigoWebKitSession) {
            let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
            let controller = UIViewController()
            window.rootViewController = controller
            session.view.frame = controller.view.bounds
            controller.view.addSubview(session.view)
            window.isHidden = false
            window.makeKeyAndVisible()
            self.window = window
            session.start()
        }

        private final class Recorder: MigoWebKitSessionDelegate {
            var diagnostics: [[String: Any]] = []
            var refusals: [URL] = []
            var terminalFailures: [Int] = []
            var rebuilds: [Int] = []
            var extraFields: [String: String] = [:]
            var onDiagnostic: (([String: Any]) -> Void)?

            func session(
                _ session: MigoWebKitSession, didReceiveDiagnostic diagnostic: [String: Any]
            ) {
                diagnostics.append(diagnostic)
                onDiagnostic?(diagnostic)
            }
            func session(_ session: MigoWebKitSession, refusedNavigationTo url: URL) {
                refusals.append(url)
            }
            func session(_ session: MigoWebKitSession, didFailTerminallyAfter count: Int) {
                terminalFailures.append(count)
            }
            func session(_ session: MigoWebKitSession, willRebuildAfterTermination count: Int) {
                rebuilds.append(count)
            }
            func sessionEnvironmentFields(_ session: MigoWebKitSession) -> [String: String] {
                extraFields
            }
        }

        /// Runs `script` in the page and reports through `diagnostics.report`.
        private func page(reporting script: String) -> String {
            """
            <!doctype html><meta charset="utf-8"><title>lane</title>
            <script type="module">
            const report = (value) => migoHost.diagnostics.report(value);
            (async () => {
              try { report(await (async () => { \(script) })()); }
              catch (error) { report({ failed: String(error && error.message || error) }); }
            })();
            </script>
            """
        }

        /// How long a mounted page is given to report.
        ///
        /// A timeout is here to turn a hang into a failure, so its value has to be
        /// longer than the slowest legitimate run on the slowest machine this suite
        /// runs on -- not just above what a warm Mac does. Twenty seconds was the
        /// latter, and it failed exactly once in CI: on the alphabetically first test,
        /// which pays the cold launch of the whole WebKit stack in a simulator that
        /// has just booted. The other nine, running behind it, passed on the same
        /// machine in the same run.
        ///
        /// A warm-up in `setUp` would hide the cost rather than budget for it, and
        /// would need a timeout of its own.
        ///
        /// 120 was still a guess about the slowest machine. It is now a measurement.
        /// WebKit logs its own helper-process launch times, and across three reds and
        /// one green on the same lane:
        ///
        ///     red   WebContent 48.9 s   59.0 s   37.5 s
        ///           GPU        37.0 s   58.6 s   39.5 s
        ///     green WebContent  1.3-2.2 s
        ///
        /// So the cold launch this number is supposed to cover is not the ~2 s a
        /// healthy runner takes -- it is up to 59 s, on a machine starved enough that
        /// everything after the launch is slow by the same factor. 120 left about a
        /// minute for the load itself at 30x, which is why it kept landing just the
        /// wrong side of the line.
        ///
        /// 240 is twice the measured worst launch plus room for the load behind it.
        /// What it costs is two extra minutes on a genuinely hung run; what it buys
        /// is that a starved runner stops being reported as a product defect. It does
        /// not hide one either: the two records and the about:blank probe below still
        /// say what happened, and a real stall still fails -- just later.
        static let reportTimeout: TimeInterval = 240

        private func firstDiagnostic(
            from script: String, surface: MigoWebKitSurface = .default,
            timeout: TimeInterval = MigoWebKitSessionTests.reportTimeout
        ) throws -> [String: Any] {
            try write(page(reporting: script), to: "index.html")
            let recorder = Recorder()
            let arrived = expectation(description: "content reported")
            recorder.onDiagnostic = { _ in arrived.fulfill() }
            let session = MigoWebKitSession(
                surface: surface, contentRoot: root, delegate: recorder)
            mount(session)

            // On timeout, say what state the session reached. A bare "the
            // expectation was not fulfilled" is the least informative red a lane
            // can produce, and this one has now cost four CI iterations. The third
            // added the host's side and the fourth read it: a run reported
            // `started: 1, delivered: 431` with the page still at ten percent, so
            // the host had served the whole page and WebKit had done nothing with
            // it. That is why the navigation counters are here too -- between them
            // the two records say which side stopped, and neither says it alone.
            if XCTWaiter().wait(for: [arrived], timeout: timeout) != .completed {
                let webView = session.webView
                XCTFail(
                    "no diagnostic within \(Int(timeout))s."
                        + " url=\(webView?.url?.absoluteString ?? "nil")"
                        + " loading=\(webView.map { String($0.isLoading) } ?? "nil")"
                        + " progress=\(webView.map { String(format: "%.2f", $0.estimatedProgress) } ?? "nil")"
                        + " inWindow=\(webView?.window != nil)"
                        + " diagnostics=\(recorder.diagnostics.count)"
                        + " refusals=\(recorder.refusals.map(\.absoluteString))"
                        + " terminalFailures=\(recorder.terminalFailures)"
                        + " rebuilds=\(recorder.rebuilds)"
                        + " origin=\(session.originActivity)"
                        + " navigation=\(session.navigation)"
                        + ". Read the two records together. origin.started=0 means WebKit"
                        + " never asked the host for the page. origin.started>0 with"
                        + " finished=0 means the host never answered. origin.finished=1"
                        + " with navigation.commits=0 means the host answered in full and"
                        + " WebKit did not take it -- look at"
                        + " navigation.contentProcessTerminations and lastError, and at"
                        + " promised against delivered, which is the same stall when a"
                        + " body is short of its own Content-Length.")
            }
            return try XCTUnwrap(recorder.diagnostics.first)
        }

        // MARK: - the bridge reaches the handlers

        func testContentCallsABridgeMethodAndGetsTheHostsAnswer() throws {
            let diagnostic = try firstDiagnostic(from: "return await migoHost.environment.get();")
            XCTAssertEqual(
                diagnostic["lane"] as? String, MigoWebKitSurface.lane,
                "the reply did not come back from the session: \(diagnostic)")
            XCTAssertNotNil(diagnostic["locale"])
            XCTAssertNotNil(diagnostic["safeArea"])
        }

        func testAHostFieldCannotOverwriteTheLaneTheSessionIsActuallyRunning() throws {
            try write(page(reporting: "return await migoHost.environment.get();"), to: "index.html")
            let recorder = Recorder()
            recorder.extraFields = ["lane": "something_else", "buildNumber": "42"]
            let arrived = expectation(description: "content reported")
            recorder.onDiagnostic = { _ in arrived.fulfill() }
            let session = MigoWebKitSession(contentRoot: root, delegate: recorder)
            mount(session)
            wait(for: [arrived], timeout: Self.reportTimeout)
            let diagnostic = try XCTUnwrap(recorder.diagnostics.first)
            XCTAssertEqual(
                diagnostic["lane"] as? String, MigoWebKitSurface.lane,
                "a host that relabelled the lane would make every diagnostic it collects wrong "
                    + "about which lane produced it")
            XCTAssertEqual(diagnostic["buildNumber"] as? String, "42")
        }

        func testAWithheldMethodThrowsInThePageAndNeverReachesTheHost() throws {
            let surface = try MigoWebKitSurface.configured(withholding: [.environment])
            let diagnostic = try firstDiagnostic(
                from: "return await migoHost.environment.get();", surface: surface)
            let failure = try XCTUnwrap(diagnostic["failed"] as? String)
            XCTAssertTrue(failure.contains("withheld"), failure)
            XCTAssertTrue(failure.contains("webkit-full-surface.json"), failure)
        }

        func testACapabilityWithNoMechanismIsNotEvenAProperty() throws {
            let diagnostic = try firstDiagnostic(
                from: """
                    return { bluetooth: typeof migoHost.bluetooth,
                             payment: typeof migoHost.payment,
                             engine: typeof globalThis.migo };
                    """)
            XCTAssertEqual(diagnostic["bluetooth"] as? String, "undefined")
            XCTAssertEqual(diagnostic["payment"] as? String, "undefined")
            XCTAssertEqual(
                diagnostic["engine"] as? String, "undefined",
                "content that feature-detects the engine's namespace must not find one in a lane "
                    + "that has no engine")
        }

        // MARK: - the origin answers what WebKit asks

        func testContentCanFetchItsOwnPackage() throws {
            try write("{\"name\":\"probe\"}", to: "manifest.json")
            let diagnostic = try firstDiagnostic(
                from: """
                    const response = await fetch('/manifest.json');
                    return { status: response.status,
                             type: response.headers.get('content-type'),
                             body: await response.text() };
                    """)
            XCTAssertEqual(diagnostic["status"] as? Int, 200)
            XCTAssertEqual(diagnostic["type"] as? String, "application/json")
            XCTAssertEqual(diagnostic["body"] as? String, "{\"name\":\"probe\"}")
        }

        func testTheOriginAnswersARangeRequestWithTheRangeAsked() throws {
            try write(String(repeating: "abcdefghij", count: 100), to: "atlas.bin")
            let diagnostic = try firstDiagnostic(
                from: """
                    const response = await fetch('/atlas.bin', { headers: { Range: 'bytes=10-19' } });
                    return { status: response.status,
                             range: response.headers.get('content-range'),
                             body: await response.text() };
                    """)
            XCTAssertEqual(
                diagnostic["status"] as? Int, 206,
                "an origin that answers every request with the whole file cannot back a seeking "
                    + "audio element: \(diagnostic)")
            XCTAssertEqual(diagnostic["range"] as? String, "bytes 10-19/1000")
            XCTAssertEqual(diagnostic["body"] as? String, "abcdefghij")
        }

        func testNoEncodingOfATraversalServesAFileOutsideThePackage() throws {
            // The first version of this test expected a 403 and got a 404, which is
            // the more interesting answer: WebKit's URL layer normalises `/../x` to
            // `/x` before the request is ever handed to the scheme handler, so a
            // plain traversal cannot reach the origin's own containment check at all.
            // That check is defence in depth -- and it is tested directly, against a
            // real directory, in MigoWebKitOriginRulesTests.
            //
            // So what this asserts is the property that actually matters and holds
            // whatever the URL layer does: no spelling of the escape returns the file.
            // The statuses are reported rather than asserted, because which layer
            // refused is WebKit's business and may change with a WebKit release; that
            // the bytes stay outside is ours.
            let outside = root.deletingLastPathComponent()
                .appendingPathComponent("outside-\(UUID().uuidString).txt")
            try Data("secret".utf8).write(to: outside)
            defer { try? FileManager.default.removeItem(at: outside) }
            let name = outside.lastPathComponent
            let diagnostic = try firstDiagnostic(
                from: """
                    const attempts = ['/../\(name)', '/%2e%2e/\(name)', '/%2E%2E%2F\(name)',
                                      '/assets/../../\(name)', '/.%2e/\(name)'];
                    const results = [];
                    for (const path of attempts) {
                      try {
                        const response = await fetch(path);
                        results.push({ path, status: response.status,
                                       body: (await response.text()).slice(0, 20) });
                      } catch (error) {
                        results.push({ path, threw: String(error && error.message || error) });
                      }
                    }
                    return { results };
                    """)
            let results = try XCTUnwrap(diagnostic["results"] as? [[String: Any]])
            XCTAssertEqual(results.count, 5, "not every spelling was attempted: \(diagnostic)")
            for result in results {
                XCTAssertNotEqual(
                    result["body"] as? String, "secret",
                    "\(result["path"] ?? "?") served a file from outside the content package")
                if let status = result["status"] as? Int {
                    XCTAssertNotEqual(
                        status, 200,
                        "\(result["path"] ?? "?") answered 200 for something outside the package")
                }
            }
        }

        func testAMissingFileIsAFourOhFourAndNotTheIndexPage() throws {
            let diagnostic = try firstDiagnostic(
                from: """
                    const response = await fetch('/nope.png');
                    return { status: response.status };
                    """)
            XCTAssertEqual(
                diagnostic["status"] as? Int, 404,
                "a missing asset answered with the index page is an image decode error somewhere "
                    + "far away from the missing file")
        }

        // MARK: - lifecycle

        func testTheHostDeliversLifecycleToASubscriber() throws {
            try write(
                """
                <!doctype html><meta charset="utf-8"><title>lane</title>
                <script type="module">
                migoHost.lifecycle.observe((event) => { migoHost.diagnostics.report(event); });
                await migoHost.lifecycle.observe(() => {});
                migoHost.diagnostics.report({ ready: true });
                </script>
                """, to: "index.html")
            let recorder = Recorder()
            let ready = expectation(description: "content subscribed")
            let delivered = expectation(description: "content heard a phase")
            recorder.onDiagnostic = { diagnostic in
                if diagnostic["ready"] as? Bool == true { ready.fulfill() }
                if diagnostic["phase"] as? String == "didEnterBackground" { delivered.fulfill() }
            }
            let session = MigoWebKitSession(contentRoot: root, delegate: recorder)
            mount(session)
            wait(for: [ready], timeout: Self.reportTimeout)

            NotificationCenter.default.post(
                name: UIApplication.didEnterBackgroundNotification, object: nil)
            wait(for: [delivered], timeout: Self.reportTimeout)
        }

        // MARK: - navigation

        func testContentCannotLeaveItsOwnOrigin() throws {
            try write(
                """
                <!doctype html><meta charset="utf-8"><title>lane</title>
                <script type="module">
                migoHost.diagnostics.report({ ready: true });
                location.href = 'https://example.com/';
                </script>
                """, to: "index.html")
            let recorder = Recorder()
            let ready = expectation(description: "content ran")
            recorder.onDiagnostic = { _ in ready.fulfill() }
            let session = MigoWebKitSession(contentRoot: root, delegate: recorder)
            mount(session)
            wait(for: [ready], timeout: Self.reportTimeout)

            // Polled rather than checked once after a fixed delay. A single look three
            // seconds later is a race that the machine wins on a busy CI runner, and
            // it fails by waiting out the whole timeout -- a slow red with a message
            // about the wrong thing.
            let refused = expectation(description: "the navigation was refused")
            let poll = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { timer in
                if recorder.refusals.contains(where: { $0.host == "example.com" }) {
                    timer.invalidate()
                    refused.fulfill()
                }
            }
            RunLoop.main.add(poll, forMode: .common)
            wait(for: [refused], timeout: Self.reportTimeout)
            poll.invalidate()
            XCTAssertEqual(
                session.webView?.url?.scheme, MigoWebKitContentOrigin.scheme,
                "the web view left the content origin")
        }
    }
#endif
