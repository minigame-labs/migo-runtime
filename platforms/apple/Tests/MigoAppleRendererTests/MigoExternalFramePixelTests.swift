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

    private let now: UInt64 = 1_000_000_000
    private var deadline: UInt64 { now + 5_000_000_000 }

    /// The colour the committed fixture clears to, in the order `readPixels`
    /// returns it. Named once so the assertion and the emitter cannot drift
    /// apart silently: `emit-clear-frame.mjs` exports the same four numbers.
    private let expected: [UInt8] = [0, 0, 255, 255]

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

    private func fixture() throws -> Data {
        let url = try XCTUnwrap(
            Bundle.module.url(forResource: "clear-blue-frame", withExtension: "bin",
                              subdirectory: "Fixtures"),
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

    func testAFrameProducedElsewhereDrawsPixelsThisSessionCanReadBack() throws {
        let session = try XCTUnwrap(self.session)
        let packet = try fixture()

        // 1. The frame crosses.
        var ingress = MigoFrameIngressOutcome()
        ingress.struct_size = UInt32(MemoryLayout<MigoFrameIngressOutcome>.size)
        ingress.abi_version = MIGO_ABI_VERSION_CURRENT
        let submitted = packet.withUnsafeBytes { raw -> MigoResult in
            migo_session_submit_external_frame(
                session, raw.bindMemory(to: UInt8.self).baseAddress, raw.count, &ingress)
        }
        XCTAssertEqual(submitted, MIGO_OK, "the call itself must succeed")
        XCTAssertEqual(
            ingress.decision, MIGO_FRAME_INGRESS_ACCEPTED,
            """
            the committed frame was not accepted (wire_error_code \
            \(ingress.wire_error_code)). That code names which field the ingress refused, \
            and frame-wire's clear_frame_fixture test asserts what each of them holds.
            """)
        XCTAssertEqual(ingress.accepted_sequence, 1, "the fixture is sequence 1")

        // 2. The pixels come back, through the barrier a blocked readPixels uses.
        var request = MigoSyncRequestDescriptor()
        request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
        request.abi_version = MIGO_ABI_VERSION_CURRENT
        request.runtime_generation = 1
        request.surface_generation = 1
        request.resource_epoch = 0
        request.triggering_sequence = 1
        request.deadline_nanos = deadline
        request.operation = MIGO_SYNC_OP_READ_PIXELS
        // Two pixels wide by two high: enough that a wrong row stride shows up,
        // small enough that a mismatch prints legibly.
        request.max_reply_bytes = 2 * 2 * 4

        var outcome = MigoSyncOutcome()
        outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
        outcome.abi_version = MIGO_ABI_VERSION_CURRENT

        let params = readPixelsParams(x: 0, y: 0, width: 2, height: 2)
        let posted = params.withUnsafeBufferPointer { buffer in
            migo_session_post_sync_request(
                session, &request, buffer.baseAddress, buffer.count, now, &outcome)
        }
        XCTAssertEqual(posted, MIGO_OK)
        XCTAssertEqual(
            outcome.state, MIGO_SYNC_STATE_READY,
            "the readback failed with error \(outcome.error)")
        XCTAssertEqual(outcome.reply_bytes, 2 * 2 * 4, "four RGBA8 pixels")

        var pixels = [UInt8](repeating: 0, count: Int(outcome.reply_bytes))
        var written = 0
        let taken = pixels.withUnsafeMutableBufferPointer { out in
            migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
        }
        XCTAssertEqual(taken, MIGO_OK)
        XCTAssertEqual(written, pixels.count)

        // 3. And they are the colour the frame asked for. Exactly: no tolerance,
        // because a tolerance is a number somebody picks and every
        // wrong-colour-space and wrong-premultiply bug this project has had
        // would fit inside a generous one.
        for pixel in 0..<4 {
            let rgba = Array(pixels[(pixel * 4)..<(pixel * 4 + 4)])
            XCTAssertEqual(
                rgba, expected,
                "pixel \(pixel) is \(rgba), and the frame cleared to \(expected)")
        }
    }
}
