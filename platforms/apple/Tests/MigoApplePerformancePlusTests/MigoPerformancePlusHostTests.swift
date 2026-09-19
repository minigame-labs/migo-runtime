import WebKit
import XCTest

// One symbol, not the module. `MigoAppleCore` also declares a `MigoFrameChannel`
// -- the policy's channel enum -- and this target already has one from
// `MigoApplePerformancePlus`. Inside that module the local declaration wins; in
// a test target that imports both, neither does, and every existing use of the
// class would become ambiguous.
import enum MigoAppleCore.MigoFrameChannelPolicy

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
                    return .accepted
                },
                takeDownlink: { _ in 0 })

            let ready = expectation(description: "the producer reports ready")
            var connected = false
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
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
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(submit: { _ in .accepted }, takeDownlink: { _ in 0 }))
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

        /// A synchronous call from content's Worker reaches the answerer, and the
        /// header and reply come back to the Worker as one body.
        ///
        /// The answerer is a closure, so what is asserted is the endpoint: the
        /// route, the body passed through unchanged, the two pieces joined in
        /// order by WebKit rather than by a copy here, and the Worker's decoder
        /// reading them. The engine's half is `MigoFrameAcceptanceTests`.
        func testASynchronousCallFromContentIsAnsweredInOneBody() throws {
            try writeContent(
                """
                import { encodeReadPixelsParams, SYNC_OP_READ_PIXELS } from "/__migo/sync-mailbox.mjs";
                export function start({ sync }) {
                  let detail;
                  try {
                    const reply = sync.call({
                      runtimeGeneration: 1n, surfaceGeneration: 2n, resourceEpoch: 3n,
                      triggeringSequence: 4n, operation: SYNC_OP_READ_PIXELS, maxReplyBytes: 4,
                      timeoutMillis: 5000,
                      params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 1, height: 1 }),
                    });
                    detail = Array.from(reply).join(",");
                  } catch (error) {
                    detail = `${error.name}: ${error.message}`;
                  }
                  self.postMessage({ type: "answered", detail });
                }
                """, to: "game/main.mjs")

            var calls: [Data] = []
            let callsLock = NSLock()
            let channel = MigoFrameChannel(
                submit: { _ in .accepted }, takeDownlink: { _ in 0 },
                answerSync: { call in
                    callsLock.lock()
                    calls.append(call)
                    callsLock.unlock()
                    // READY, request 9, four reply bytes -- written the way the
                    // document lays it out, which is the one place this test
                    // builds an answer.
                    var header = Data()
                    for word: UInt32 in [2, 0, 9, 4] {
                        withUnsafeBytes(of: word.littleEndian) { header.append(contentsOf: $0) }
                    }
                    return .init(header: header, reply: Data([10, 20, 30, 40]))
                })

            let answered = expectation(description: "content's call was answered")
            var detail: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: channel)
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "answered":
                    detail = report["detail"] as? String
                    answered.fulfill()
                case "failed":
                    detail = "failed at \(report["stage"] as? String ?? "?"): \(report["detail"] as? String ?? "?")"
                    answered.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [answered], timeout: Self.reportTimeout)

            XCTAssertEqual(detail, "10,20,30,40", "the Worker did not read the answer's reply")
            callsLock.lock()
            let seen = calls
            callsLock.unlock()
            XCTAssertEqual(seen.count, 1)
            // The envelope (56 bytes) and readPixels' arguments (32), unchanged.
            XCTAssertEqual(seen.first?.count, 88)
            XCTAssertEqual(host.originActivity.syncCallsAnswered, 1)
            XCTAssertEqual(host.originActivity.syncCallsRefused, 0)
        }

        /// An answer that arrives after its page went away goes nowhere -- rather
        /// than to a task WebKit has already stopped, which traps and takes the
        /// app with it.
        ///
        /// The page going away is the case that exists. A producer cannot give up
        /// on a call from its side: WebKit applies no timeout to a synchronous
        /// request on this origin (the first version of this test assumed one,
        /// and the Worker waited out the thirty seconds the answerer held it for).
        /// What can happen is the host tearing the web view down while the engine
        /// is still answering -- a backgrounded app, a session being replaced --
        /// and then the answer arrives for a task that no longer exists.
        func testAnAnswerForACallWhosePageWentAwayIsDropped() throws {
            try writeContent(
                """
                import { encodeReadPixelsParams, SYNC_OP_READ_PIXELS } from "/__migo/sync-mailbox.mjs";
                export function start({ sync }) {
                  sync.call({
                    runtimeGeneration: 1n, surfaceGeneration: 1n, resourceEpoch: 0n,
                    triggeringSequence: 0n, operation: SYNC_OP_READ_PIXELS, maxReplyBytes: 4,
                    timeoutMillis: 60000,
                    params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 1, height: 1 }),
                  });
                }
                """, to: "game/main.mjs")

            let arrived = expectation(description: "the call reached the answerer")
            let released = DispatchSemaphore(value: 0)
            let answerReturned = expectation(description: "the late answer was produced")
            let channel = MigoFrameChannel(
                submit: { _ in .accepted }, takeDownlink: { _ in 0 },
                answerSync: { _ in
                    arrived.fulfill()
                    // Held until the page is gone, so the answer is late by
                    // construction rather than by timing.
                    _ = released.wait(timeout: .now() + 60)
                    defer { answerReturned.fulfill() }
                    var header = Data()
                    for word: UInt32 in [2, 0, 1, 4] {
                        withUnsafeBytes(of: word.littleEndian) { header.append(contentsOf: $0) }
                    }
                    return .init(header: header, reply: Data([1, 2, 3, 4]))
                })

            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: channel)
            self.host = host
            mount(host)
            try host.start()
            wait(for: [arrived], timeout: Self.reportTimeout)

            // The page goes away with the call outstanding: WebKit stops the task.
            host.stop()
            self.host = nil
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))

            // Now let the answer go, and give the main queue time to try to deliver it.
            released.signal()
            wait(for: [answerReturned], timeout: 10)
            RunLoop.current.run(until: Date().addingTimeInterval(0.5))
            XCTAssertEqual(
                host.originActivity.syncCallsAnswered, 1,
                "the answer was produced; reaching here without a trap is the assertion")
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
                    channel: MigoFrameChannel(submit: { _ in .accepted }, takeDownlink: { _ in 0 }))
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

#if os(iOS)
    /// The other uplink.
    ///
    /// Above `MigoFrameChannelPolicy.socketCeilingBytes` a frame is POSTed to the
    /// content origin instead of sent on the socket, because G0's P3 measured the
    /// scheme 4.4x faster and 4.6x cheaper at 1 MiB. What needs a real web view is
    /// the part no unit test reaches: whether `fetch` from a module Worker on a
    /// custom scheme delivers a megabyte-scale body to a `WKURLSchemeHandler` at
    /// all. WebKit 191362 -- "a POST body is lost on the way to the handler" -- is
    /// RESOLVED FIXED, and a fixed bug carried forward as a design constraint is
    /// how a transport gets eliminated without a measurement.
    extension MigoPerformancePlusHostTests {

        func testAFrameOverTheCeilingArrivesThroughTheSchemeHandler() throws {
            // One byte over, so the test is about the threshold rather than about
            // being large. The ceiling is read from the policy, not written here:
            // two copies of a measured number drift the first time it is remeasured.
            let ceiling = MigoFrameChannelPolicy.socketCeilingBytes
            try writeContent(
                """
                export async function start({ session }) {
                  const big = new Uint8Array(\(ceiling + 1));
                  // A pattern rather than zeros: a body that arrived as the right
                  // LENGTH of the wrong bytes is a different defect from a body
                  // that did not arrive, and zeros cannot tell them apart.
                  for (let i = 0; i < big.length; i += 1) big[i] = i & 0xff;
                  session.submit(big);
                }
                """, to: "game/main.mjs")

            let submitted = expectation(description: "the large frame reaches the engine")
            var seen: Data?
            let channel = MigoFrameChannel(
                submit: { packet in
                    seen = packet
                    submitted.fulfill()
                    return .accepted
                },
                takeDownlink: { _ in 0 })

            let ready = expectation(description: "the producer reports ready")
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: channel)
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "ready": ready.fulfill()
                case "failed":
                    failure =
                        "\(report["stage"] as? String ?? "?"): \(report["detail"] as? String ?? "?")"
                    ready.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()

            wait(for: [ready], timeout: Self.reportTimeout)
            XCTAssertNil(failure, "the producer reported a failure")
            wait(for: [submitted], timeout: 30)

            XCTAssertEqual(seen?.count, ceiling + 1, "the body arrived short")
            XCTAssertEqual(
                Array(seen ?? Data()), (0...(ceiling)).map { UInt8($0 & 0xff) },
                "the bytes that arrived are not the bytes content submitted")
            XCTAssertEqual(
                host.originActivity.framesDelivered, 1,
                "it must have come through the scheme handler, not the socket")
            XCTAssertEqual(host.originActivity.framesRefused, 0)
            XCTAssertEqual(
                channel.currentStatistics.framesAccepted, 1,
                "one frame, counted once, whichever uplink carried it")
        }

        func testAFrameAtTheCeilingStillGoesOverTheSocket() throws {
            // The other side of the same threshold, in the same place, because a
            // hybrid that sent everything over one channel would pass the test
            // above and be wrong.
            let ceiling = MigoFrameChannelPolicy.socketCeilingBytes
            try writeContent(
                """
                export async function start({ session }) {
                  session.submit(new Uint8Array(\(ceiling)));
                }
                """, to: "game/main.mjs")

            let submitted = expectation(description: "the frame reaches the engine")
            var seen: Data?
            let channel = MigoFrameChannel(
                submit: { packet in
                    seen = packet
                    submitted.fulfill()
                    return .accepted
                },
                takeDownlink: { _ in 0 })

            let ready = expectation(description: "the producer reports ready")
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: channel)
            self.host = host
            host.onReport = { report in
                if report["type"] as? String == "ready" || report["type"] as? String == "failed" {
                    ready.fulfill()
                }
            }
            mount(host)
            try host.start()

            wait(for: [ready], timeout: Self.reportTimeout)
            wait(for: [submitted], timeout: 30)
            XCTAssertEqual(seen?.count, ceiling)
            XCTAssertEqual(
                host.originActivity.framesDelivered, 0,
                "at exactly the ceiling the socket carries it; the scheme must not have seen it")
        }

        func testTheFrameEndpointRefusesAGet() throws {
            // Not a 404. A GET on the frame endpoint is a caller using it wrong,
            // and answering "no such file" sends whoever reads it looking for a
            // packaging fault.
            try writeContent(
                """
                export async function start() {
                  const response = await fetch("\(MigoPerformancePlusOrigin.framePath)");
                  self.postMessage({ type: "probe", status: response.status });
                }
                """, to: "game/main.mjs")

            let answered = expectation(description: "the endpoint answered")
            var status: Int?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, harnessEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(submit: { _ in .accepted }, takeDownlink: { _ in 0 }))
            self.host = host
            host.onReport = { report in
                if report["type"] as? String == "probe" {
                    status = report["status"] as? Int
                    answered.fulfill()
                }
            }
            mount(host)
            try host.start()

            wait(for: [answered], timeout: Self.reportTimeout)
            XCTAssertEqual(status, 405)
            XCTAssertEqual(host.originActivity.framesRefused, 1)
        }
    }
#endif
