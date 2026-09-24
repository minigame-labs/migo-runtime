import Foundation
import MigoEngine
import QuartzCore

#if os(macOS)
    private typealias LayerPayload = MigoMacosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER
#elseif os(iOS)
    private typealias LayerPayload = MigoIosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_IOS_CA_METAL_LAYER
#endif

/// An engine, a session, and a surface to draw on -- the eighty lines that have
/// to run before any external-frame question can be asked.
///
/// A target rather than a copy in each test file. `MigoAppleRendererTests` owns
/// this bring-up today and the acceptance test for the Performance+ lane needs
/// the same one; the alternative was to have `MigoAppleRendererTests` depend on
/// the Performance+ lane, which `Package.swift` rejects for a good reason -- it
/// would make every renderer test build the lane as well.
///
/// **Not a test target.** It carries no assertions: every failure comes back as
/// a thrown `Failure` naming the call that refused, so the test that asked can
/// say what it was asking. A helper that called `XCTAssert` itself would report
/// its own line numbers for somebody else's question.
///
/// The numbers here are the committed fixture's, not defaults: generation 1 and
/// launch nonce `0xA3` are what `clear-blue-frame` is addressed to, and a packet
/// whose nonce does not match is refused before anything looks at its contents.
public final class MigoFrameHarness {

    public enum Failure: Error, CustomStringConvertible {
        case call(String, MigoResult)
        case nothingProduced(String)

        public var description: String {
            switch self {
            case .call(let name, let result): return "\(name) returned \(result)"
            case .nothingProduced(let name): return "\(name) succeeded and produced nothing"
            }
        }
    }

    public let session: OpaquePointer
    public let sizePixels: Int

    private let root: URL
    private let engine: OpaquePointer
    private var attachment: OpaquePointer?
    private var retainedLayer: CAMetalLayer?

    /// The generation and nonce the committed fixtures are addressed to.
    public static let fixtureGeneration: UInt64 = 1
    public static let fixtureLaunchNonce: UInt8 = 0xA3

    public init(sizePixels: Int = 64) throws {
        self.sizePixels = sizePixels
        // A local until every stored property is initialised: `withCString`
        // takes a closure, and a closure that reads `self.root` before the
        // initialiser has finished is one Swift refuses to compile.
        let root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("migo-frame-harness-\(UUID().uuidString)")
        self.root = root
        for name in ["files", "cache", "code-cache"] {
            try FileManager.default.createDirectory(
                at: root.appendingPathComponent(name), withIntermediateDirectories: true)
        }

        var created: OpaquePointer?
        var engineResult = MIGO_OK
        root.appendingPathComponent("files").path.withCString { files in
            root.appendingPathComponent("cache").path.withCString { cache in
                root.appendingPathComponent("code-cache").path.withCString { codeCache in
                    var config = MigoEngineConfig()
                    config.struct_size = UInt32(MemoryLayout<MigoEngineConfig>.size)
                    config.abi_version = MIGO_ABI_VERSION_CURRENT
                    config.files_dir_utf8 = files
                    config.cache_dir_utf8 = cache
                    config.code_cache_dir_utf8 = codeCache
                    // The fixtures are written unsigned, and the lane verifies
                    // content unless told otherwise -- so it is told.
                    config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT
                    engineResult = migo_engine_create(&config, &created)
                }
            }
        }
        guard engineResult == MIGO_OK else { throw Failure.call("migo_engine_create", engineResult) }
        guard let engine = created else { throw Failure.nothingProduced("migo_engine_create") }
        self.engine = engine

        var sessionConfig = MigoSessionConfig()
        sessionConfig.struct_size = UInt32(MemoryLayout<MigoSessionConfig>.size)
        sessionConfig.abi_version = MIGO_ABI_VERSION_CURRENT
        withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { bytes in
            bytes.copyBytes(from: CollectionOfOne(Self.fixtureLaunchNonce))
        }
        var startedSession: OpaquePointer?
        let sessionResult = migo_session_create(engine, &sessionConfig, &startedSession)
        guard sessionResult == MIGO_OK else {
            throw Failure.call("migo_session_create", sessionResult)
        }
        guard let session = startedSession else {
            throw Failure.nothingProduced("migo_session_create")
        }
        self.session = session

        let layer = CAMetalLayer()
        layer.drawableSize = CGSize(width: sizePixels, height: sizePixels)
        layer.frame = CGRect(x: 0, y: 0, width: sizePixels, height: sizePixels)
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
            descriptor.generation = Self.fixtureGeneration
            descriptor.platform_kind = layerKind
            descriptor.width_pixels = UInt32(sizePixels)
            descriptor.height_pixels = UInt32(sizePixels)
            descriptor.scale_factor = 1.0
            descriptor.color_space = MIGO_COLOR_SPACE_SRGB
            descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
            descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
            descriptor.platform_descriptor_size = payloadSize
            descriptor.platform_descriptor = UnsafeRawPointer(raw)
            return migo_session_attach_surface(session, &descriptor, &produced)
        }
        guard attachResult == MIGO_OK else {
            throw Failure.call("migo_session_attach_surface", attachResult)
        }
        attachment = produced
    }

    /// Retire the surface, waiting for the renderer to report it released.
    ///
    /// Explicit rather than in `deinit`, because the wait is the point: a
    /// session destroyed while a surface is still attached is refused, and a
    /// `deinit` that swallowed that would turn a real defect into a leak.
    /// Returns whether the surface reached RELEASED within `timeout`.
    @discardableResult
    public func shutDown(timeout: TimeInterval = 60) -> Bool {
        var released = true
        if let live = attachment {
            var observer: OpaquePointer?
            if migo_surface_begin_detach(live, &observer) == MIGO_OK, let observer {
                released = false
                var status = MigoSurfaceReleaseStatus()
                status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
                status.abi_version = MIGO_ABI_VERSION_CURRENT
                let end = Date().addingTimeInterval(timeout)
                while Date() < end {
                    guard migo_surface_release_query(observer, &status) == MIGO_OK else { break }
                    if status.state == MIGO_SURFACE_RELEASE_RELEASED {
                        released = true
                        break
                    }
                    RunLoop.current.run(until: Date().addingTimeInterval(0.001))
                }
            }
            attachment = nil
        }
        _ = migo_session_destroy(session)
        _ = migo_engine_destroy(engine)
        retainedLayer = nil
        try? FileManager.default.removeItem(at: root)
        return released
    }

    /// Install a game where the engine looks for installed content, load it,
    /// and answer with the directory the engine mounted -- the one a
    /// Performance+ host serves at the content origin's root.
    ///
    /// The install layout is the one `session.h` documents for
    /// `MigoContentDescriptor` (`<files_dir>/migo/games/<id>/code`): writing it is
    /// what an installer does. Where the code is *served from* is asked of the
    /// engine rather than recomputed here, which is the property a product host
    /// relies on.
    public func installAndLoadContent(id: String, entry: String, files: [String: Data]) throws -> URL {
        let code = root.appendingPathComponent("files/migo/games/\(id)/code")
        for (path, bytes) in files {
            let target = code.appendingPathComponent(path)
            try FileManager.default.createDirectory(
                at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            try bytes.write(to: target)
        }
        let loaded = id.withCString { idText in
            entry.withCString { entryText -> MigoResult in
                var descriptor = MigoContentDescriptor()
                descriptor.struct_size = UInt32(MemoryLayout<MigoContentDescriptor>.size)
                descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
                descriptor.content_id_utf8 = idText
                descriptor.entry_utf8 = entryText
                return migo_session_load_content(session, &descriptor)
            }
        }
        guard loaded == MIGO_OK else { throw Failure.call("migo_session_load_content", loaded) }
        var length = 0
        _ = migo_session_copy_content_root(session, nil, 0, &length)
        var buffer = [CChar](repeating: 0, count: length + 1)
        let copied = migo_session_copy_content_root(session, &buffer, buffer.count, &length)
        guard copied == MIGO_OK else { throw Failure.call("migo_session_copy_content_root", copied) }
        return URL(fileURLWithPath: String(cString: buffer), isDirectory: true)
    }

    /// Send one touch event the way a host's view does: through the C ABI, in
    /// CSS pixels, one point per finger.
    public func sendTouch(
        _ type: MigoTouchType, points: [MigoTouchPoint], timestampMilliseconds: Int64
    ) throws {
        var event = MigoTouchEvent()
        event.struct_size = UInt32(MemoryLayout<MigoTouchEvent>.size)
        event.abi_version = MIGO_ABI_VERSION_CURRENT
        event.type = type
        event.point_count = UInt32(points.count)
        event.timestamp_ms = timestampMilliseconds
        let result = points.withUnsafeBufferPointer { buffer -> MigoResult in
            event.points = buffer.baseAddress
            return migo_session_send_touch(session, &event)
        }
        guard result == MIGO_OK else { throw Failure.call("migo_session_send_touch", result) }
    }

    /// The argument record a blocked `readPixels` sends through the barrier.
    ///
    /// Built here rather than in each test because it is the wire format's, not
    /// a test's: canvas id, rect, then GL's own format and type enums.
    public static func readPixelsParameters(
        canvasId: UInt32 = 1, x: Int32, y: Int32, width: Int32, height: Int32
    ) -> [UInt8] {
        var bytes: [UInt8] = []
        func word(_ value: UInt32) {
            withUnsafeBytes(of: value.littleEndian) { bytes.append(contentsOf: $0) }
        }
        word(canvasId)
        word(UInt32(bitPattern: x))
        word(UInt32(bitPattern: y))
        word(UInt32(bitPattern: width))
        word(UInt32(bitPattern: height))
        word(0x1908)  // GL_RGBA
        word(0x1401)  // GL_UNSIGNED_BYTE
        word(0)
        return bytes
    }
}

extension MigoFrameHarness {

    /// One committed frame, by name.
    ///
    /// From this target's own bundle, so both test targets read the SAME bytes.
    /// The acceptance test's claim is that a packet which draws when submitted
    /// directly also draws when it has crossed the transport; two copies of the
    /// fixture would be two things that can differ, and the test could not tell
    /// that apart from the transport corrupting one.
    public static func fixture(named name: String) throws -> Data {
        guard
            let url = Bundle.module.url(
                forResource: name, withExtension: "bin", subdirectory: "Fixtures")
                ?? Bundle.module.url(forResource: name, withExtension: "bin")
        else {
            throw Failure.nothingProduced(
                """
                the committed frame \(name).bin is not in MigoAppleFrameHarness's bundle. \
                It is declared as a resource of that target in Package.swift; regenerate it \
                with `node platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs`.
                """)
        }
        return try Data(contentsOf: url)
    }
}
