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

        private func firstDiagnostic(
            from script: String, surface: MigoWebKitSurface = .default,
            timeout: TimeInterval = 20
        ) throws -> [String: Any] {
            try write(page(reporting: script), to: "index.html")
            let recorder = Recorder()
            let arrived = expectation(description: "content reported")
            recorder.onDiagnostic = { _ in arrived.fulfill() }
            let session = MigoWebKitSession(
                surface: surface, contentRoot: root, delegate: recorder)
            mount(session)
            wait(for: [arrived], timeout: timeout)
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
            wait(for: [arrived], timeout: 20)
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

        func testAFileOutsideThePackageIsRefusedAndNotServed() throws {
            let outside = root.deletingLastPathComponent()
                .appendingPathComponent("outside-\(UUID().uuidString).txt")
            try Data("secret".utf8).write(to: outside)
            defer { try? FileManager.default.removeItem(at: outside) }
            let diagnostic = try firstDiagnostic(
                from: """
                    const response = await fetch('/../\(outside.lastPathComponent)');
                    return { status: response.status, body: (await response.text()).slice(0, 40) };
                    """)
            XCTAssertNotEqual(
                diagnostic["body"] as? String, "secret",
                "the origin served a file from outside the content package")
            XCTAssertEqual(diagnostic["status"] as? Int, 403)
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
            wait(for: [ready], timeout: 20)

            NotificationCenter.default.post(
                name: UIApplication.didEnterBackgroundNotification, object: nil)
            wait(for: [delivered], timeout: 10)
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
            wait(for: [ready], timeout: 20)

            let refused = expectation(description: "the navigation was refused")
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
                if recorder.refusals.contains(where: { $0.host == "example.com" }) {
                    refused.fulfill()
                }
            }
            wait(for: [refused], timeout: 10)
            XCTAssertEqual(
                session.webView?.url?.scheme, MigoWebKitContentOrigin.scheme,
                "the web view left the content origin")
        }
    }
#endif
