import MigoAppleFrameHarness
import MigoEngine
import XCTest

@testable import MigoApplePerformancePlus

#if os(iOS)
    import UIKit

    /// The lane's acceptance: a frame content produced in WebContent draws.
    ///
    /// Every other test in this package stops at a boundary that it can see both
    /// sides of. `MigoExternalFramePixelTests` submits a committed packet through
    /// the C ABI and reads the pixels back, which establishes that these bytes
    /// draw. `MigoPerformancePlusHostTests` establishes that bytes content
    /// submits arrive at a closure. Neither says the two are the same bytes going
    /// the same way, and the interesting failures live exactly there -- a
    /// transport that delivers a truncated body, a producer that sends a view of
    /// the wrong buffer, a host that hands the engine a copy with a header
    /// rewritten.
    ///
    /// So this runs the whole path: a real `WKWebView` loads the producer page,
    /// starts a module Worker, the Worker imports content, content `fetch`es the
    /// committed frame off its own origin and submits it, the frame crosses the
    /// transport into `MigoFrameChannel`, the engine executes it on the GPU, and
    /// the pixels come back through the synchronous barrier -- which is the path
    /// a blocked `readPixels` in WebContent will take.
    ///
    /// **The fixture is the same file, not a copy.** It lives in
    /// `MigoAppleFrameHarness` and both tests read it from there. Two copies
    /// would be two things that can differ, and this test could not tell that
    /// apart from the transport corrupting one.
    final class MigoFrameAcceptanceTests: XCTestCase {

        private var contentRoot: URL!
        private var window: UIWindow!
        private var host: MigoPerformancePlusHost?
        private var harness: MigoFrameHarness?

        /// The clock a host would pass in, and the budget derived from it.
        ///
        /// Only the difference is used -- the library reads no clock of its own
        /// for this. 60 s for the same measured reason `MigoExternalFramePixelTests`
        /// gives: the first readback in a process pays ANGLE's load and EGL
        /// bring-up, and a test that named a small budget would be asserting how
        /// fast a starved runner is.
        private let now: UInt64 = 1_000_000_000
        private var deadline: UInt64 { now + 60_000_000_000 }

        override func setUpWithError() throws {
            try super.setUpWithError()
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

        func testAFrameContentSubmittedInWebContentDrawsPixelsThisSessionCanReadBack() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness

            // The committed frame, served to content off its own origin as a
            // game asset would be. Content fetches it rather than having it
            // handed over, because that is how a game's data reaches a game.
            try MigoFrameHarness.fixture(named: "clear-blue-frame")
                .write(to: contentRoot.appendingPathComponent("game/frame.bin"))
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

            wait(for: [submitted], timeout: 240)
            XCTAssertEqual(
                submitOutcome, "true",
                "content's submit did not go out (it reports a reason string when it does not)")
            XCTAssertEqual(
                submittedBytes, try MigoFrameHarness.fixture(named: "clear-blue-frame").count,
                "content read a different number of bytes than the committed frame has")

            // The engine accepted it, which is the host's side of the same event.
            // Asserted separately from the pixels: an accepted frame that draws
            // nothing and a refused frame both read back as the wrong colour, and
            // they want opposite investigations.
            let accepted = expectation(description: "the engine accepted it")
            let pollDeadline = Date().addingTimeInterval(30)
            while Date() < pollDeadline {
                if host.channel.currentStatistics.framesAccepted == 1 {
                    accepted.fulfill()
                    break
                }
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            wait(for: [accepted], timeout: 1)
            XCTAssertEqual(host.channel.currentStatistics.framesRefused, 0)

            let pixel = try readPixel(session: harness.session, x: 0, y: 0)
            XCTAssertEqual(
                pixel, [0, 0, 255, 255],
                """
                the frame crossed the transport and did not draw. The bytes arrived \
                (\(submittedBytes ?? -1) of them) and the engine accepted them, so this is \
                the renderer, not the wire.
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

            // Sized from the request rather than the outcome: a readback that
            // failed reports zero bytes, and an array sized from that is one the
            // caller indexes past -- a crash where an assertion should have been.
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
