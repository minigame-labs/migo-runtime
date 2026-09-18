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

        /// Content draws a textured triangle through the engine's WebGL facade:
        /// shaders compiled and linked, a vertex buffer and a texture uploaded,
        /// all as resource records the host decodes.
        ///
        /// The texture is two texels, green then magenta, drawn with nearest
        /// filtering across the whole surface, so the left half must read green
        /// and the right half magenta. A shader that did not compile, a program
        /// that did not link, an attribute that was not bound, a buffer or texel
        /// that did not upload -- each reads as the blue clear instead.
        func testContentDrawsATexturedTriangleThroughTheEnginesWebGLFacade() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                import { lastSequence } from "/__migo/engine-frames.mjs";

                export function start({ report }) {
                  const gl = migo.createCanvas().getContext("webgl");
                  const vertex = gl.createShader(gl.VERTEX_SHADER);
                  gl.shaderSource(vertex, "attribute vec2 p; varying vec2 uv; " +
                    "void main() { uv = p * 0.5 + 0.5; gl_Position = vec4(p, 0.0, 1.0); }");
                  gl.compileShader(vertex);
                  const fragment = gl.createShader(gl.FRAGMENT_SHADER);
                  gl.shaderSource(fragment, "precision mediump float; varying vec2 uv; " +
                    "uniform sampler2D t; void main() { gl_FragColor = texture2D(t, uv); }");
                  gl.compileShader(fragment);
                  const program = gl.createProgram();
                  gl.attachShader(program, vertex);
                  gl.attachShader(program, fragment);
                  gl.bindAttribLocation(program, 0, "p");
                  gl.linkProgram(program);

                  const buffer = gl.createBuffer();
                  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
                  // One triangle that covers the whole surface.
                  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);

                  const texture = gl.createTexture();
                  gl.bindTexture(gl.TEXTURE_2D, texture);
                  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 2, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE,
                    new Uint8Array([0, 255, 0, 255, 255, 0, 255, 255]));
                  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
                  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
                  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
                  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

                  requestAnimationFrame(() => {
                    gl.clearColor(0, 0, 1, 1);
                    gl.clear(gl.COLOR_BUFFER_BIT);
                    gl.useProgram(program);
                    gl.enableVertexAttribArray(0);
                    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
                    gl.drawArrays(gl.TRIANGLES, 0, 3);
                    setTimeout(() => report({ type: "drawn", sequence: lastSequence() }), 0);
                  });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let drawn = expectation(description: "content drew a textured triangle through the engine")
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
            let sequence = UInt64(report?["sequence"] as? Int ?? 0)
            XCTAssertGreaterThan(sequence, 0)

            let pollDeadline = Date().addingTimeInterval(60)
            while host.channel.currentStatistics.framesAccepted < Int(sequence), Date() < pollDeadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            XCTAssertEqual(host.channel.currentStatistics.framesRefused, 0)

            let quarter = Int32(harness.sizePixels / 4)
            let middle = Int32(harness.sizePixels / 2)
            let left = try readPixel(
                session: harness.session, x: quarter, y: middle, triggeringSequence: sequence)
            let right = try readPixel(
                session: harness.session, x: 3 * quarter, y: middle, triggeringSequence: sequence)
            print("textured triangle: left=\(left) right=\(right) sequence=\(sequence)")
            XCTAssertEqual(left, [0, 255, 0, 255], "the left half samples the green texel")
            XCTAssertEqual(right, [255, 0, 255, 255], "the right half samples the magenta texel")
        }

        /// The queries: a compile status, a link status, a uniform location, an
        /// error code -- each answered after the frame it is about.
        ///
        /// The acceptance for D15.3c. Every one of these is a call whose return
        /// value IS the answer, and each asks about work that is *records in the
        /// frame being built*: the shader whose `COMPILE_STATUS` content wants
        /// was given its source three records ago. So the producer sends what it
        /// has recorded as a barrier -- executed, not presented, or the half
        /// frame would flash onto the screen -- and blocks until the host has
        /// run it. Three.js does exactly this the first time it uses a material.
        ///
        /// The pixels are what prove the location is right: the fragment shader
        /// has one uniform and nothing else decides its colour, so a location
        /// that came back wrong draws black.
        func testContentQueriesTheEngineAndDrawsWithWhatItLearns() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                export function start({ report }) {
                  const gl = migo.createCanvas().getContext("webgl");
                  const stage = (name, run) => {
                    try { return run(); }
                    catch (error) { report({ type: "failed", stage: name, detail: `${error.name}: ${error.message}` }); throw error; }
                  };

                  const answers = stage("program", () => {
                    const vertex = gl.createShader(gl.VERTEX_SHADER);
                    gl.shaderSource(vertex, "attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }");
                    gl.compileShader(vertex);
                    const fragment = gl.createShader(gl.FRAGMENT_SHADER);
                    gl.shaderSource(fragment, "precision mediump float; uniform vec4 uColor; " +
                      "void main() { gl_FragColor = uColor; }");
                    gl.compileShader(fragment);
                    const program = gl.createProgram();
                    gl.attachShader(program, vertex);
                    gl.attachShader(program, fragment);
                    gl.bindAttribLocation(program, 0, "p");
                    gl.linkProgram(program);
                    return {
                      // Each of these crosses as a barrier and a blocking call.
                      vertexCompiled: gl.getShaderParameter(vertex, gl.COMPILE_STATUS) === true,
                      fragmentCompiled: gl.getShaderParameter(fragment, gl.COMPILE_STATUS) === true,
                      linked: gl.getProgramParameter(program, gl.LINK_STATUS) === true,
                      // An empty log is the normal answer for a program that
                      // linked; what matters is that it came back as a string.
                      logIsText: typeof gl.getProgramInfoLog(program) === "string",
                      attributes: gl.getProgramParameter(program, gl.ACTIVE_ATTRIBUTES),
                      activeUniform: (gl.getActiveUniform(program, 0) || {}).name,
                      program,
                    };
                  });

                  stage("draw", () => {
                    const buffer = gl.createBuffer();
                    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
                    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
                    gl.useProgram(answers.program);
                    // The location content asks for, used to colour the frame.
                    const location = gl.getUniformLocation(answers.program, "uColor");
                    answers.locationFound = location !== null && location !== -1;
                    gl.uniform4f(location, 0, 1, 0, 1);
                    gl.enableVertexAttribArray(0);
                    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
                    gl.viewport(0, 0, 64, 64);
                    gl.clearColor(0, 0, 1, 1);
                    gl.clear(gl.COLOR_BUFFER_BIT);
                    gl.drawArrays(gl.TRIANGLES, 0, 3);
                  });

                  // The error queue is the host's, filled while it decoded this
                  // producer's own records: a negative offset is INVALID_VALUE
                  // there, and this is how content reads it back.
                  stage("error", () => {
                    answers.errorBeforeMistake = gl.getError();
                    gl.bufferSubData(gl.ARRAY_BUFFER, -4, new Float32Array([1, 2]));
                    answers.errorAfterMistake = gl.getError();
                    answers.errorDrained = gl.getError();
                  });

                  requestAnimationFrame(() => {
                    setTimeout(() => report({ type: "queried", ...answers, program: undefined }), 0);
                  });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let queried = expectation(description: "content asked the engine and drew")
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
                case "queried":
                    report = message
                    queried.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    queried.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [queried], timeout: 240)
            XCTAssertNil(failure)

            XCTAssertEqual(report?["vertexCompiled"] as? Bool, true, "the vertex shader compiled")
            XCTAssertEqual(report?["fragmentCompiled"] as? Bool, true, "the fragment shader compiled")
            XCTAssertEqual(report?["linked"] as? Bool, true, "the program linked")
            XCTAssertEqual(report?["logIsText"] as? Bool, true, "the info log came back as text")
            XCTAssertEqual(report?["attributes"] as? Int, 1, "the program has one active attribute")
            XCTAssertEqual(report?["activeUniform"] as? String, "uColor", "and one active uniform, by name")
            XCTAssertEqual(report?["locationFound"] as? Bool, true, "the uniform's location came back")
            XCTAssertEqual(report?["errorBeforeMistake"] as? Int, 0, "no error before the mistake")
            XCTAssertEqual(
                report?["errorAfterMistake"] as? Int, 0x0501,
                "a negative bufferSubData offset is INVALID_VALUE, recorded where the record was decoded")
            XCTAssertEqual(report?["errorDrained"] as? Int, 0, "and the queue drains, one error per call")

            let pixel = try readPixel(session: harness.session, x: 0, y: 0, triggeringSequence: 1)
            print("queries: pixel=\(pixel) attributes=\(report?["attributes"] as? Int ?? -1)")
            XCTAssertEqual(pixel, [0, 255, 0, 255], "the frame is the colour the located uniform was set to")
        }

        /// Text, drawn by the engine's own 2D context.
        ///
        /// The acceptance for D15.4a. `ctx.font = "..."` is answered by the
        /// producer -- it parses the shorthand itself, because the assignment
        /// returns whether it parsed and the host's answer is a frame away --
        /// and the string, the coordinates and the alignment cross as records
        /// the host decodes into the commands its own ops build.
        ///
        /// WHAT THE PIXELS PROVE. Not which pixels a glyph fills: that is the
        /// font's business, and a test that named them would be a test of the
        /// rasteriser. The canvas is cleared to blue and the text is drawn in
        /// green, so what is asserted is that green ink arrived inside the box
        /// the call named, that none arrived outside it, and that a shorthand
        /// the parser refuses draws nothing at all.
        func testContentDrawsTextThroughTheEngines2DContext() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                export function start({ report }) {
                  const canvas = migo.createCanvas();
                  const ctx = canvas.getContext("2d");
                  const answers = {};
                  const stage = (name, run) => {
                    try { return run(); }
                    catch (error) { report({ type: "failed", stage: name, detail: `${error.name}: ${error.message}` }); throw error; }
                  };

                  stage("draw", () => {
                    ctx.fillStyle = "#0000ff";
                    ctx.fillRect(0, 0, canvas.width, canvas.height);

                    // A shorthand with no size: refused by the parser here, and
                    // the font stays what it was -- which is what the assignment
                    // answers, and the only thing content can observe about it.
                    answers.refusedFont = ctx.font;
                    ctx.font = "not-a-font";
                    answers.fontAfterRefusal = ctx.font;

                    ctx.font = "48px sans-serif";
                    answers.fontApplied = ctx.font;
                    ctx.textAlign = "left";
                    ctx.textBaseline = "top";
                    ctx.fillStyle = "#00ff00";
                    // The measurement crosses as a barrier and a blocked call,
                    // and it is of the font two records ago: a host that
                    // answered before applying it would measure at the default
                    // size, which is half of this.
                    const measured = ctx.measureText("ABC");
                    answers.measuredWidth = measured.width;
                    answers.measuredNarrower = ctx.measureText("A").width;
                    // Large enough that any face puts ink in the top-left
                    // quadrant, and placed so the bottom rows stay untouched.
                    ctx.fillText("ABC", 0, 0);
                  });

                  requestAnimationFrame(() => {
                    setTimeout(() => report({ type: "drew", ...answers,
                      width: canvas.width, height: canvas.height }), 0);
                  });
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let drew = expectation(description: "content drew text")
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
                case "drew":
                    report = message
                    drew.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    drew.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [drew], timeout: 240)
            XCTAssertNil(failure)

            // The refused shorthand left the font alone, which is the whole of
            // what `ctx.font =` answers.
            XCTAssertEqual(
                report?["fontAfterRefusal"] as? String, report?["refusedFont"] as? String,
                "a shorthand with no size is a no-op, as it is in a browser")
            XCTAssertEqual(report?["fontApplied"] as? String, "48px sans-serif")

            // The measurement is the renderer's, of the font the barrier
            // applied. Its exact value is the face's business; that it is a
            // plausible width for three glyphs at 48 px, and that one glyph
            // measures narrower than three, is the query working.
            let width = report?["measuredWidth"] as? Double ?? 0
            let narrower = report?["measuredNarrower"] as? Double ?? 0
            print("2D measure: ABC=\(width) A=\(narrower)")
            XCTAssertGreaterThan(width, 24, "three glyphs at 48px are wider than that")
            XCTAssertLessThan(width, 400)
            XCTAssertLessThan(narrower, width, "one glyph is narrower than three")
            XCTAssertGreaterThan(narrower, 0)

            let size = harness.sizePixels
            let inked = try readPixels(
                session: harness.session, x: 0, y: 0, width: Int32(size), height: Int32(size))
            var green = 0
            var blue = 0
            var other = 0
            for index in stride(from: 0, to: inked.count, by: 4) {
                let pixel = Array(inked[index..<index + 4])
                if pixel == [0, 255, 0, 255] { green += 1 } else if pixel == [0, 0, 255, 255] {
                    blue += 1
                } else {
                    // Antialiased edges: between the two colours, alpha opaque.
                    other += 1
                }
            }
            print("2D text: green=\(green) blue=\(blue) other=\(other) of \(size * size)")
            XCTAssertGreaterThan(green, 20, "the text put ink on the canvas")
            XCTAssertGreaterThan(blue, 200, "and did not cover the whole of it")
        }

        /// What a game reads before it draws: the screen, the pixel ratio, the
        /// safe area, the model.
        ///
        /// Every one of these is synchronous and none of it changes during a
        /// session, so the host hands the JSON to the producer at startup and
        /// the calls are answered there without crossing. What this checks is
        /// that the numbers content reads are the ones the host described --
        /// through the engine's own `migo.*` layer, which parses them and
        /// converts the safe area from insets to positions.
        func testContentReadsTheDeviceTheHostDescribed() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            try Data(
                """
                export function start({ report }) {
                  try {
                    const window = migo.getWindowInfo();
                    const system = migo.getSystemInfoSync();
                    report({ type: "described",
                      screenWidth: window.screenWidth, screenHeight: window.screenHeight,
                      pixelRatio: window.pixelRatio, statusBarHeight: window.statusBarHeight,
                      safeTop: window.safeArea.top, safeBottom: window.safeArea.bottom,
                      model: system.model, platform: system.platform });
                  } catch (error) {
                    report({ type: "failed", stage: "device", detail: `${error.name}: ${error.message}` });
                  }
                }
                """.utf8
            ).write(to: contentRoot.appendingPathComponent("game/main.mjs"))

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let described = expectation(description: "content read the device")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            typealias Profile = MigoPerformancePlusHost.DeviceProfile
            let profile = Profile(
                screenWidth: 390, screenHeight: 844, windowWidth: 390, windowHeight: 844,
                pixelRatio: 3, statusBarHeight: 47,
                safeAreaInsets: Profile.SafeAreaInsets(left: 0, top: 47, right: 0, bottom: 34),
                brand: "Apple", model: "iPhone14,5", system: "iOS 26.0", platform: "ios")
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: contentRoot, contentEntry: "/game/main.mjs",
                    engineSession: .init(
                        launchNonce: nonce, surfaceGeneration: MigoFrameHarness.fixtureGeneration,
                        surfaceWidthPixels: harness.sizePixels, surfaceHeightPixels: harness.sizePixels,
                        device: profile)),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "described":
                    report = message
                    described.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    described.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [described], timeout: 240)
            XCTAssertNil(failure)

            XCTAssertEqual(report?["screenWidth"] as? Double, 390)
            XCTAssertEqual(report?["screenHeight"] as? Double, 844)
            XCTAssertEqual(report?["pixelRatio"] as? Double, 3)
            XCTAssertEqual(report?["statusBarHeight"] as? Double, 47)
            // Insets in, positions out: the engine's own conversion, which is
            // the reason these cross as insets rather than as positions.
            XCTAssertEqual(report?["safeTop"] as? Double, 47)
            XCTAssertEqual(report?["safeBottom"] as? Double, 844 - 34)
            XCTAssertEqual(report?["model"] as? String, "iPhone14,5")
            XCTAssertEqual(report?["platform"] as? String, "ios")
        }

        /// The engine's storage API, answered by the host's storage service.
        ///
        /// The whole service stream in one test: content is installed and
        /// loaded through the C ABI, the host serves the directory the engine
        /// mounted, and `migo.*Storage*` -- synchronous and awaited -- reaches the
        /// same SQLite store and the same rules the embedded runtime uses; an
        /// oversized value is refused with the message games match on.
        func testContentReachesTheHostsStorageThroughTheServiceStream() throws {
            let harness = try MigoFrameHarness()
            self.harness = harness
            let content = Data(
                """
                export async function start({ report }) {
                  try {
                    migo.setStorageSync("sync-key", { n: 1, s: "h\\u00e9llo" });
                    const syncBack = migo.getStorageSync("sync-key");
                    await migo.setStorage({ key: "async-key", data: 42 });
                    const syncOfAsync = migo.getStorageSync("async-key");
                    const asyncBack = (await migo.getStorage({ key: "async-key" })).data;
                    const info = migo.getStorageInfoSync();
                    let refused = null;
                    try {
                      migo.setStorageSync("big", "x".repeat(1024 * 1024 + 1));
                    } catch (error) {
                      refused = `${error.name}: ${error.message}`;
                    }
                    report({ type: "stored", syncBack: JSON.stringify(syncBack), syncOfAsync, asyncBack,
                      keys: info.keys.slice().sort().join(","), refused });
                  } catch (error) {
                    // An awaited mini-game API rejects with `{errMsg}`, not an Error.
                    const detail = error instanceof Error ? `${error.name}: ${error.message}` : JSON.stringify(error);
                    report({ type: "failed", stage: "storage", detail });
                  }
                }
                """.utf8)
            let root = try harness.installAndLoadContent(
                id: "storage-game", entry: "game/main.mjs", files: ["game/main.mjs": content])

            var nonce = [UInt8](repeating: 0, count: 16)
            nonce[0] = MigoFrameHarness.fixtureLaunchNonce
            let stored = expectation(description: "content used storage")
            var report: MigoPerformancePlusHost.Report?
            var failure: String?
            let host = try MigoPerformancePlusHost(
                configuration: .init(
                    contentRoot: root, contentEntry: "/game/main.mjs",
                    engineSession: .init(
                        launchNonce: nonce, surfaceGeneration: MigoFrameHarness.fixtureGeneration,
                        surfaceWidthPixels: harness.sizePixels, surfaceHeightPixels: harness.sizePixels)),
                channel: MigoFrameChannel(session: harness.session))
            self.host = host
            host.onReport = { message in
                switch message["type"] as? String {
                case "stored":
                    report = message
                    stored.fulfill()
                case "failed":
                    failure = "failed at \(message["stage"] as? String ?? "?"): \(message["detail"] as? String ?? "?")"
                    stored.fulfill()
                default: break
                }
            }
            mount(host)
            try host.start()
            wait(for: [stored], timeout: 240)
            XCTAssertNil(failure)
            XCTAssertEqual(report?["syncBack"] as? String, #"{"n":1,"s":"héllo"}"#)
            XCTAssertEqual(report?["syncOfAsync"] as? Double, 42, "one store behind both kinds of call")
            XCTAssertEqual(report?["asyncBack"] as? Double, 42)
            XCTAssertEqual(report?["keys"] as? String, "async-key,sync-key")
            XCTAssertEqual(
                report?["refused"] as? String, "Error: setStorage:fail data exceeds max size",
                "refused by the host's rule, with the message the embedded op throws")
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
        /// A rectangle of pixels, for a test whose question is "did anything get
        /// painted here" rather than "what colour is this pixel".
        ///
        /// Glyph coverage is the font's business: which pixels an `M` fills at
        /// 48 px depends on the face, the hinting and the rasteriser, and a test
        /// that named one of them would be a test of the font. What text drawing
        /// owes its caller is that the ink arrives, in the fill colour, inside
        /// the box the call named.
        private func readPixels(
            session: OpaquePointer, x: Int32, y: Int32, width: Int32, height: Int32,
            triggeringSequence: UInt64 = 1
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
            let expected = Int(width) * Int(height) * 4
            request.max_reply_bytes = UInt32(expected)

            var outcome = MigoSyncOutcome()
            outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
            outcome.abi_version = MIGO_ABI_VERSION_CURRENT

            let params = MigoFrameHarness.readPixelsParameters(
                x: x, y: y, width: width, height: height)
            let posted = params.withUnsafeBufferPointer { buffer in
                migo_session_post_sync_request(
                    session, &request, buffer.baseAddress, buffer.count, now, &outcome)
            }
            XCTAssertEqual(posted, MIGO_OK, "post")
            XCTAssertEqual(
                outcome.state, MIGO_SYNC_STATE_READY,
                "the readback failed with error \(outcome.error)")

            var pixels = [UInt8](repeating: 0, count: expected)
            var written = 0
            let taken = pixels.withUnsafeMutableBufferPointer { out in
                migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
            }
            XCTAssertEqual(taken, MIGO_OK, "take")
            XCTAssertEqual(written, expected, "the rectangle's RGBA8 rows")
            return pixels
        }

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
