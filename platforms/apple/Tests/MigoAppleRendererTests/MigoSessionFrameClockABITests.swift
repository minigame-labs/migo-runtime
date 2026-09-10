import MigoAppleCore
import MigoEngine
import QuartzCore
import XCTest

@testable import MigoAppleRenderer

// This platform's layer descriptor, named once. The same pair is named in
// MigoSurfaceAttachTests.swift, where it is `private` and therefore file-scoped;
// repeating it here rather than widening it keeps each file's payload type a
// local decision, and the two cannot drift apart silently because a wrong kind
// fails the attach that both files assert on.
#if os(macOS)
    private typealias LayerPayload = MigoMacosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER
#elseif os(iOS)
    private typealias LayerPayload = MigoIosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_IOS_CA_METAL_LAYER
#endif

/// The half of `MigoSessionFrameClock` its unit tests deliberately cannot reach.
///
/// `MigoSessionFrameClockTests` injects the notify closure, because what it is
/// about -- when a vsync becomes a frame -- is the clock's own decision and a
/// live session would only make those assertions slower. The consequence is that
/// nothing there exercises the convenience initialiser: whether it captures the
/// session, calls `migo_session_notify_vsync` on it, and records what the engine
/// actually answered. That is a seam, and a seam no test crosses is a seam that
/// works until somebody changes it.
///
/// The engine's own answer is what makes this checkable without a surface:
/// `include/migo/session.h` states that `migo_session_notify_vsync` returns
/// `MIGO_ERROR_INVALID_STATE` when no surface is attached. So a session with no
/// surface is not a limitation here -- it is a known reply, and seeing it come
/// back through the clock's statistics is proof the call was really made.
final class MigoSessionFrameClockABITests: XCTestCase {

    private var root: URL!
    private var engine: OpaquePointer?
    private var session: OpaquePointer?

    private let decision = MigoDisplayLinkPolicy.decide(
        MigoDisplayLinkPolicy.Request(platform: .iOS, osMajor: 17, targetFramesPerSecond: 60))

    override func setUpWithError() throws {
        try super.setUpWithError()
        // The engine reads MIGO_CAPI_LOG once, at engine create. Without it the
        // library's own account of a failure goes to a subscriber that does not
        // exist, and a red here would say only that a number was wrong.
        setenv("MIGO_CAPI_LOG", "info", 1)

        root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("migo-frame-clock-\(UUID().uuidString)")
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
        // Fixed and non-zero for the same reason MigoSurfaceAttachTests uses one:
        // the shipped Apple archive is the external-frame product, and an
        // all-zero nonce is what an uninitialised struct holds.
        withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { bytes in
            bytes.copyBytes(from: CollectionOfOne(UInt8(0xA3)))
        }
        var startedSession: OpaquePointer?
        XCTAssertEqual(migo_session_create(engine, &sessionConfig, &startedSession), MIGO_OK)
        session = try XCTUnwrap(startedSession)
    }

    override func tearDownWithError() throws {
        if let session {
            XCTAssertEqual(migo_session_destroy(session), MIGO_OK)
            self.session = nil
        }
        if let engine {
            XCTAssertEqual(migo_engine_destroy(engine), MIGO_OK)
            self.engine = nil
        }
        try? FileManager.default.removeItem(at: root)
        try super.tearDownWithError()
    }

    func testTheClockReachesTheEngineAndRecordsItsAnswer() throws {
        let session = try XCTUnwrap(self.session)
        let clock = MigoSessionFrameClock(session: session, decision: decision)

        clock.requestFrame()
        clock.tick(targetTimestamp: 1.0)

        let statistics = clock.currentStatistics
        XCTAssertEqual(statistics.ticks, 1)
        XCTAssertEqual(statistics.delivered, 0)
        XCTAssertEqual(
            statistics.refused, 1,
            "the tick has to have reached migo_session_notify_vsync for the engine to refuse it")
        XCTAssertEqual(
            statistics.lastRefusal, MIGO_ERROR_INVALID_STATE,
            "a session with no surface answers INVALID_STATE, and that is the reply this test "
                + "reads as proof the call was made rather than the clock guessing")
    }

    func testATickTheAbiWouldRefuseAsAnArgumentNeverReachesIt() throws {
        // `MigoVsyncTimestamp` rejects a negative timestamp before the call, and
        // the ABI would answer MIGO_ERROR_INVALID_ARGUMENT for it. Seeing
        // `refused` stay at zero is what says the rejection happened on this side
        // -- spending a call per frame to be told so is the thing being avoided,
        // and it is invisible unless something checks.
        let session = try XCTUnwrap(self.session)
        let clock = MigoSessionFrameClock(session: session, decision: decision)

        clock.requestFrame()
        clock.tick(targetTimestamp: -1.0)

        let statistics = clock.currentStatistics
        XCTAssertEqual(statistics.unusableTimestamps, 1)
        XCTAssertEqual(statistics.refused, 0, "the engine was not asked")
        XCTAssertNil(statistics.lastRefusal)
    }

    /// The accepted path, which every case above deliberately cannot reach.
    ///
    /// The three tests around this one all run against a session with no surface,
    /// because that is what makes the refusal a known reply. The cost is that
    /// `delivered` is zero in all of them: the clock has never had a vsync the
    /// engine accepted, and `MigoDisplayLink` -- the thing that produces vsyncs --
    /// has never driven it at all. Both halves of the shipping arrangement were
    /// untested together.
    ///
    /// So this attaches a real `CAMetalLayer`, starts a real link, and requires
    /// the engine to accept frames. Then it detaches and requires the next tick to
    /// be refused with `MIGO_ERROR_INVALID_STATE`, which is the same reply the
    /// no-surface tests read -- arriving here as a transition rather than as a
    /// starting condition, so it says the surface was the reason.
    func testADisplayLinkDrivesFramesTheEngineAccepts() throws {
        #if os(macOS) || os(iOS)
            let session = try XCTUnwrap(self.session)

            // Whether this machine can answer at all, decided BEFORE anything is
            // attached. Skipping after an attach would leave the attachment live,
            // and `migo_session_destroy` refuses while one is -- so the skip would
            // arrive as a teardown failure on exactly the machines the skip exists
            // for. A throwaway link asks the question without touching the
            // session's statistics, which the assertions below read.
            let probe = MigoDisplayLink(
                decision: MigoDisplayLinkPolicy.decide(.init(platform: .macOS, osMajor: 12)),
                onTick: { _, _ in })
            probe.start()
            let displayAvailable = probe.isRunning
            probe.stop()
            try XCTSkipUnless(
                displayAvailable,
                "no display link started on this machine, so no vsync can arrive. That is the "
                    + "headless case and not a failure")

            let layer = CAMetalLayer()
            layer.drawableSize = CGSize(width: 256, height: 256)
            layer.frame = CGRect(x: 0, y: 0, width: 256, height: 256)

            var payload = LayerPayload()
            payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
            payload.abi_version = MIGO_ABI_VERSION_CURRENT
            payload.platform_kind = layerKind
            payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()

            // Read out before the pointer is taken: reading `payload.struct_size`
            // inside `withUnsafePointer(to: &payload)` is an overlapping access
            // and the compiler refuses it.
            let payloadSize = payload.struct_size
            var attachment: OpaquePointer?
            let attached = withExtendedLifetime(layer) {
                withUnsafePointer(to: &payload) { raw -> MigoResult in
                    var descriptor = MigoSurfaceDescriptor()
                    descriptor.struct_size = UInt32(MemoryLayout<MigoSurfaceDescriptor>.size)
                    descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
                    descriptor.generation = 1
                    descriptor.platform_kind = layerKind
                    descriptor.width_pixels = 256
                    descriptor.height_pixels = 256
                    descriptor.scale_factor = 1.0
                    descriptor.color_space = MIGO_COLOR_SPACE_SRGB
                    descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
                    descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
                    descriptor.platform_descriptor_size = payloadSize
                    descriptor.platform_descriptor = UnsafeRawPointer(raw)
                    return migo_session_attach_surface(session, &descriptor, &attachment)
                }
            }
            XCTAssertEqual(attached, MIGO_OK, "attaching a host-owned CAMetalLayer failed")
            let live = try XCTUnwrap(attachment)

            // The real platform and OS, so what runs is what ships on this
            // machine rather than a branch picked to be convenient.
            let major = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
            #if os(macOS)
                let platform = MigoDisplayLinkPolicy.Platform.macOS
            #else
                let platform = MigoDisplayLinkPolicy.Platform.iOS
            #endif
            let clock = MigoSessionFrameClock(
                session: session,
                decision: MigoDisplayLinkPolicy.decide(.init(platform: platform, osMajor: major)))

            clock.start()
            XCTAssertTrue(
                clock.isRunning,
                "the probe above started a link on this machine and the clock's did not")

            // A real host calls requestFrame from `on_request_frame`, and nothing
            // requests frames here because no content is running -- this product
            // carries no engine to run any. A timer faster than the display is the
            // honest stand-in: it keeps a request outstanding so that every vsync
            // has one to spend, which is also what makes `coalescedRequests`
            // meaningful below.
            let wanted = 10
            let enough = expectation(description: "\(wanted) delivered frames")
            var fulfilled = false
            let pump = Timer.scheduledTimer(withTimeInterval: 1.0 / 240.0, repeats: true) { _ in
                clock.requestFrame()
                if !fulfilled, clock.currentStatistics.delivered >= wanted {
                    fulfilled = true
                    enough.fulfill()
                }
            }
            defer { pump.invalidate() }
            wait(for: [enough], timeout: 10)

            let running = clock.currentStatistics
            XCTAssertGreaterThanOrEqual(running.delivered, wanted)
            XCTAssertEqual(
                running.refused, 0,
                "the engine refused a vsync while a surface was attached; it answered "
                    + "\(String(describing: running.lastRefusal))")
            XCTAssertEqual(running.unusableTimestamps, 0, "a vsync produced no usable timestamp")
            XCTAssertGreaterThan(
                running.coalescedRequests, 0,
                "requests were made faster than the display ticks, so some had to coalesce; zero "
                    + "would mean the request flag is not doing the work it exists for")

            // Retire the surface, and only then ask again. The engine's reply has
            // to change, which is the point: the same call that was accepted a
            // moment ago is now INVALID_STATE, and nothing about the clock changed.
            // Stop asking for frames BEFORE retiring the surface, which is what a
            // host does: a display link still firing into a session whose surface
            // is being retired is work the retirement then has to drain. The first
            // version of this test only stopped the pump -- the clock kept
            // running -- and the retirement below timed out on a loaded CI runner
            // while the same retirement in MigoSurfaceAttachTests, which renders
            // nothing at all, completes in milliseconds.
            pump.invalidate()
            clock.stop()
            XCTAssertFalse(clock.isRunning)

            var release: OpaquePointer?
            XCTAssertEqual(migo_surface_begin_detach(live, &release), MIGO_OK)
            let observer = try XCTUnwrap(release)
            let released = expectation(description: "RELEASED")
            let poll = Timer.scheduledTimer(withTimeInterval: 0.02, repeats: true) { timer in
                var status = MigoSurfaceReleaseStatus()
                status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
                status.abi_version = MIGO_ABI_VERSION_CURRENT
                if migo_surface_release_query(observer, &status) == MIGO_OK,
                    status.state == MIGO_SURFACE_RELEASE_RELEASED
                {
                    timer.invalidate()
                    released.fulfill()
                }
            }
            defer { poll.invalidate() }
            // Budgeted against what this retirement has to do rather than against
            // the other test's: this one retires a surface that rendered frames,
            // so the render thread has work to finish first. 60 s is far above any
            // plausible teardown and is a budget, not a measurement -- the same
            // reasoning that set the WebKit page-report timeout.
            wait(for: [released], timeout: 60)
            XCTAssertEqual(migo_surface_release_destroy(observer), MIGO_OK)

            let before = clock.currentStatistics.refused
            clock.requestFrame()
            clock.tick(targetTimestamp: 1.0)
            let after = clock.currentStatistics
            XCTAssertEqual(
                after.refused, before + 1,
                "with the surface retired the engine has to refuse, and the clock has to record it")
            XCTAssertEqual(after.lastRefusal, MIGO_ERROR_INVALID_STATE)

        #else
            throw XCTSkip("this test needs a CAMetalLayer, which is macOS and iOS only")
        #endif
    }

    func testAClockWithNoOutstandingRequestNeverTouchesTheSession() throws {
        // The idle case is the common one: a display link fires every vsync
        // whether or not content asked for a frame. If those crossed into the
        // engine, the lane would spend a call per vsync being refused.
        let session = try XCTUnwrap(self.session)
        let clock = MigoSessionFrameClock(session: session, decision: decision)

        for index in 0..<10 {
            clock.tick(targetTimestamp: Double(index) / 60.0)
        }

        let statistics = clock.currentStatistics
        XCTAssertEqual(statistics.ticks, 10)
        XCTAssertEqual(statistics.idle, 10)
        XCTAssertEqual(statistics.refused, 0)
        XCTAssertEqual(statistics.delivered, 0)
    }
}
