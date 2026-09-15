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

/// The synchronous barrier's four entry points, called as a host calls them.
///
/// Everything below the C boundary has tests: the mailbox's state machine in
/// `frame-wire`, the session's execution path in `migo-core`, the producer and
/// the relay in `node`. The boundary itself had none, and it is the layer where
/// a mistake is invisible to every one of them -- a descriptor read at the wrong
/// offset, an outcome written with a state the record forbids, a reply copied
/// into a buffer whose length was never checked. Those are not caught by a Rust
/// test that constructs a `SyncRequest` directly; they are caught here, by
/// calling the exported symbols with the structs a host actually fills in.
///
/// WHERE IT RUNS, because the entry points are external-frames-only and that is
/// not obvious from this file. `migo_session_post_sync_request` and the three
/// beside it are compiled into the external-frame product and nothing else --
/// a session with a JavaScript runtime in this process has no producer to be
/// blocked. Both places this bundle runs link that product: on iOS the shipping
/// archive is external-frames, and on macOS these tests run only inside the
/// generated diagnostic package, because `platforms/apple`'s own xcframework
/// carries no macOS slice at all. So the symbols are always there; a lane that
/// changed either of those would find out at link time, which is the loud
/// failure and the right one.
///
/// WHAT THIS DOES NOT DO, deliberately: it does not read pixels. Answering
/// `readPixels` needs a renderer with a canvas, and the canvas comes from
/// content the producer has drawn -- which needs the A3 transport that is not
/// built yet. So every case here is one where the host must refuse, which is
/// exactly the set worth pinning first: a producer inside `Atomics.wait` is
/// woken by a refusal or not at all, and the refusals are what this boundary
/// exists to deliver reliably.
final class MigoSyncBarrierABITests: XCTestCase {

    private var root: URL!
    private var engine: OpaquePointer?
    private var session: OpaquePointer?
    private var attachment: OpaquePointer?

    /// The layer the engine was handed a raw pointer to.
    ///
    /// Held by the fixture through `migo_engine_destroy`, because `surface.h` is
    /// explicit that the barrier is the engine's teardown and not the session's:
    /// "only after it returns may the host destroy native display/window
    /// resources".
    private var retainedLayer: CAMetalLayer?

    private let now: UInt64 = 1_000_000_000
    private var deadline: UInt64 { now + 50_000_000 }

    override func setUpWithError() throws {
        try super.setUpWithError()
        setenv("MIGO_CAPI_LOG", "info", 1)

        root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("migo-sync-barrier-\(UUID().uuidString)")
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
        // Non-zero, because an all-zero nonce is what an uninitialised struct
        // holds and the external-frame product refuses one.
        withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { bytes in
            bytes.copyBytes(from: CollectionOfOne(UInt8(0xA3)))
        }
        var startedSession: OpaquePointer?
        XCTAssertEqual(migo_session_create(engine, &sessionConfig, &startedSession), MIGO_OK)
        session = try XCTUnwrap(startedSession)

        // Attach a surface, because that is when the session acquires an engine.
        // `migo_session_attach_surface` is what installs it (see
        // `capi/src/surface.rs`), so every entry point below would otherwise be
        // answering "there is no session to ask" rather than answering at all --
        // and a test suite that only ever saw MIGO_ERROR_INVALID_STATE would
        // pass against a boundary that does nothing.
        let layer = CAMetalLayer()
        layer.drawableSize = CGSize(width: 64, height: 64)
        layer.frame = CGRect(x: 0, y: 0, width: 64, height: 64)
        retainedLayer = layer

        var payload = LayerPayload()
        payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
        payload.abi_version = MIGO_ABI_VERSION_CURRENT
        payload.platform_kind = layerKind
        payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()

        var produced: OpaquePointer?
        // The size is read out before the pointer is taken: reading
        // `payload.struct_size` while `&payload` is already borrowed is an
        // exclusivity violation the compiler rejects.
        let payloadSize = payload.struct_size
        let attachResult = withUnsafeMutablePointer(to: &payload) { raw -> MigoResult in
            var descriptor = MigoSurfaceDescriptor()
            descriptor.struct_size = UInt32(MemoryLayout<MigoSurfaceDescriptor>.size)
            descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
            // Generations start at 1 and must strictly increase per session.
            descriptor.generation = 1
            descriptor.platform_kind = layerKind
            descriptor.width_pixels = 64
            descriptor.height_pixels = 64
            descriptor.scale_factor = 1.0
            descriptor.color_space = MIGO_COLOR_SPACE_SRGB
            // OPAQUE and capability_flags zero, for the reason
            // MigoSurfaceAttachTests states at length: any other value is
            // refused during configuration validation and never reaches the
            // platform layer at all.
            descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
            descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
            descriptor.platform_descriptor_size = payloadSize
            descriptor.platform_descriptor = UnsafeRawPointer(raw)
            return migo_session_attach_surface(startedSession, &descriptor, &produced)
        }
        XCTAssertEqual(attachResult, MIGO_OK, "the barrier needs a session with an engine")
        attachment = try XCTUnwrap(produced)
    }

    override func tearDownWithError() throws {
        // Retire the surface before anything is destroyed. `surface.h` is
        // explicit that MIGO_OK from begin_detach means retirement STARTED, and
        // that the native resource must stay alive until release_query reports
        // RELEASED -- and `migo_session_destroy` refuses a session that still
        // holds an unretired attachment, which is how this test first learned
        // it had skipped the step.
        if let live = attachment {
            var release: OpaquePointer?
            XCTAssertEqual(migo_surface_begin_detach(live, &release), MIGO_OK)
            attachment = nil
            if let observer = release {
                var status = MigoSurfaceReleaseStatus()
                status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
                status.abi_version = MIGO_ABI_VERSION_CURRENT
                // 60 s, for the reason the other waits in this bundle give: this
                // lane starves, and retirement has been observed completing
                // after a 5 s wait expired.
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
        attachment = nil
        if let session {
            XCTAssertEqual(migo_session_destroy(session), MIGO_OK)
            self.session = nil
        }
        if let engine {
            XCTAssertEqual(migo_engine_destroy(engine), MIGO_OK)
            self.engine = nil
        }
        // Only now, per surface.h: the engine destroy is the thread-completion
        // barrier, and the layer may not go before it returns.
        retainedLayer = nil
        try? FileManager.default.removeItem(at: root)
        try super.tearDownWithError()
    }

    // MARK: - Helpers

    private func descriptor(
        operation: UInt32 = MIGO_SYNC_OP_READ_PIXELS,
        maxReplyBytes: UInt32 = 4096,
        deadlineNanos: UInt64? = nil
    ) -> MigoSyncRequestDescriptor {
        var request = MigoSyncRequestDescriptor()
        request.struct_size = UInt32(MemoryLayout<MigoSyncRequestDescriptor>.size)
        request.abi_version = MIGO_ABI_VERSION_CURRENT
        request.runtime_generation = 1
        request.surface_generation = 1
        request.resource_epoch = 1
        request.triggering_sequence = 1
        request.deadline_nanos = deadlineNanos ?? deadline
        request.operation = operation
        request.max_reply_bytes = maxReplyBytes
        return request
    }

    private func outcomeRecord() -> MigoSyncOutcome {
        var outcome = MigoSyncOutcome()
        outcome.struct_size = UInt32(MemoryLayout<MigoSyncOutcome>.size)
        outcome.abi_version = MIGO_ABI_VERSION_CURRENT
        return outcome
    }

    /// `readPixels`' arguments: eight little-endian 32-bit words.
    private func readPixelsParams(width: Int32, height: Int32) -> [UInt8] {
        var bytes: [UInt8] = []
        func word(_ value: UInt32) {
            withUnsafeBytes(of: value.littleEndian) { bytes.append(contentsOf: $0) }
        }
        word(1)
        word(0)
        word(0)
        word(UInt32(bitPattern: width))
        word(UInt32(bitPattern: height))
        word(0x1908)  // GL_RGBA
        word(0x1401)  // GL_UNSIGNED_BYTE
        word(0)
        return bytes
    }

    private func post(
        _ request: inout MigoSyncRequestDescriptor,
        params: [UInt8],
        into outcome: inout MigoSyncOutcome
    ) -> MigoResult {
        let session = self.session!
        return params.withUnsafeBufferPointer { buffer in
            migo_session_post_sync_request(
                session, &request, buffer.baseAddress, buffer.count, now, &outcome)
        }
    }

    // MARK: - The record's own contract

    func testAnOutcomeRecordThatWasNeverInitialisedIsRefused() throws {
        // Two ways to get the header wrong, and they are answered differently on
        // purpose. Zeros -- what a host that forgot gets -- fail on abi_version,
        // because a record claiming version 0 is not a record this library knows
        // how to write. A right version with a wrong size fails on the size,
        // because struct_size is what bounds the write into the caller's
        // storage. Asserting only the second would let the first start
        // succeeding without anyone noticing.
        var request = descriptor()
        var zeroed = MigoSyncOutcome()
        XCTAssertEqual(
            post(&request, params: readPixelsParams(width: 2, height: 2), into: &zeroed),
            MIGO_ERROR_UNSUPPORTED_ABI,
            "an all-zero record claims abi_version 0 and must be refused as such")

        var wrongSize = outcomeRecord()
        wrongSize.struct_size = 8
        var second = descriptor()
        XCTAssertEqual(
            post(&second, params: readPixelsParams(width: 2, height: 2), into: &wrongSize),
            MIGO_ERROR_INVALID_ARGUMENT,
            "a struct_size that is not this library's is refused, never filled in")
    }

    func testARequestDescriptorThatWasNeverInitialisedIsRefused() throws {
        var zeroed = MigoSyncRequestDescriptor()
        var outcome = outcomeRecord()
        XCTAssertEqual(
            post(&zeroed, params: readPixelsParams(width: 2, height: 2), into: &outcome),
            MIGO_ERROR_UNSUPPORTED_ABI,
            "an uninitialised request descriptor must be refused")

        var wrongSize = descriptor()
        wrongSize.struct_size = 8
        var secondOutcome = outcomeRecord()
        XCTAssertEqual(
            post(&wrongSize, params: readPixelsParams(width: 2, height: 2), into: &secondOutcome),
            MIGO_ERROR_INVALID_ARGUMENT,
            "a descriptor whose size is not this library's is refused")
    }

    // MARK: - Refusals a producer must be woken by

    func testAnOperationThisHostDoesNotImplementIsRefusedWithAReason() throws {
        // Operation 0 is not an operation. The request is still posted and still
        // answered -- with a verdict. The alternative to a verdict, for an agent
        // inside Atomics.wait, is blocking until WebKit reclaims its process.
        var request = descriptor(operation: 0)
        var outcome = outcomeRecord()
        XCTAssertEqual(
            post(&request, params: readPixelsParams(width: 2, height: 2), into: &outcome),
            MIGO_OK,
            "the call itself succeeds; the verdict is in the outcome")
        XCTAssertEqual(outcome.state, MIGO_SYNC_STATE_FAILED)
        XCTAssertEqual(outcome.error, MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION)
        XCTAssertEqual(outcome.reply_bytes, 0, "a failure carries no bytes")
        XCTAssertNotEqual(outcome.request_id, 0, "a posted request has an id")
    }

    func testADeadlineThatHasAlreadyPassedIsRefusedBeforeTheSlotIsTaken() throws {
        var request = descriptor(deadlineNanos: now)
        var outcome = outcomeRecord()
        XCTAssertEqual(
            post(&request, params: readPixelsParams(width: 2, height: 2), into: &outcome),
            MIGO_OK)
        XCTAssertEqual(outcome.state, MIGO_SYNC_STATE_FAILED)
        XCTAssertEqual(outcome.error, MIGO_SYNC_ERROR_BAD_DEADLINE)
        // Refused before it was given an id, which is what request_id 0 says.
        XCTAssertEqual(outcome.request_id, 0)

        // And the slot is free, so a producer whose first call was refused can
        // make another. If it were not, one bad deadline would end synchronous
        // calls for the session.
        var second = descriptor()
        var secondOutcome = outcomeRecord()
        XCTAssertEqual(
            post(&second, params: readPixelsParams(width: 2, height: 2), into: &secondOutcome),
            MIGO_OK)
        XCTAssertNotEqual(
            secondOutcome.error, MIGO_SYNC_ERROR_ALREADY_PENDING,
            "a refused request must not occupy the one slot a session has")
    }

    func testAReservationOutsideTheProtocolsBoundsIsRefused() throws {
        for reservation in [UInt32(0), UInt32(16 * 1024 * 1024 + 1)] {
            var request = descriptor(maxReplyBytes: reservation)
            var outcome = outcomeRecord()
            XCTAssertEqual(
                post(&request, params: readPixelsParams(width: 2, height: 2), into: &outcome),
                MIGO_OK)
            XCTAssertEqual(
                outcome.error, MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION,
                "reservation \(reservation) must be refused")
        }
    }

    func testMalformedArgumentsNeverReachTheRenderer() throws {
        // A rectangle with no pixels, and a record one byte short. The second is
        // the one that matters: a decoder that accepted it would read its
        // remaining fields out of whatever followed the record.
        var empty = descriptor()
        var emptyOutcome = outcomeRecord()
        XCTAssertEqual(
            post(&empty, params: readPixelsParams(width: 0, height: 4), into: &emptyOutcome),
            MIGO_OK)
        XCTAssertEqual(emptyOutcome.error, MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION)

        var short = descriptor()
        var shortOutcome = outcomeRecord()
        var truncated = readPixelsParams(width: 4, height: 4)
        truncated.removeLast()
        XCTAssertEqual(post(&short, params: truncated, into: &shortOutcome), MIGO_OK)
        XCTAssertEqual(shortOutcome.error, MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION)
    }

    // MARK: - Taking a reply

    func testTakingAReplyThereIsNoAnswerForReportsFailureAndWritesZero() throws {
        let session = try XCTUnwrap(self.session)
        var buffer = [UInt8](repeating: 0xEE, count: 64)
        // Deliberately not zero, so "written was set to zero" is distinguishable
        // from "written was never touched".
        var written = 999
        let result = buffer.withUnsafeMutableBufferPointer { out in
            migo_session_take_sync_reply(session, out.baseAddress, out.count, &written)
        }
        XCTAssertEqual(result, MIGO_ERROR_INVALID_STATE, "there is no ready answer to take")
        XCTAssertEqual(
            written, 0,
            "a caller that ignores the result must not read a stale count as a byte count")
    }

    func testPollingASessionWithNoOutstandingRequestReportsAnEmptySlot() throws {
        let session = try XCTUnwrap(self.session)
        var outcome = outcomeRecord()
        XCTAssertEqual(migo_session_poll_sync(session, now, &outcome), MIGO_OK)
        XCTAssertEqual(outcome.state, MIGO_SYNC_STATE_FREE)
        // FREE is the one state the record requires to carry nothing.
        XCTAssertEqual(outcome.request_id, 0)
        XCTAssertEqual(outcome.reply_bytes, 0)
        XCTAssertEqual(outcome.error, 0)
    }

    func testCancellingWithNothingOutstandingIsNotAnError() throws {
        let session = try XCTUnwrap(self.session)
        var outcome = outcomeRecord()
        XCTAssertEqual(migo_session_cancel_sync(session, now, &outcome), MIGO_OK)
        XCTAssertEqual(outcome.state, MIGO_SYNC_STATE_FREE)
    }

    // MARK: - Null and bad handles

    func testEveryEntryPointRefusesANullSession() throws {
        var request = descriptor()
        var outcome = outcomeRecord()
        var written = 0
        var buffer = [UInt8](repeating: 0, count: 16)

        XCTAssertNotEqual(
            migo_session_post_sync_request(nil, &request, nil, 0, now, &outcome), MIGO_OK)
        XCTAssertNotEqual(migo_session_poll_sync(nil, now, &outcome), MIGO_OK)
        XCTAssertNotEqual(migo_session_cancel_sync(nil, now, &outcome), MIGO_OK)
        XCTAssertNotEqual(
            buffer.withUnsafeMutableBufferPointer { out in
                migo_session_take_sync_reply(nil, out.baseAddress, out.count, &written)
            }, MIGO_OK)
    }
}
