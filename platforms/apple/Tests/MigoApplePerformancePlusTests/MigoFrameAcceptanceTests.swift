import MigoAppleFrameHarness
import MigoEngine
import XCTest

@testable import MigoApplePerformancePlus

#if os(iOS)
    import UIKit

    /// The lane's acceptance: a frame content produced in WebContent draws, and a
    /// read of it through the synchronous barrier sees it.
    ///
    /// Every other test in this package stops at a boundary it can see both sides
    /// of. `MigoExternalFramePixelTests` submits a committed packet through the C
    /// ABI and reads it back; `MigoPerformancePlusHostTests` shows that bytes
    /// content submits arrive at a closure. Neither says the two are the same bytes
    /// going the same way. So this runs the whole path: a real `WKWebView` loads
    /// the producer page, starts a module worker, content `fetch`es the committed
    /// frame off its own origin and submits it, the frame crosses the transport
    /// into `MigoFrameChannel`, the engine executes it, and the pixels come back
    /// through the barrier -- the path a blocked `readPixels` in WebContent takes.
    ///
    /// **Why there are two tests, and why the second waits.** Every packet on this
    /// lane ends a frame, so the producer's read of frame N reaches the host after
    /// frame N was submitted -- and possibly after it was presented. With
    /// DrawingBuffer bypass on, a present leaves the window surface undefined, and
    /// the first read snapshots that surface. Measured on the iOS simulator from an
    /// identical frame: a read that beat the present by 2 ms saw blue, and a present
    /// that beat the read by 2 ms left `[0,0,0,0]`. The first test is the ordering
    /// that happened to work; the second makes the other ordering the only one, and
    /// is red for as long as this lane's sessions can enter bypass.
    ///
    /// **The fixture is the same file, not a copy.** It lives in
    /// `MigoAppleFrameHarness`, and both this and `MigoExternalFramePixelTests`
    /// read it from there: two copies are two things that can differ, and this
    /// test could not tell that from the transport corrupting one.
    final class MigoFrameAcceptanceTests: XCTestCase {

        private var contentRoot: URL!
        private var window: UIWindow!
        private var host: MigoPerformancePlusHost?
        private var harness: MigoFrameHarness?

        /// The clock a host would pass in, and the budget derived from it. Only the
        /// difference is used. 60 s for the reason `MigoExternalFramePixelTests`
        /// gives: the first readback in a process pays ANGLE's load and EGL
        /// bring-up, and a smaller budget would assert how fast a starved runner is.
        private let now: UInt64 = 1_000_000_000
        private var deadline: UInt64 { now + 60_000_000_000 }

        override func setUpWithError() throws {
            try super.setUpWithError()
            // The renderer's own account is the only thing that can say why an
            // accepted frame drew nothing, and without this it says nothing: the
            // bypass transitions that explained the first failure of this test
            // were invisible until it was set.
            setenv("MIGO_CAPI_LOG", "info", 1)
            contentRoot = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("migo-acceptance-\(UUID().uuidString)")
            try FileManager.default.createDirectory(
                at: contentRoot.appendingPathComponent("game"), withIntermediateDirectories: true)
        }

        override func tearDownWithError() throws {
            host?.stop()
            host = nil
            window?.isHidden = true
            window = nil
            if let harness {
                XCTAssertTrue(harness.shutDown(), "the retired surface never reported RELEASED")
            }
            harness = nil
            try? FileManager.default.removeItem(at: contentRoot)
            try super.tearDownWithError()
        }

        /// Content submits one frame and the host reads it back at once.
        func testAFrameContentSubmitsIsWhatTheBarrierReadsBack() throws {
            try submitFromContentThenRead(afterRunLoop: 0)
        }

        /// The same frame, read after the run loop has turned for long enough that
        /// the frame has presented.
        ///
        /// Half a second against a present that follows its frame by a few
        /// milliseconds. The wait cannot make this pass wrongly: a runner too
        /// starved to present in that time only exercises the other ordering, which
        /// the first test already covers.
        func testTheReadStillSeesTheFrameAfterItHasPresented() throws {
            try submitFromContentThenRead(afterRunLoop: 0.5)
        }

        private func submitFromContentThenRead(afterRunLoop settle: TimeInterval) throws {
            let harness = try MigoFrameHarness()
            self.harness = harness

            let fixture = try MigoFrameHarness.fixture(named: "clear-blue-frame")
            // Served to content off its own origin as a game asset would be, and
            // fetched rather than handed over, because that is how a game's data
            // reaches a game.
            try fixture.write(to: contentRoot.appendingPathComponent("game/frame.bin"))
            try Data(
                """
                export async function start({ session }) {
                  const response = await fetch("/game/frame.bin");
                  const packet = new Uint8Array(await response.arrayBuffer());
                  const outcome = session.submit(packet);
                  self.postMessage({ type: "submitted", outcome: String(outcome), bytes: packet.length });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            let submitted = expectation(description: "content submitted the frame")
            var submittedBytes: Int?
            var submitOutcome: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "submitted":
                    submittedBytes = report["bytes"] as? Int
                    submitOutcome = report["outcome"] as? String
                    submitted.fulfill()
                case "failed":
                    submitOutcome =
                        "failed at \(report["stage"] as? String ?? "?"): "
                        + "\(report["detail"] as? String ?? "?")"
                    submitted.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()

            // 240 s because WebContent's own helper processes have taken 37-59 s to
            // launch on a starved CI runner; that is WebKit's startup, not Migo's.
            wait(for: [submitted], timeout: 240)
            XCTAssertEqual(
                submitOutcome, "true",
                "content's submit did not go out (it reports a reason string when it does not)")
            XCTAssertEqual(
                submittedBytes, fixture.count,
                "content read a different number of bytes than the committed frame has")

            // Accepted, asserted apart from the pixels: an accepted frame that draws
            // nothing and a refused frame both read back as the wrong colour, and they
            // want opposite investigations.
            let pollDeadline = Date().addingTimeInterval(30)
            while host.channel.currentStatistics.framesAccepted < 1, Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            XCTAssertEqual(host.channel.currentStatistics.framesAccepted, 1)
            XCTAssertEqual(host.channel.currentStatistics.framesRefused, 0)

            if settle > 0 {
                RunLoop.current.run(until: Date().addingTimeInterval(settle))
            }

            let pixel = try readPixel(session: harness.session, x: 0, y: 0)
            XCTAssertEqual(
                pixel, [0, 0, 255, 255],
                """
                the frame crossed the transport, was accepted, and reads back as \(pixel). \
                [0,0,0,0] after a present is the window surface a swap left undefined: \
                this session entered DrawingBuffer bypass, which a lane whose reads \
                follow their frame cannot allow.
                """)
        }

        /// Attached and off-screen: an unattached web view is killed since iOS 16
        /// and an occluded one stops executing JavaScript.
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

        /// One pixel, through the synchronous barrier.
        private func readPixel(session: OpaquePointer, x: Int32, y: Int32) throws -> [UInt8] {
            var request = MigoSyncRequestDescriptor()
            request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
            request.abi_version = MIGO_ABI_VERSION_CURRENT
            request.runtime_generation = 1
            request.surface_generation = MigoFrameHarness.fixtureGeneration
            request.resource_epoch = 0
            request.triggering_sequence = 1
            request.deadline_nanos = deadline
            request.operation = MIGO_SYNC_OP_READ_PIXELS
            request.max_reply_bytes = 4

            var outcome = MigoSyncOutcome()
            outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
            outcome.abi_version = MIGO_ABI_VERSION_CURRENT

            let params = MigoFrameHarness.readPixelsParameters(x: x, y: y, width: 1, height: 1)
            let posted = params.withUnsafeBufferPointer { buffer in
                migo_session_post_sync_request(
                    session, &request, buffer.baseAddress, buffer.count, now, &outcome)
            }
            XCTAssertEqual(posted, MIGO_OK, "post")
            XCTAssertEqual(
                outcome.state, MIGO_SYNC_STATE_READY,
                "the readback failed with error \(outcome.error)")

            // Sized from the request rather than the outcome: a failed readback
            // reports zero bytes, and an array sized from that is one the caller
            // indexes past -- a crash where an assertion should have been.
            var pixel = [UInt8](repeating: 0, count: 4)
            var written = 0
            let taken = pixel.withUnsafeMutableBufferPointer { out in
                migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
            }
            XCTAssertEqual(taken, MIGO_OK, "take")
            XCTAssertEqual(written, 4, "one RGBA8 pixel")
            return pixel
        }
    }
#endif
