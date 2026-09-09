import MigoAppleCore
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

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
