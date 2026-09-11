import WebKit
import XCTest

@testable import MigoApplePerformancePlus

#if os(iOS)
    import UIKit

    /// The lane, end to end, minus the engine and the renderer.
    ///
    /// A real `WKWebView` loads the producer page from the reserved prefix, the page
    /// starts a real module worker, the worker opens a real WebSocket to the host's
    /// ephemeral port, and a frame content submits arrives at the closure the engine
    /// would be behind. Everything in that sentence except the last clause is the
    /// part no unit test reaches: `MigoFrameChannelTests` connects a
    /// `URLSessionWebSocketTask` written in Swift, which proves the channel and
    /// proves nothing about whether WebKit will run the producer at all.
    ///
    /// Simulator is enough. None of this is a measurement -- what a *device* can
    /// host is the capability gate's question, and it has answered it. Whether the
    /// page can start a module worker, whether the worker's imports resolve on a
    /// custom scheme, and whether a frame survives the trip are all structural.
    final class MigoPerformancePlusHostTests: XCTestCase {

        private var contentRoot: URL!
        private var window: UIWindow!
        private var host: MigoPerformancePlusHost?

        override func setUpWithError() throws {
            contentRoot = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("migo-pplus-\(UUID().uuidString)")
            try FileManager.default.createDirectory(
                at: contentRoot, withIntermediateDirectories: true)
        }

        override func tearDownWithError() throws {
            host?.stop()
            host = nil
            window?.isHidden = true
            window = nil
            try? FileManager.default.removeItem(at: contentRoot)
        }

        private func writeContent(_ text: String, to name: String) throws {
            let url = contentRoot.appendingPathComponent(name)
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data(text.utf8).write(to: url)
        }

        /// Attached and off-screen, which is the shape the lane ships: an unattached
        /// web view is killed since iOS 16 and an occluded one stops executing
        /// JavaScript, so "behind the CAMetalLayer" is not available.
        private func mount(_ host: MigoPerformancePlusHost) {
            let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
            let controller = UIViewController()
            window.rootViewController = controller
            host.view.frame = CGRect(x: -390, y: 0, width: 390, height: 844)
            controller.view.addSubview(host.view)
            window.isHidden = false
            window.makeKeyAndVisible()
            self.window = window
        }

        /// The same budget, and the same reason, as `MigoWebKitSessionTests`: a
        /// starved runner's WebContent launch has been measured at up to 59 seconds,
        /// and a timeout below that reports a product defect that is not there.
        private static let reportTimeout: TimeInterval = 240

        func testTheProducerStartsAndAFrameReachesTheEngine() throws {
            // Content, as the product will supply it: a module on the game's own
            // root, imported by the producer, handed a session. The bytes are
            // arbitrary -- the engine is a closure here, and what is being asserted
            // is that they arrive unchanged.
            try writeContent(
                """
                export function start({ session }) {
                  session.submit(new Uint8Array([7, 6, 5, 4, 3, 2, 1, 0]));
                }
                """, to: "game/main.mjs")

            let submitted = expectation(description: "a frame reaches the engine")
            var seen: Data?
            let channel = MigoFrameChannel(
                submit: { packet in
                    seen = packet
                    submitted.fulfill()
                    return true
                },
                takeDownlink: { _ in 0 })

            let ready = expectation(description: "the producer reports ready")
            var connected = false
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: channel)
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "connected": connected = true
                case "ready": ready.fulfill()
                case "failed":
                    // Recorded rather than ignored: a failure that only shows up as
                    // a timeout costs a CI round trip to learn what this line already
                    // knows.
                    failure =
                        "\(report["stage"] as? String ?? "?"): \(report["detail"] as? String ?? "?")"
                    ready.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()

            // Sequential, not one combined wait: a producer that reports a failure
            // fulfils `ready` and will never submit, and a combined wait would spend
            // the whole budget before saying so.
            wait(for: [ready], timeout: Self.reportTimeout)
            XCTAssertNil(failure, "the producer reported a failure")
            XCTAssertTrue(connected, "the worker never said it had a socket")
            // The submit happens inside content's `start`, so it precedes `ready` at
            // the producer. It crosses a different queue to get here, which is the
            // only reason this is a wait rather than an assertion.
            wait(for: [submitted], timeout: 10)
            XCTAssertEqual(
                seen, Data([7, 6, 5, 4, 3, 2, 1, 0]),
                "the engine must see the bytes content submitted, unchanged")
            XCTAssertEqual(channel.currentStatistics.framesAccepted, 1)
        }

        /// A game cannot replace the producer by shipping a file with its name.
        ///
        /// The rule is tested against strings in `MigoWebKitOriginRulesTests`; this
        /// is the same rule with WebKit's own loader doing the fetching, because a
        /// reserved prefix that the delivery layer forgot to pass through is a rule
        /// that holds everywhere except where it matters.
        func testAContentPackageCannotShadowTheProducer() throws {
            // A decoy PAGE as well as a decoy module. Both, because the prefix
            // protects both and because with only the module the injected-defect
            // run failed by timeout -- the shadowed page simply was not there in
            // the content root, so nothing loaded and nothing reported. Measured:
            // 243 seconds to say "unwaited expectation", against 1.6 seconds to
            // say which file was served. A guard that is right for an
            // uninformative reason is one someone later "fixes".
            try writeContent(
                """
                <!doctype html><meta charset="utf-8">
                <script type="module" src="./page-entry.mjs"></script>
                """, to: "__migo/producer-page.html")
            try writeContent(
                """
                globalThis.webkit.messageHandlers.migoPerformancePlus.postMessage({
                  type: "failed",
                  stage: "shadowed",
                  detail: "the content package's __migo/page-entry.mjs was served",
                });
                """, to: "__migo/page-entry.mjs")
            try writeContent(
                "export function start() {}", to: "game/main.mjs")

            let ready = expectation(description: "the producer reports ready")
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(submit: { _ in true }, takeDownlink: { _ in 0 }))
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "ready": ready.fulfill()
                case "failed":
                    failure = report["detail"] as? String ?? "?"
                    ready.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()

            wait(for: [ready], timeout: Self.reportTimeout)
            // The decoy would have blanked the configuration, so the page would have
            // reported a configuration failure instead of starting a worker.
            XCTAssertNil(failure, "a report arrived that the real producer would never make")
        }

        func testAMissingProducerBundleIsNamedRatherThanSilent() {
            // The packaging step is what puts the producer in the resource bundle,
            // and a build that skipped it otherwise presents as a worker that never
            // connects -- which reads as a transport fault and is not one.
            XCTAssertThrowsError(
                try MigoPerformancePlusHost(
                    configuration: .init(
                        contentRoot: contentRoot,
                        engineRoot: contentRoot.appendingPathComponent("nothing-here")),
                    channel: MigoFrameChannel(submit: { _ in true }, takeDownlink: { _ in 0 }))
            ) { error in
                guard case MigoPerformancePlusHost.StartFailure.engineModulesMissing = error else {
                    return XCTFail("expected engineModulesMissing, got \(error)")
                }
                XCTAssertTrue(
                    "\(error)".contains("build-apple-sdk.sh"),
                    "the message has to say what produces them")
            }
        }
    }
#endif
