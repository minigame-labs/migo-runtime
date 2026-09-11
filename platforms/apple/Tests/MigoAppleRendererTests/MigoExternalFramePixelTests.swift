import MigoEngine
import QuartzCore
import XCTest

#if os(macOS)
    private typealias LayerPayload = MigoMacosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER
#elseif os(iOS)
    private typealias LayerPayload = MigoIosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_IOS_CA_METAL_LAYER
#endif

/// A frame in, and the pixels it drew back out.
///
/// Every other test of this lane stops at a boundary. The ABI tests prove the
/// synchronous barrier refuses what it must; the surface tests prove a layer
/// attaches; the wire tests prove a packet validates. None of them establishes
/// the thing the product is: that drawing work produced somewhere else arrives
/// here, executes on the GPU, and can be read back as the pixels it asked for.
///
/// This does. It submits a packet built by the producer's own encoder -- clear
/// to opaque blue, present -- and then asks the barrier for those pixels
/// through `migo_session_post_sync_request`, which is the same path a blocked
/// `readPixels` in WebContent will take.
///
/// WHY IT NEEDS NO EXPLICIT SYNCHRONISATION. The frame and the readback are two
/// commands on one render-thread queue, and the queue is ordered. The clear has
/// executed before the readback runs because it was posted first, so there is
/// nothing here to wait for and nothing to poll -- which is worth stating,
/// because a test that slept instead would be measuring the sleep.
final class MigoExternalFramePixelTests: XCTestCase {

    private var root: URL!
    private var engine: OpaquePointer?
    private var session: OpaquePointer?
    private var attachment: OpaquePointer?
    private var retainedLayer: CAMetalLayer?

    /// The clock reading a host would pass in, and the deadline derived from it.
    ///
    /// A literal rather than a real reading, because only the difference between
    /// the two is used: the library reads no clock of its own for this, so
    /// `deadline - now` is the whole budget.
    ///
    /// 60 s, which is absurd for a readback and right for this lane. The first
    /// test to run in a process pays ANGLE's load and EGL bring-up before its
    /// readback can execute, and this bundle already waits 60 s elsewhere for
    /// the same reason -- measured on the iOS simulator, where a 5 s budget
    /// timed out on the first test and the second passed in a warm process. A
    /// production producer would name a far smaller one; a test that named a
    /// small one would be asserting how fast a starved runner is.
    private let now: UInt64 = 1_000_000_000
    private var deadline: UInt64 { now + 60_000_000_000 }

    /// The two committed frames and the colour each clears to, in the order
    /// `readPixels` returns it. `emit-clear-frame.mjs` exports the same table.
    ///
    /// Two, because one frame read back as the colour it cleared to is also
    /// satisfied by a readback that returns a constant -- and "the pixels
    /// happened to be what we expected" is the shape of green this repository
    /// keeps finding. Submitting both in one session and asserting the pixels
    /// CHANGED is what a constant cannot pass.
    private let frames: [(name: String, rgba: [UInt8])] = [
        ("clear-blue-frame", [0, 0, 255, 255]),
        ("clear-red-frame", [255, 0, 0, 255]),
    ]

    override func setUpWithError() throws {
        try super.setUpWithError()
        setenv("MIGO_CAPI_LOG", "info", 1)

        root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("migo-external-pixels-\(UUID().uuidString)")
        for name in ["files", "cache", "code-cache"] {
            try FileManager.default.createDirectory(
                at: root.appendingPathComponent(name), withIntermediateDirectories: true)
        }

        var created: OpaquePointer?
        try root.appendingPathComponent("files").path.withCString { files in
            try root.appendingPathComponent("cache").path.withCString { cache in
                try root.appendingPathComponent("code-cache").path.withCString { codeCache in
                    var config = MigoEngineConfig()
                    config.struct_size = UInt32(MemoryLayout<MigoEngineConfig>.size)
                    config.abi_version = MIGO_ABI_VERSION_CURRENT
                    config.files_dir_utf8 = files
                    config.cache_dir_utf8 = cache
                    config.code_cache_dir_utf8 = codeCache
                    XCTAssertEqual(migo_engine_create(&config, &created), MIGO_OK)
                }
            }
        }
        engine = try XCTUnwrap(created)

        var sessionConfig = MigoSessionConfig()
        sessionConfig.struct_size = UInt32(MemoryLayout<MigoSessionConfig>.size)
        sessionConfig.abi_version = MIGO_ABI_VERSION_CURRENT
        // 0xA3, which is the nonce the committed fixture is addressed to. A
        // packet whose nonce does not match is refused before anything looks at
        // its contents.
        withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { bytes in
            bytes.copyBytes(from: CollectionOfOne(UInt8(0xA3)))
        }
        var startedSession: OpaquePointer?
        XCTAssertEqual(migo_session_create(engine, &sessionConfig, &startedSession), MIGO_OK)
        session = try XCTUnwrap(startedSession)

        let layer = CAMetalLayer()
        layer.drawableSize = CGSize(width: 64, height: 64)
        layer.frame = CGRect(x: 0, y: 0, width: 64, height: 64)
        retainedLayer = layer

        var payload = LayerPayload()
        payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
        payload.abi_version = MIGO_ABI_VERSION_CURRENT
        payload.platform_kind = layerKind
        payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()
        let payloadSize = payload.struct_size

        var produced: OpaquePointer?
        let attachResult = withUnsafeMutablePointer(to: &payload) { raw -> MigoResult in
            var descriptor = MigoSurfaceDescriptor()
            descriptor.struct_size = UInt32(MemoryLayout<MigoSurfaceDescriptor>.size)
            descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
            // 1, which is the generation the fixture names.
            descriptor.generation = 1
            descriptor.platform_kind = layerKind
            descriptor.width_pixels = 64
            descriptor.height_pixels = 64
            descriptor.scale_factor = 1.0
            descriptor.color_space = MIGO_COLOR_SPACE_SRGB
            descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
            descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
            descriptor.platform_descriptor_size = payloadSize
            descriptor.platform_descriptor = UnsafeRawPointer(raw)
            return migo_session_attach_surface(startedSession, &descriptor, &produced)
        }
        XCTAssertEqual(attachResult, MIGO_OK)
        attachment = try XCTUnwrap(produced)
    }

    override func tearDownWithError() throws {
        if let live = attachment {
            var release: OpaquePointer?
            XCTAssertEqual(migo_surface_begin_detach(live, &release), MIGO_OK)
            attachment = nil
            if let observer = release {
                var status = MigoSurfaceReleaseStatus()
                status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
                status.abi_version = MIGO_ABI_VERSION_CURRENT
                let deadline = Date().addingTimeInterval(60)
                var released = false
                while Date() < deadline {
                    XCTAssertEqual(migo_surface_release_query(observer, &status), MIGO_OK)
                    if status.state == MIGO_SURFACE_RELEASE_RELEASED {
                        released = true
                        break
                    }
                    RunLoop.current.run(until: Date().addingTimeInterval(0.001))
                }
                XCTAssertTrue(released, "the retired surface never reported RELEASED")
            }
        }
        if let session {
            XCTAssertEqual(migo_session_destroy(session), MIGO_OK)
            self.session = nil
        }
        if let engine {
            XCTAssertEqual(migo_engine_destroy(engine), MIGO_OK)
            self.engine = nil
        }
        retainedLayer = nil
        try? FileManager.default.removeItem(at: root)
        try super.tearDownWithError()
    }

    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: name, withExtension: "bin", subdirectory: "Fixtures"),
            """
            the committed frame is not in the test bundle. It is declared as a resource of \
            this target in Package.swift; regenerate it with \
            `node platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs`.
            """)
        return try Data(contentsOf: url)
    }

    private func readPixelsParams(x: Int32, y: Int32, width: Int32, height: Int32) -> [UInt8] {
        var bytes: [UInt8] = []
        func word(_ value: UInt32) {
            withUnsafeBytes(of: value.littleEndian) { bytes.append(contentsOf: $0) }
        }
        word(1)  // canvas id, which is what the fixture's records name
        word(UInt32(bitPattern: x))
        word(UInt32(bitPattern: y))
        word(UInt32(bitPattern: width))
        word(UInt32(bitPattern: height))
        word(0x1908)  // GL_RGBA
        word(0x1401)  // GL_UNSIGNED_BYTE
        word(0)
        return bytes
    }

    /// Submit one frame, waiting for a credit if the window is full.
    ///
    /// `MIGO_FRAME_INGRESS_WOULD_BLOCK` is not a failure, it is the backpressure
    /// working: a credit is held from acceptance until the renderer has executed
    /// the frame, and a producer that ignored the answer would run the queue
    /// unbounded. Retrying is what a producer does, so it is what this does --
    /// and a test that submitted three frames without it reported "the frame was
    /// refused" for a session behaving exactly as designed.
    @discardableResult
    private func submit(_ name: String, sequence: UInt64) throws -> MigoFrameIngressOutcome {
        let session = try XCTUnwrap(self.session)
        let packet = try fixture(name)
        var ingress = MigoFrameIngressOutcome()
        ingress.struct_size = UInt32(MemoryLayout<MigoFrameIngressOutcome>.size)
        ingress.abi_version = MIGO_ABI_VERSION_CURRENT

        let waitUntil = Date().addingTimeInterval(30)
        while true {
            let sent = packet.withUnsafeBytes { raw -> MigoResult in
                migo_session_submit_external_frame(
                    session, raw.bindMemory(to: UInt8.self).baseAddress, raw.count, &ingress)
            }
            XCTAssertEqual(sent, MIGO_OK, "\(name): the call itself must succeed")
            if ingress.decision != MIGO_FRAME_INGRESS_WOULD_BLOCK { break }
            XCTAssertLessThan(
                Date(), waitUntil,
                "\(name): no credit came back in 30 s, so frames are not being executed")
            RunLoop.current.run(until: Date().addingTimeInterval(0.002))
        }

        XCTAssertEqual(
            ingress.decision, MIGO_FRAME_INGRESS_ACCEPTED,
            """
            \(name) was not accepted (decision \(ingress.decision), wire_error_code \
            \(ingress.wire_error_code)). That code names which field the ingress refused, and \
            frame-wire's clear_frame_fixture test asserts what each of them holds.
            """)
        XCTAssertEqual(ingress.accepted_sequence, sequence, "\(name): sequence")
        return ingress
    }

    /// Submit one frame and read the pixels it drew.
    private func submitAndRead(_ name: String, sequence: UInt64) throws -> [UInt8] {
        let session = try XCTUnwrap(self.session)
        try submit(name, sequence: sequence)

        var request = MigoSyncRequestDescriptor()
        request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
        request.abi_version = MIGO_ABI_VERSION_CURRENT
        request.runtime_generation = 1
        request.surface_generation = 1
        request.resource_epoch = 0
        request.triggering_sequence = sequence
        request.deadline_nanos = deadline
        request.operation = MIGO_SYNC_OP_READ_PIXELS
        // Two by two: enough that a wrong row stride shows up, small enough that
        // a mismatch prints legibly.
        request.max_reply_bytes = 2 * 2 * 4

        var outcome = MigoSyncOutcome()
        outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
        outcome.abi_version = MIGO_ABI_VERSION_CURRENT

        let params = readPixelsParams(x: 0, y: 0, width: 2, height: 2)
        let posted = params.withUnsafeBufferPointer { buffer in
            migo_session_post_sync_request(
                session, &request, buffer.baseAddress, buffer.count, now, &outcome)
        }
        XCTAssertEqual(posted, MIGO_OK, "\(name): post")
        XCTAssertEqual(
            outcome.state, MIGO_SYNC_STATE_READY,
            "\(name): the readback failed with error \(outcome.error)")
        XCTAssertEqual(outcome.reply_bytes, 2 * 2 * 4, "\(name): four RGBA8 pixels")

        // Sized from the request, not from the outcome. A readback that failed
        // reports zero bytes, and an array sized from that is one the caller
        // then indexes past -- which is how the first simulator failure ended:
        // three legible assertion messages followed by "Array index is out of
        // range", where the crash is the least informative line and the only one
        // that stops the run.
        var pixels = [UInt8](repeating: 0, count: 2 * 2 * 4)
        var written = 0
        let taken = pixels.withUnsafeMutableBufferPointer { out in
            migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
        }
        XCTAssertEqual(taken, MIGO_OK, "\(name): take")
        XCTAssertEqual(written, pixels.count, "\(name): written")
        return pixels
    }

    /// Read one pixel at a named point, through the barrier.
    private func readPixel(x: Int32, y: Int32, label: String) throws -> [UInt8] {
        let session = try XCTUnwrap(self.session)
        var request = MigoSyncRequestDescriptor()
        request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
        request.abi_version = MIGO_ABI_VERSION_CURRENT
        request.runtime_generation = 1
        request.surface_generation = 1
        request.resource_epoch = 0
        request.triggering_sequence = 3
        request.deadline_nanos = deadline
        request.operation = MIGO_SYNC_OP_READ_PIXELS
        request.max_reply_bytes = 4

        var outcome = MigoSyncOutcome()
        outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
        outcome.abi_version = MIGO_ABI_VERSION_CURRENT

        let params = readPixelsParams(x: x, y: y, width: 1, height: 1)
        let posted = params.withUnsafeBufferPointer { buffer in
            migo_session_post_sync_request(
                session, &request, buffer.baseAddress, buffer.count, now, &outcome)
        }
        XCTAssertEqual(posted, MIGO_OK, "\(label): post")
        XCTAssertEqual(
            outcome.state, MIGO_SYNC_STATE_READY,
            "\(label): the readback failed with error \(outcome.error)")

        var pixel = [UInt8](repeating: 0, count: 4)
        var written = 0
        let taken = pixel.withUnsafeMutableBufferPointer { out in
            migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
        }
        XCTAssertEqual(taken, MIGO_OK, "\(label): take")
        XCTAssertEqual(written, 4, "\(label): one RGBA8 pixel")
        return pixel
    }

    /// The readback's rectangle is honoured, not just its size.
    ///
    /// The two flat frames cannot establish this: every rectangle of a flat
    /// surface has the same bytes, so a readback that ignored x and y would
    /// return the right answer for the wrong reason. This frame clears the
    /// lower-left quadrant to red behind a scissor and leaves the rest blue, so
    /// two points give two answers -- which an ignored origin cannot produce.
    ///
    /// It also establishes that the records execute in the order they were
    /// written. A scissor applied after the second clear would paint the whole
    /// surface red, and the two points would then agree.
    ///
    /// GL's origin is bottom-left, so the scissor's (0,0) and readPixels' (0,0)
    /// are the same corner.
    func testTheReadbackHonoursItsRectangleAndTheRecordsRunInOrder() throws {
        // Sequences must be strictly contiguous, and this fixture is 3 -- so the
        // two flat frames go first. Each waits for a credit if the window is
        // full, which is what a producer does.
        for (index, frame) in frames.enumerated() {
            try submit(frame.name, sequence: UInt64(index + 1))
        }
        try submit("clear-scissor-frame", sequence: 3)

        let inside = try readPixel(x: 4, y: 4, label: "inside the scissor")
        let outside = try readPixel(x: 48, y: 48, label: "outside the scissor")

        XCTAssertEqual(inside, [255, 0, 0, 255], "the scissored quadrant is red")
        XCTAssertEqual(outside, [0, 0, 255, 255], "the rest of the surface is blue")
        XCTAssertNotEqual(
            inside, outside,
            "both points returned the same bytes, so the readback ignored its origin")
    }

    /// Canvas2D draws, and its records reach the same pixels WebGL's do.
    ///
    /// Half of what this engine draws is 2D and nothing had ever put a 2D
    /// record through this lane. The first record the fixture sends is
    /// `OP2D_CREATE_CONTEXT`, which did not exist until this test needed it:
    /// the 2D block was a complete drawing vocabulary with no way to bring a
    /// context into existence, and a canvas without one drops every record it
    /// is sent -- accepted, decoded, batched, and silently not drawn, with no
    /// error anywhere.
    ///
    /// Canvas2D's origin is top-left and `readPixels`' is bottom-left, so the
    /// quadrant filled at 2D (0,0) is the one `readPixels` finds at the top of
    /// its own space. Both points are asserted by name, so a flip reads as two
    /// known colours in the wrong places rather than as a puzzle.
    func testCanvas2DRecordsDrawPixelsThisSessionCanReadBack() throws {
        // Sequences are contiguous and this fixture is 4, so the three before
        // it go first.
        for (index, frame) in frames.enumerated() {
            try submit(frame.name, sequence: UInt64(index + 1))
        }
        try submit("clear-scissor-frame", sequence: 3)
        try submit("canvas2d-fill-frame", sequence: 4)

        // A characterised defect, tracked rather than deleted, because
        // XCTExpectFailure fails when the failure STOPS happening -- whoever
        // makes Skia build a context here finds out at this line.
        //
        // WHERE IT STOPS, measured rather than guessed. The records arrive and
        // dispatch correctly: OP2D_CREATE_CONTEXT reaches the renderer and runs.
        // What fails is Skia, and the log now says exactly where:
        //
        //     INFO  Skia is about to build a GL context
        //           gl_version=OpenGL ES 3.0 (ANGLE 2.1.1 git hash: 52f5942878)
        //     ERROR Skia GrDirectContext::make_gl returned none for the current
        //           GL context
        //     ERROR Canvas2DBatch cmd failed: 2d context not found: canvas_id=1 (x4)
        //
        // So a live, valid ANGLE ES 3.0 context IS current, the GL interface
        // loads, and Skia looks at that context and declines it. Two candidates
        // were eliminated on the way: the loader resolves every core GL name
        // ANGLE is asked for (checked against the dylibs directly, outside this
        // process), and the interface-load step is not the one that fails -- it
        // has its own message now and it does not appear.
        //
        // WHAT IS NOT YET KNOWN is whether this is particular to this lane or
        // true of Skia-on-ANGLE anywhere on macOS. `canvas2d_text` in the
        // graphics crate does render pixels on this machine, so Skia builds a
        // context somewhere; whether that path is this ANGLE is the next
        // question, and it is a graphics investigation rather than a wire one.
        XCTExpectFailure(
            "Canvas2D records reach the renderer and Skia will not build a GrDirectContext on "
                + "the ANGLE context they arrive under. See the comment above for what has been "
                + "eliminated. If this expectation itself fails, Skia has started building one "
                + "and the assertions below should become real again.")

        // 2D (4,4) is near the TOP-left; readPixels counts from the bottom, so
        // it is at y = 64 - 4.
        let quadrant = try readPixel(x: 4, y: 60, label: "the 2D-filled quadrant")
        let background = try readPixel(x: 4, y: 4, label: "outside it")

        XCTAssertEqual(quadrant, [0, 255, 0, 255], "the quadrant Canvas2D filled green")
        XCTAssertEqual(background, [0, 0, 255, 255], "the rest, which Canvas2D filled blue")
        XCTAssertNotEqual(
            quadrant, background,
            "both points returned the same bytes, so the second fill did not land where it was told")
    }

    func testAFrameProducedElsewhereDrawsPixelsThisSessionCanReadBack() throws {
        var readings: [[UInt8]] = []
        for (index, frame) in frames.enumerated() {
            let pixels = try submitAndRead(frame.name, sequence: UInt64(index + 1))
            // Exactly, with no tolerance: a tolerance is a number somebody picks,
            // and every wrong-colour-space and wrong-premultiply bug this project
            // has had would fit inside a generous one.
            for pixel in 0..<4 {
                let rgba = Array(pixels[(pixel * 4)..<(pixel * 4 + 4)])
                XCTAssertEqual(
                    rgba, frame.rgba,
                    "\(frame.name) pixel \(pixel) is \(rgba), and the frame cleared to \(frame.rgba)")
            }
            readings.append(pixels)
        }

        // The control. A readback that ignored the frame and returned a constant
        // would have satisfied every assertion above for whichever colour it
        // happened to return; it cannot satisfy this one.
        XCTAssertNotEqual(
            readings[0], readings[1],
            "the two frames cleared to different colours and the readback returned the same bytes")
    }
}
