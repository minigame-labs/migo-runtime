import MigoAppleFrameHarness
import enum MigoAppleCore.MigoFrameChannelPolicy
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

        /// A generation's first two frames, one on each uplink, execute in order.
        ///
        /// The hybrid uplink sends a packet above `socketCeilingBytes` as a
        /// scheme request and anything smaller on the socket, and those are two
        /// independent streams: nothing makes a request that left first arrive
        /// first. Ingress holds a packet that overtakes its predecessor -- but not
        /// before a generation's first packet has been accepted, because sequence
        /// 1 first is what leaves a replayed later packet nothing to wait on. So
        /// a scene load followed by its first ordinary frame, sent back to back,
        /// was the ordinary way to lose a session: measured on the simulator, the
        /// small frame won the race, was refused as a gap, and the surface stayed
        /// on the first frame.
        ///
        /// What keeps them in order is the producer, not the host: before the
        /// first verdict it has one credit, so the second frame waits for the
        /// first to be decided. The second submit is refused locally, and nothing
        /// is refused by the host.
        func testFramesKeepTheirOrderAcrossTheTwoUplinks() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { encodeFrame, SECTION_KIND_COMMAND_STREAM } from "/__migo/wire-frame-packet.mjs";
                import { MAGIC, STREAM_VERSION, OP_CLEAR, OP_CLEAR_COLOR } from "/__migo/render-opcodes.mjs";

                const scratch = new DataView(new ArrayBuffer(4));
                const bits = (v) => { scratch.setFloat32(0, v, true); return scratch.getUint32(0, true); };
                const header = (op, words) => ((words << 12) | op) >>> 0;

                function frame(sequence, rgba, clears) {
                  const words = [MAGIC, STREAM_VERSION,
                    header(OP_CLEAR_COLOR, 6), 1, bits(rgba[0]), bits(rgba[1]), bits(rgba[2]), bits(rgba[3])];
                  for (let i = 0; i < clears; i += 1) words.push(header(OP_CLEAR, 3), 1, 0x4000);
                  const stream = new Uint8Array(words.length * 4);
                  const view = new DataView(stream.buffer);
                  words.forEach((w, i) => view.setUint32(i * 4, w, true));
                  return encodeFrame({ launchNonce: 0xa3n, sequence, runtimeGeneration: 1n,
                    surfaceGeneration: 1n, resourceEpoch: 0n,
                    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }] });
                }

                export async function start({ session }) {
                  // 6,000 clears is 72 KiB of stream: above the socket ceiling.
                  const large = frame(1n, [0, 0, 1, 1], 6000);
                  const small = frame(2n, [1, 0, 0, 1], 1);
                  const first = session.submit(large);
                  // Back to back, as a scene load and its first frame would be.
                  const early = session.submit(small);
                  // And again until the window opens, which is what content does
                  // on its next tick; bounded, so a verdict that never comes is a
                  // failure this test reports rather than a hang.
                  let second = early;
                  for (let i = 0; second !== true && i < 30000; i += 1) {
                    await new Promise((resolve) => setTimeout(resolve, 1));
                    second = session.submit(small);
                  }
                  self.postMessage({ type: "submitted", outcome: `${first},${early},${second}`, bytes: large.length });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            let submitted = expectation(description: "content submitted both frames")
            var submittedBytes: Int?
            var outcome: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "submitted":
                    submittedBytes = report["bytes"] as? Int
                    outcome = report["outcome"] as? String
                    submitted.fulfill()
                case "failed":
                    outcome = "failed at \(report["stage"] as? String ?? "?"): \(report["detail"] as? String ?? "?")"
                    submitted.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [submitted], timeout: 240)
            XCTAssertEqual(
                outcome, "true,no-credit,true",
                "the first frame goes, the second waits for its verdict, then goes")
            XCTAssertGreaterThan(
                submittedBytes ?? 0, MigoFrameChannelPolicy.socketCeilingBytes,
                "the first frame has to be above the socket ceiling or this tests one uplink")

            let pollDeadline = Date().addingTimeInterval(30)
            while host.channel.currentStatistics.framesReceived < 2, Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            let statistics = host.channel.currentStatistics
            XCTAssertEqual(
                statistics.framesRefused, 0,
                """
                a frame was refused. Two frames that left in order on different uplinks \
                reached ingress out of order, and ingress answers a gap by ending the content.
                """)
            // Both accepted on arrival. Nothing was held: the second frame left
            // after the first was decided, so it could not overtake it.
            XCTAssertEqual(statistics.framesAccepted, 2)
            XCTAssertEqual(statistics.framesDeferred, 0)

            let pixel = try readPixel(session: harness.session, x: 0, y: 0)
            XCTAssertEqual(pixel, [255, 0, 0, 255], "the later, red frame is not the one on the surface")
        }

        /// Content drives its own frame loop: it asks for a frame, draws on the
        /// tick, and asks again -- the loop every game is.
        ///
        /// This is what the frame clock crossing the process boundary has to
        /// carry. Each request is a control message on the socket, which the
        /// engine arms (the first one races the renderer's bring-up, and is held);
        /// each tick comes back through the waker, carrying the window, because
        /// nothing else would tell a producer whose last verdict said zero that a
        /// credit came back. Without that the loop stops after two frames.
        ///
        /// Each tick sends a pair: a frame above the socket ceiling, then an
        /// ordinary one, as a texture-heavy frame and the next would go. When the
        /// window says two, both leave together on different uplinks and the
        /// small one can arrive first. Ingress holds it and runs it second; the
        /// test requires that to have happened at least once, and that nothing was
        /// refused and the last frame drawn is the last one sent.
        func testContentRunsItsOwnFrameLoopAndFramesThatOvertakeAreRunInOrder() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { encodeFrame, SECTION_KIND_COMMAND_STREAM } from "/__migo/wire-frame-packet.mjs";
                import { MAGIC, STREAM_VERSION, OP_CLEAR, OP_CLEAR_COLOR } from "/__migo/render-opcodes.mjs";

                const scratch = new DataView(new ArrayBuffer(4));
                const bits = (v) => { scratch.setFloat32(0, v, true); return scratch.getUint32(0, true); };
                const header = (op, words) => ((words << 12) | op) >>> 0;

                function frame(sequence, rgba, clears) {
                  const words = [MAGIC, STREAM_VERSION,
                    header(OP_CLEAR_COLOR, 6), 1, bits(rgba[0]), bits(rgba[1]), bits(rgba[2]), bits(rgba[3])];
                  for (let i = 0; i < clears; i += 1) words.push(header(OP_CLEAR, 3), 1, 0x4000);
                  const stream = new Uint8Array(words.length * 4);
                  const view = new DataView(stream.buffer);
                  words.forEach((w, i) => view.setUint32(i * 4, w, true));
                  return encodeFrame({ launchNonce: 0xa3n, sequence, runtimeGeneration: 1n,
                    surfaceGeneration: 1n, resourceEpoch: 0n,
                    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }] });
                }

                const TICKS = 40;
                export function start({ session }) {
                  let sequence = 0n;
                  let ticks = 0;
                  let sent = 0;
                  let bothInOneTick = 0;
                  let waiting = [];
                  const loop = () => {
                    ticks += 1;
                    if (waiting.length === 0 && ticks <= TICKS) {
                      // 6,000 clears is 72 KiB: above the socket ceiling. Blue, then red.
                      waiting = [frame(++sequence, [0, 0, 1, 1], 6000), frame(++sequence, [1, 0, 0, 1], 1)];
                    }
                    // In order: once one is held back, everything after it is too.
                    let went = 0;
                    while (waiting.length > 0 && session.submit(waiting[0]) === true) {
                      waiting.shift();
                      went += 1;
                    }
                    sent += went;
                    if (went === 2) bothInOneTick += 1;
                    if (ticks < TICKS || waiting.length > 0) {
                      session.requestFrame(1n, loop);
                    } else {
                      self.postMessage({ type: "looped", ticks, sent, bothInOneTick,
                        lastSequence: Number(sequence) });
                    }
                  };
                  session.requestFrame(1n, loop);
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            let looped = expectation(description: "content ran its frame loop to the end")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "looped":
                    report = message
                    looped.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    looped.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            // 240 s for WebContent's own start on a starved runner, as above. A loop
            // that stalls -- a request dropped, a tick never sent, a window never
            // reopened -- runs out this clock rather than finishing.
            wait(for: [looped], timeout: 240)
            XCTAssertNil(failure)

            let ticks = report?["ticks"] as? Int ?? 0
            let sent = report?["sent"] as? Int ?? 0
            let lastSequence = report?["lastSequence"] as? Int ?? 0
            XCTAssertGreaterThanOrEqual(ticks, 10, "the loop has to run at least ten frames")
            XCTAssertEqual(sent, lastSequence, "every frame content made was sent")
            XCTAssertGreaterThanOrEqual(sent, 20)
            XCTAssertGreaterThan(
                report?["bothInOneTick"] as? Int ?? 0, 0,
                "the window never said two, so no pair left together and nothing could overtake")

            let pollDeadline = Date().addingTimeInterval(30)
            while host.channel.currentStatistics.framesReceived < sent, Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            let statistics = host.channel.currentStatistics
            // The run's own numbers, in the log, so a pass can be cited rather
            // than inferred: how many pairs left together and how many overtook.
            print(
                "frame loop: ticks=\(ticks) sent=\(sent)"
                    + " bothInOneTick=\(report?["bothInOneTick"] as? Int ?? 0)"
                    + " accepted=\(statistics.framesAccepted) deferred=\(statistics.framesDeferred)"
                    + " refused=\(statistics.framesRefused) requests=\(statistics.controlMessagesReceived)"
                    + " wakes=\(statistics.downlinkWakes)")
            XCTAssertEqual(statistics.framesReceived, sent)
            XCTAssertEqual(
                statistics.framesRefused, 0,
                "a frame was refused: a producer that follows the window is never told to wait,"
                    + " and a gap ends the content")
            XCTAssertEqual(statistics.framesAccepted + statistics.framesDeferred, sent)
            XCTAssertGreaterThan(
                statistics.framesDeferred, 0,
                "no frame overtook its predecessor, so the held-frame path went unexercised")
            XCTAssertGreaterThanOrEqual(statistics.controlMessagesReceived, ticks)
            XCTAssertEqual(statistics.controlMessagesRefused, 0)
            XCTAssertGreaterThan(statistics.downlinkWakes, 0, "ticks reached the producer by being woken")

            let pixel = try readPixel(session: harness.session, x: 0, y: 0, triggeringSequence: UInt64(sent))
            XCTAssertEqual(
                pixel, [255, 0, 0, 255],
                "the last frame sent is red; blue is its predecessor run after it")
        }

        /// Content draws through the engine's own API layer, not the frame channel.
        ///
        /// Every test above has content build packets itself. A game does not: it
        /// calls `migo.createCanvas().getContext("webgl")` and draws in
        /// `requestAnimationFrame`, and on every other Migo platform that is the
        /// engine's JavaScript -- its WebGL facade and frame loop -- running beside
        /// the engine. Here the same modules run in WebContent, staged by the SDK
        /// build, and this is the first test in which nothing in content knows a
        /// wire format exists. Three frames: blue, blue, red; the surface has to
        /// show the last one.
        func testContentDrawsThroughTheEnginesOwnWebGLFacade() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                // `report`, not `self.postMessage`: with the engine loaded that global
                // is the mini-game one, as it is for a game on every other platform.
                export function start({ report }) {
                  const canvas = migo.createCanvas();
                  const gl = canvas.getContext("webgl");
                  let frames = 0;
                  const draw = () => {
                    frames += 1;
                    gl.clearColor(frames < 3 ? 0 : 1, 0, frames < 3 ? 1 : 0, 1);
                    gl.clear(gl.COLOR_BUFFER_BIT);
                    if (frames < 3) {
                      requestAnimationFrame(draw);
                    } else {
                      // After this callback returns, the frame loop ends the frame.
                      setTimeout(() => report({ type: "drawn", frames,
                        width: canvas.width, height: canvas.height }), 0);
                    }
                  };
                  requestAnimationFrame(draw);
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let drawn = expectation(description: "content drew three frames through the engine")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: contentRoot, contentEntry: "/game/main.mjs",
                    engineSession: .init(
                        launchNonce: nonce, surfaceGeneration: MigoFrameHarness.fixtureGeneration,
                        surfaceWidthPixels: harness.sizePixels, surfaceHeightPixels: harness.sizePixels)),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "drawn":
                    report = message
                    drawn.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    drawn.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [drawn], timeout: 240)
            XCTAssertNil(failure)
            XCTAssertEqual(report?["frames"] as? Int, 3)
            XCTAssertEqual(report?["width"] as? Int, harness.sizePixels, "the main canvas is the surface")

            let pollDeadline = Date().addingTimeInterval(30)
            while host.channel.currentStatistics.framesAccepted < 3, Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            let statistics = host.channel.currentStatistics
            print(
                "engine facade: accepted=\(statistics.framesAccepted) refused=\(statistics.framesRefused)"
                    + " requests=\(statistics.controlMessagesReceived) wakes=\(statistics.downlinkWakes)")
            XCTAssertEqual(statistics.framesAccepted, 3, "one packet per frame the engine's loop ended")
            XCTAssertEqual(statistics.framesRefused, 0)
            XCTAssertEqual(statistics.controlMessagesRefused, 0)

            let pixel = try readPixel(session: harness.session, x: 0, y: 0, triggeringSequence: 3)
            XCTAssertEqual(pixel, [255, 0, 0, 255], "the third frame, drawn by the engine's WebGL facade, is red")
        }

        /// A frame too large for one packet crosses as barriers and one present.
        ///
        /// Sixty thousand clears in one `requestAnimationFrame` is 256 KiB of
        /// wire but over 8 MiB of decoded GL commands against the host's 4 MiB
        /// budget, so the engine's frames have to be split -- and a split the
        /// host refused would end the content. The last clear is red; every
        /// packet but the last must be a barrier, and nothing may be refused.
        func testAFrameLargerThanOnePacketCrossesAsBarriersAndPresentsOnce() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { frameStatistics, lastSequence } from "/__migo/engine-frames.mjs";

                export function start({ report }) {
                  const gl = migo.createCanvas().getContext("webgl");
                  requestAnimationFrame(() => {
                    gl.clearColor(0, 0, 1, 1);
                    for (let i = 0; i < 60000; i += 1) gl.clear(gl.COLOR_BUFFER_BIT);
                    gl.clearColor(1, 0, 0, 1);
                    gl.clear(gl.COLOR_BUFFER_BIT);
                    // After this callback returns, the frame loop ends the frame.
                    setTimeout(() => report({ type: "split", sequence: lastSequence(),
                      ...frameStatistics() }), 0);
                  });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let split = expectation(description: "content drew a frame larger than one packet")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: contentRoot, contentEntry: "/game/main.mjs",
                    engineSession: .init(
                        launchNonce: nonce, surfaceGeneration: MigoFrameHarness.fixtureGeneration,
                        surfaceWidthPixels: harness.sizePixels, surfaceHeightPixels: harness.sizePixels)),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "split":
                    report = message
                    split.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    split.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [split], timeout: 240)
            XCTAssertNil(failure)

            let packets = report?["packets"] as? Int ?? 0
            let barriers = report?["barriers"] as? Int ?? 0
            let sequence = report?["sequence"] as? Int ?? 0
            let pollDeadline = Date().addingTimeInterval(60)
            while host.channel.currentStatistics.framesAccepted < packets, Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            let statistics = host.channel.currentStatistics
            print(
                "split frame: packets=\(packets) barriers=\(barriers)"
                    + " windowWaits=\(report?["windowWaits"] as? Int ?? -1)"
                    + " accepted=\(statistics.framesAccepted) refused=\(statistics.framesRefused)"
                    + " syncCalls=\(statistics.syncCallsAnswered)")
            XCTAssertGreaterThanOrEqual(barriers, 1, "the frame was not split")
            XCTAssertEqual(packets, barriers + 1, "one packet presents, and only one")
            XCTAssertEqual(sequence, packets)
            XCTAssertEqual(statistics.framesAccepted, packets)
            XCTAssertEqual(statistics.framesRefused, 0, "the host refused a packet the producer split")

            let pixel = try readPixel(
                session: harness.session, x: 0, y: 0, triggeringSequence: UInt64(sequence))
            XCTAssertEqual(pixel, [255, 0, 0, 255], "the frame's last clear is red")
        }

        /// A barrier that leaves on the socket, then a synchronous call naming it.
        ///
        /// The call blocks the Worker in a synchronous request, and the barrier
        /// is a WebSocket message sent a moment before -- the one combination the
        /// earlier tests avoided by making their frames large enough for the
        /// scheme. If WebKit held the socket message until the Worker unblocked,
        /// the host would wait for a packet that cannot arrive and the call would
        /// run out its timeout. The elapsed time is reported as evidence either
        /// way.
        func testASynchronousCallSeesTheBarrierSentOnTheSocketJustBeforeIt() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { frameStatistics, lastSequence } from "/__migo/engine-frames.mjs";
                import { encodeReadPixelsParams, SYNC_OP_READ_PIXELS } from "/__migo/sync-mailbox.mjs";

                export function start({ sync, report }) {
                  const gl = migo.createCanvas().getContext("webgl");
                  gl.clearColor(0, 0, 1, 1);
                  gl.clear(gl.COLOR_BUFFER_BIT);
                  // The engine's own barrier: flushes the facade's buffer and sends
                  // what is recorded without presenting. A few dozen bytes, so it
                  // leaves on the socket.
                  gl.flush();
                  const sequence = lastSequence();
                  const started = Date.now();
                  let detail;
                  try {
                    const pixel = sync.call({
                      runtimeGeneration: 1n, surfaceGeneration: 1n, resourceEpoch: 0n,
                      triggeringSequence: BigInt(sequence), operation: SYNC_OP_READ_PIXELS,
                      maxReplyBytes: 4, timeoutMillis: 30000,
                      params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 1, height: 1 }),
                    }, new Uint8Array(4));
                    detail = Array.from(pixel).join(",");
                  } catch (error) {
                    detail = `${error.name}: ${error.message}`;
                  }
                  report({ type: "read", detail, sequence, elapsedMillis: Date.now() - started,
                    ...frameStatistics() });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let read = expectation(description: "content read back through its barrier")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: contentRoot, contentEntry: "/game/main.mjs",
                    engineSession: .init(
                        launchNonce: nonce, surfaceGeneration: MigoFrameHarness.fixtureGeneration,
                        surfaceWidthPixels: harness.sizePixels, surfaceHeightPixels: harness.sizePixels)),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "read":
                    report = message
                    read.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    read.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [read], timeout: 240)
            XCTAssertNil(failure)
            print(
                "socket barrier then sync read: elapsed=\(report?["elapsedMillis"] as? Int ?? -1)ms"
                    + " sequence=\(report?["sequence"] as? Int ?? -1)"
                    + " barriers=\(report?["barriers"] as? Int ?? -1)"
                    + " detail=\(report?["detail"] as? String ?? "?")")
            XCTAssertEqual(report?["barriers"] as? Int, 1)
            XCTAssertEqual(
                report?["detail"] as? String, "0,0,255,255",
                "the read did not see the blue the barrier carried; a TimedOut error means the socket barrier never arrived while the Worker was blocked")
            XCTAssertEqual(host.channel.currentStatistics.framesRefused, 0)
        }

        /// The read content makes itself, from its own Worker, sees the frame it
        /// submitted.
        ///
        /// Every test above reads through the C ABI from the test's thread. This
        /// one is the lane's actual readback: content calls `sync.call` in the
        /// module worker, which blocks in a synchronous request to the content
        /// origin -- there is no SharedArrayBuffer on a custom scheme to block on
        /// instead -- and the answer body comes back to the Worker.
        ///
        /// The frame goes over the SCHEME uplink and the call leaves right after
        /// it, so the two are separate requests that nothing orders: the engine
        /// has to hold the read until the frame it names is admitted. A read that
        /// overtook it would see the cleared-to-nothing surface rather than blue.
        func testContentReadsBackTheFrameItSubmittedThroughItsOwnSynchronousCall() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { encodeFrame, SECTION_KIND_COMMAND_STREAM } from "/__migo/wire-frame-packet.mjs";
                import { MAGIC, STREAM_VERSION, OP_CLEAR, OP_CLEAR_COLOR } from "/__migo/render-opcodes.mjs";
                import { encodeReadPixelsParams, SYNC_OP_READ_PIXELS } from "/__migo/sync-mailbox.mjs";

                const scratch = new DataView(new ArrayBuffer(4));
                const bits = (v) => { scratch.setFloat32(0, v, true); return scratch.getUint32(0, true); };
                const header = (op, words) => ((words << 12) | op) >>> 0;

                export function start({ session, sync }) {
                  if (!sync) {
                    self.postMessage({ type: "read", detail: "the host injected no sync endpoint" });
                    return;
                  }
                  // 6,000 clears is 72 KiB of stream: above the socket ceiling, so
                  // this frame is a scheme request of its own.
                  const words = [MAGIC, STREAM_VERSION, header(OP_CLEAR_COLOR, 6), 1,
                    bits(0), bits(0), bits(1), bits(1)];
                  for (let i = 0; i < 6000; i += 1) words.push(header(OP_CLEAR, 3), 1, 0x4000);
                  const stream = new Uint8Array(words.length * 4);
                  const view = new DataView(stream.buffer);
                  words.forEach((w, i) => view.setUint32(i * 4, w, true));
                  const packet = encodeFrame({ launchNonce: 0xa3n, sequence: 1n, runtimeGeneration: 1n,
                    surfaceGeneration: 1n, resourceEpoch: 0n,
                    sections: [{ kind: SECTION_KIND_COMMAND_STREAM, payload: stream }] });
                  const submitted = session.submit(packet);

                  const into = new Uint8Array(4);
                  let detail;
                  try {
                    const pixel = sync.call({
                      runtimeGeneration: 1n, surfaceGeneration: 1n, resourceEpoch: 0n,
                      triggeringSequence: 1n, operation: SYNC_OP_READ_PIXELS, maxReplyBytes: 4,
                      // A minute, for the reason the Swift reads in this file give:
                      // the first readback in a process pays ANGLE's bring-up.
                      timeoutMillis: 60000,
                      params: encodeReadPixelsParams({ canvasId: 1, x: 0, y: 0, width: 1, height: 1 }),
                    }, into);
                    detail = Array.from(pixel).join(",");
                  } catch (error) {
                    detail = `${error.name}: ${error.message}`;
                  }
                  self.postMessage({ type: "read", submitted: String(submitted), bytes: packet.length, detail });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            let read = expectation(description: "content read its frame back")
            var detail: String?
            var submitted: String?
            var bytes: Int?
            let host = try MigoPerformancePlusHost(
                configuration: .init(contentRoot: contentRoot, contentEntry: "/game/main.mjs"),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { report in
                switch report["type"] as? String {
                case "read":
                    detail = report["detail"] as? String
                    submitted = report["submitted"] as? String
                    bytes = report["bytes"] as? Int
                    read.fulfill()
                case "failed":
                    detail = "failed at \(report["stage"] as? String ?? "?"): \(report["detail"] as? String ?? "?")"
                    read.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [read], timeout: 240)

            XCTAssertEqual(submitted, "true", "content's submit did not go out")
            XCTAssertGreaterThan(
                bytes ?? 0, MigoFrameChannelPolicy.socketCeilingBytes,
                "the frame has to be above the socket ceiling or the read and the frame share a stream")
            XCTAssertEqual(
                detail, "0,0,255,255",
                """
                content's own synchronous read did not see the blue frame it submitted. \
                [0,0,0,0] or the clear colour of an empty surface is a read that overtook its \
                frame; a SyncTransportError is the endpoint, a SyncRequestError is the engine's verdict.
                """)
            XCTAssertEqual(host.originActivity.syncCallsAnswered, 1)
            XCTAssertEqual(host.originActivity.syncCallsRefused, 0)
            XCTAssertEqual(host.channel.currentStatistics.syncCallsAnswered, 1)
            XCTAssertEqual(host.channel.currentStatistics.framesRefused, 0)
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

        /// One pixel, through the synchronous barrier, once the frame numbered
        /// `triggeringSequence` has been admitted.
        private func readPixel(
            session: OpaquePointer, x: Int32, y: Int32, triggeringSequence: UInt64 = 1
        ) throws -> [UInt8] {
            var request = MigoSyncRequestDescriptor()
            request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
            request.abi_version = MIGO_ABI_VERSION_CURRENT
            request.runtime_generation = 1
            request.surface_generation = MigoFrameHarness.fixtureGeneration
            request.resource_epoch = 0
            request.triggering_sequence = triggeringSequence
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
