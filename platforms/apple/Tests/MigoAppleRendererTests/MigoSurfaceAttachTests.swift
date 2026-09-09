import MigoEngine
import QuartzCore
import XCTest

#if os(macOS)
    import AppKit
#elseif os(iOS)
    import UIKit
#endif

// What this platform's two descriptors are, stated once, so each test below has
// one body rather than two.
//
// Both platforms declare the same PAIR, for the reason ios.h gives: "a tagless
// void* would let a host set the wrong kind, compile cleanly, and hand a UIView*
// to code that calls nextDrawable on it". That pair is exactly what these two
// tests exercise -- the layer is accepted, the view is refused -- so the pair is
// what is named here and the bodies stay platform-free.
#if os(macOS)
    private typealias HostView = NSView
    private typealias LayerPayload = MigoMacosMetalLayerDescriptor
    private typealias ViewPayload = MigoMacosNsViewDescriptor
    private let layerKind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER
    private let viewKind = MIGO_PLATFORM_MACOS_NS_VIEW
    private let viewTypeName = "NSView"
    private let ledgerConstantName = "MIGO_PLATFORM_MACOS_CA_METAL_LAYER"
#elseif os(iOS)
    private typealias HostView = UIView
    private typealias LayerPayload = MigoIosMetalLayerDescriptor
    private typealias ViewPayload = MigoIosUiViewDescriptor
    private let layerKind = MIGO_PLATFORM_IOS_CA_METAL_LAYER
    private let viewKind = MIGO_PLATFORM_IOS_UI_VIEW
    private let viewTypeName = "UIView"
    private let ledgerConstantName = "MIGO_PLATFORM_IOS_CA_METAL_LAYER"
#endif

/// Where the engine's account of a failure goes, so a failing assertion can
/// carry it.
///
/// `on_error` is the channel `session.h` promises operational errors arrive on
/// -- "Runtime errors and recoverable pressure notifications" -- and installing
/// it is what a host does. Not installing it is why the first iOS run of these
/// tests could only report `MIGO_ERROR_INTERNAL`: the session thread had failed
/// on its way up, `run_external_session` had called `notify_error` with the
/// reason, and there was nobody on the other end. `MIGO_CAPI_LOG` does not
/// cover this on a simulator either -- the test process's stdout does not reach
/// the xcodebuild log, and that run's log contains no engine line at all.
///
/// Locked because the callback runs on whichever thread dispatched the task,
/// and the assertions read it from the test's thread.
private final class EngineErrors {
    private let lock = NSLock()
    private var reported: [String] = []

    func record(_ message: String) {
        lock.lock()
        reported.append(message)
        lock.unlock()
    }

    /// What to append to a failure message: the engine's own words, or an
    /// explicit statement that it said nothing, which is itself a finding.
    var summary: String {
        lock.lock()
        defer { lock.unlock() }
        if reported.isEmpty {
            return "the engine reported no error through on_error"
        }
        return reported.joined(separator: " | ")
    }
}

/// Runs the task inline, which `session.h` explicitly allows: "the dispatcher
/// ... must invoke it exactly once (inline or later)". A test has no event loop
/// to post to, and a queued task would be a second thing that has to be pumped
/// before an assertion could read what it delivered.
private let dispatchInline: MigoDispatchFn = { _, task, taskContext in
    guard let task else { return MIGO_ERROR_DISPATCH_REJECTED }
    task(taskContext)
    return MIGO_OK
}

private let recordEngineError: MigoOnErrorFn = { userData, _, error in
    guard let userData, let error else { return }
    let sink = Unmanaged<EngineErrors>.fromOpaque(userData).takeUnretainedValue()
    var text = "(no message)"
    if let message = error.pointee.message_utf8 {
        text = String(cString: message)
    }
    sink.record("code \(error.pointee.code): \(text)")
}

/// A host-owned `CAMetalLayer` survives the whole C ABI attach path.
///
/// WHAT THIS PROVES, precisely, because the constant it feeds is guarded by a
/// rule about evidence and a vague claim here would be worse than none: a real
/// `CAMetalLayer` -- not a pointer-shaped integer -- is copied out of a versioned
/// descriptor, validated, matched to the Apple platform module, turned into an
/// `AppleMetalLayerSurface` and a `CAMetalLayer` graphics platform, and accepted
/// by a Session as generation 1. Every one of those steps is code this repository
/// owns and none of it had ever run on an Apple machine.
///
/// WHAT IT DOES NOT PROVE, and this is deliberate rather than an omission. It
/// does not prove ANGLE loaded, that EGL got a context, or that anything was
/// drawn. The shipped Apple product is `--features external-frames`, and
/// `engine/crates/core/src/runtime/external.rs` destructures `gpu_init_started`
/// into `_gpu_init_started` on purpose: an external-frame session reports ready
/// once its tokio runtime is up and never waits for the renderer, because its
/// producer needs somewhere to put the first packet before the GPU is warm. So
/// `MIGO_OK` here is a statement about the boundary, not about the GPU. The EGL
/// half is asserted separately and directly by
/// `angle_loads_under_its_pinned_name_and_answers_with_a_display` in
/// `engine/crates/platform/src/apple/presenter.rs`, on this same lane.
///
/// Saying that out loud matters because the failure this pair exists to prevent
/// is exactly a green signal that meant less than it looked like: a published
/// Windows SDK exported every entry point, loaded cleanly, and could attach
/// nothing, while every gate agreed with every other gate.
///
/// WHY SWIFT AND NOT A RUST TEST. The thing that has to be attached is a real
/// `CAMetalLayer`, which is what a host passes and what ANGLE's Metal backend
/// requires -- handed a plain `CALayer`, ANGLE allocates its own metal layer as a
/// sublayer and the host loses control of `maximumDrawableCount`, `contentsScale`
/// and `presentsWithTransaction`. Creating one from Rust would mean an
/// Objective-C crate this repository does not otherwise need; creating one here
/// needs nothing, because this test target already links the engine and already
/// runs on the macOS lane.
final class MigoSurfaceAttachTests: XCTestCase {
    #if os(macOS) || os(iOS)
        private var root: URL!
        private var engine: OpaquePointer!
        private var session: OpaquePointer!
        private var attachment: OpaquePointer?

        /// Native objects the engine was handed a raw pointer to.
        ///
        /// Held by the fixture rather than by the test method, because two header
        /// contracts forbid releasing them where a local would be released.
        /// `migo_surface_begin_detach` is explicit that MIGO_OK means retirement
        /// STARTED: "Keep the native resource and its event loop alive until
        /// migo_surface_release_query reports MIGO_SURFACE_RELEASE_RELEASED;
        /// destroying it earlier is a use-after-free inside the driver, which the
        /// engine cannot detect or prevent." And `migo_engine_destroy` is the
        /// stricter, final gate -- it is a thread-completion barrier, and "only
        /// after it returns may the host destroy native display/window resources".
        ///
        /// Most tests keep a host reference through shutdown. The ownership
        /// regression opts out so it can check Migo's independent native retain.
        /// This array is cleared after `migo_engine_destroy` has returned.
        private var retained: [AnyObject] = []

        /// Held by the fixture because the engine is handed an unretained
        /// pointer to it and may call back from its own thread.
        private let engineErrors = EngineErrors()

        /// Set by the retirement test when the host's layer outlived RELEASED, so
        /// that teardown can answer the one question that separates "Migo leaked"
        /// from "somebody else still holds it".
        ///
        /// `migo_engine_destroy` is the thread-completion barrier, and it is where
        /// ANGLE's display goes. If the layer is gone once that returns, the owner
        /// was display-level ANGLE state Migo kept alive -- ours to fix. If it is
        /// still there, no part of Migo held it and the assertion at RELEASED was
        /// asking for something the C ABI never promised.
        private var layerThatOutlivedRelease: (() -> Bool)?

        override func setUpWithError() throws {
            try super.setUpWithError()
            layerThatOutlivedRelease = nil

            // Turn the engine's own diagnostics on before anything creates an
            // engine: `migo_engine_create` reads MIGO_CAPI_LOG once and installs
            // a subscriber, and without it every `tracing::error!` the library
            // emits is discarded.
            //
            // Not a debugging leftover. When the iOS arm of this test first ran,
            // attach answered MIGO_ERROR_INTERNAL and the log said nothing at
            // all -- the session thread had failed on its way up and the one
            // sentence naming the reason went to a subscriber that did not
            // exist. A test whose whole purpose is producing evidence should not
            // be throwing the library's own account of a failure away, and the
            // cost of finding out otherwise is a lane round trip per question.
            setenv("MIGO_CAPI_LOG", "info", 1)

            // Directories the engine may write into. A test that pointed these
            // at the source tree would be a test that leaves artefacts behind on
            // the machine that ran it.
            root = URL(fileURLWithPath: NSTemporaryDirectory())
                .appendingPathComponent("migo-attach-\(UUID().uuidString)")
            for name in ["files", "cache", "code-cache"] {
                try FileManager.default.createDirectory(
                    at: root.appendingPathComponent(name),
                    withIntermediateDirectories: true)
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
                        let result = migo_engine_create(&config, &created)
                        XCTAssertEqual(result, MIGO_OK, "migo_engine_create returned \(result)")
                    }
                }
            }
            engine = try XCTUnwrap(created)

            var sessionConfig = MigoSessionConfig()
            sessionConfig.struct_size = UInt32(MemoryLayout<MigoSessionConfig>.size)
            sessionConfig.abi_version = MIGO_ABI_VERSION_CURRENT
            // Required, and required by THIS product rather than by attach in
            // general. The shipped Apple xcframework is built
            // `--no-default-features --features external-frames`, and that lane
            // refuses to start a session with no launch identity: an all-zero
            // nonce is what an uninitialised struct holds, so a session that
            // accepted it would accept frame packets from any other process that
            // also failed to initialise one. Without this, attach returns
            // MIGO_ERROR_INVALID_STATE and never reaches the platform layer these
            // tests are about.
            //
            // Any non-zero value will do, because nothing here submits a packet.
            // A fixed one rather than a random one, so a failure reproduces.
            withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { bytes in
                bytes.copyBytes(from: CollectionOfOne(UInt8(0xA2)))
            }
            var startedSession: OpaquePointer?
            let result = migo_session_create(engine, &sessionConfig, &startedSession)
            XCTAssertEqual(result, MIGO_OK, "migo_session_create returned \(result)")
            session = try XCTUnwrap(startedSession)

            // Before the first attach, which is the only window the ABI allows:
            // "Callback configuration can be installed successfully only once
            // per Session and only before its first Surface attach".
            //
            // This makes the attach below a slightly more representative
            // exercise than it was -- a real host installs callbacks, so the
            // attach path now builds a Notifier as it would in production
            // instead of taking the None branch -- and it is the only way a
            // failure inside the session thread can reach an assertion.
            var callbacks = MigoHostCallbacks()
            callbacks.struct_size = UInt32(MemoryLayout<MigoHostCallbacks>.size)
            callbacks.abi_version = MIGO_ABI_VERSION_CURRENT
            callbacks.user_data = Unmanaged.passUnretained(engineErrors).toOpaque()
            callbacks.dispatch = dispatchInline
            callbacks.on_error = recordEngineError
            let installed = migo_session_set_host_callbacks(session, &callbacks)
            XCTAssertEqual(
                installed, MIGO_OK, "migo_session_set_host_callbacks returned \(installed)")
        }

        /// The documented shutdown handshake, in full, and asserted at every
        /// step.
        ///
        /// Not housekeeping -- this is the half of the surface contract a host
        /// must implement, and running it here is the only place it is exercised
        /// on Apple. It also has to be done properly for the tests above to mean
        /// anything: `migo_session_destroy`'s header says destruction "refuses
        /// with MIGO_ERROR_INVALID_STATE while a Surface transition is active, an
        /// attachment is still live, or any retired Surface is still PENDING". A
        /// teardown that called destroy with a live attachment and ignored the
        /// result would leak the Session and its host thread once per test, and
        /// leave that thread holding a pointer to a deallocated layer.
        override func tearDownWithError() throws {
            if let live = attachment {
                attachment = nil
                var release: OpaquePointer?
                let began = migo_surface_begin_detach(live, &release)
                XCTAssertEqual(began, MIGO_OK, "migo_surface_begin_detach returned \(began)")

                if let release {
                    // The status record is a CALLER-OWNED versioned output, so
                    // its header is an input to the call rather than something
                    // the library fills in. `write_versioned_output` reads
                    // `struct_size` out of the caller's storage to decide how
                    // many bytes it may write there, and a record left at its
                    // Swift default holds zero -- below the minimum this ABI
                    // defines -- so the query is refused before it ever looks at
                    // the release.
                    //
                    // Asserting that refusal here rather than only fixing it: it
                    // is the documented contract, and the first run of this lane
                    // failed 463 times on exactly this while reporting "the
                    // retired surface never reported RELEASED" -- which names the
                    // wrong half of the system. A host reading that message would
                    // go looking for a renderer that never started.
                    var uninitialised = MigoSurfaceReleaseStatus()
                    XCTAssertEqual(
                        migo_surface_release_query(release, &uninitialised),
                        MIGO_ERROR_INVALID_ARGUMENT,
                        "a status record whose struct_size was never set must be refused, "
                            + "because struct_size is what bounds the write into the "
                            + "caller's own storage")

                    var status = MigoSurfaceReleaseStatus()
                    status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
                    status.abi_version = MIGO_ABI_VERSION_CURRENT
                    var released = false
                    // Polled, not waited on: the header says the query never
                    // blocks precisely so a host can ask from its UI thread or an
                    // idle handler. A host's obligation is to keep asking while
                    // its event loop runs, so that is what this imitates.
                    let deadline = Date().addingTimeInterval(5)
                    while Date() < deadline {
                        let queried = migo_surface_release_query(release, &status)
                        XCTAssertEqual(queried, MIGO_OK, "release_query returned \(queried)")
                        if status.state == MIGO_SURFACE_RELEASE_RELEASED {
                            released = true
                            break
                        }
                        usleep(1000)
                    }
                    XCTAssertTrue(
                        released,
                        """
                        the retired surface never reported RELEASED. Until it does, \
                        migo_session_destroy is required to refuse and the native layer \
                        is not safe to free, so a host in this state cannot shut down.
                        """)
                    let destroyed = migo_surface_release_destroy(release)
                    XCTAssertEqual(
                        destroyed, MIGO_OK, "migo_surface_release_destroy returned \(destroyed)")
                }
            }

            var wentAtSessionDestroy = false
            if let session {
                let result = migo_session_destroy(session)
                XCTAssertEqual(
                    result, MIGO_OK,
                    "migo_session_destroy returned \(result); a refusal means something was "
                        + "still attached or still PENDING")
                self.session = nil
                // Halves the search. Session destroy takes the render thread, its
                // canvas manager and every EGL context down; engine destroy is
                // what terminates the display. A layer that survives RELEASED and
                // goes here is held by something inside the renderer; one that
                // survives to the next step is held at display level.
                if let stillAlive = layerThatOutlivedRelease {
                    wentAtSessionDestroy = !stillAlive()
                }
            }
            if let engine {
                let result = migo_engine_destroy(engine)
                XCTAssertEqual(result, MIGO_OK, "migo_engine_destroy returned \(result)")
                self.engine = nil

                // Asked only when a test already failed for outliving RELEASED, so
                // a green run pays nothing and reports nothing.
                if let stillAlive = layerThatOutlivedRelease {
                    XCTFail(
                        wentAtSessionDestroy
                            ? "the layer that outlived RELEASED went away at "
                                + "migo_session_destroy, before the engine was destroyed. The "
                                + "owner is inside the renderer -- the render thread, the canvas "
                                + "manager or one of their EGL contexts -- and not the display."
                            : stillAlive()
                            ? "the layer that outlived RELEASED is STILL alive after "
                                + "migo_engine_destroy returned. Nothing of Migo's is left at "
                                + "that point -- the display, its contexts and the render "
                                + "thread are all gone -- so no part of Migo was holding it, "
                                + "and the assertion at RELEASED was asking for a deallocation "
                                + "the C ABI does not promise: surface.h requires the host to "
                                + "keep the resource alive UNTIL RELEASED, not that the object "
                                + "dies then."
                            : "the layer that outlived RELEASED survived "
                                + "migo_session_destroy and went away only when "
                                + "migo_engine_destroy returned. The owner is DISPLAY-level: "
                                + "eglTerminate is what freed it, so ANGLE deferred the window "
                                + "surface's destruction past the eglDestroySurface that "
                                + "reported success.")
                }
            }
            layerThatOutlivedRelease = nil
            // Last, and only now: `migo_engine_destroy` is the thread-completion
            // barrier, and its header says only after it returns may the host
            // destroy native display or window resources.
            retained.removeAll()
            if let root { try? FileManager.default.removeItem(at: root) }
            root = nil
            try super.tearDownWithError()
        }

        /// One attach, driven from a caller-owned payload of the given kind.
        ///
        /// `platform_descriptor_size` is required to equal the typed
        /// descriptor's own `struct_size`; the header calls that duplication an
        /// intentional envelope-versus-payload cross-check, so it is written from
        /// the same expression rather than typed twice.
        ///
        /// `hostObject` is the native object the payload points at, and taking it
        /// here normally keeps a host reference until teardown sees RELEASED.
        /// The ownership regression opts out and leaves Migo's retain responsible
        /// for the layer's lifetime. A successful attachment is recorded on the
        /// fixture so it is retired before the Session is destroyed.
        private func attach<Payload>(
            kind: MigoPlatformKind,
            payload: inout Payload,
            payloadSize: UInt32,
            hostObject: AnyObject,
            retainHostObject: Bool = true
        ) -> MigoResult {
            if retainHostObject { retained.append(hostObject) }
            var produced: OpaquePointer?
            let result = withExtendedLifetime(hostObject) {
                withUnsafePointer(to: &payload) { raw -> MigoResult in
                    var descriptor = MigoSurfaceDescriptor()
                    descriptor.struct_size = UInt32(MemoryLayout<MigoSurfaceDescriptor>.size)
                    descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
                    // Generations start at 1 and must strictly increase per session.
                    descriptor.generation = 1
                    descriptor.platform_kind = kind
                    descriptor.width_pixels = 256
                    descriptor.height_pixels = 256
                    descriptor.scale_factor = 1.0
                    descriptor.color_space = MIGO_COLOR_SPACE_SRGB
                    // OPAQUE, and not PREMULTIPLIED, which is what a layer-backed
                    // renderer would reach for first. `validate_configuration`
                    // answers PREMULTIPLIED and POSTMULTIPLIED with
                    // MIGO_ERROR_UNSUPPORTED_CAPABILITY on purpose -- capability bits
                    // and modes are requirements rather than hints, and the renderer
                    // has not plumbed alpha semantics end to end -- so asking for it
                    // here would fail during configuration validation and never reach
                    // the platform layer this test is about. The same reasoning fixes
                    // `capability_flags` at zero: any non-zero bit is refused for the
                    // same documented reason.
                    descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
                    descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
                    descriptor.platform_descriptor_size = payloadSize
                    descriptor.platform_descriptor = UnsafeRawPointer(raw)
                    return migo_session_attach_surface(session, &descriptor, &produced)
                }
            }
            attachment = produced
            return result
        }
    #endif

    /// A host-owned `CAMetalLayer` is accepted, and the engine reports the
    /// attachment rather than merely not failing.
    ///
    /// Runs on both Apple platforms, against each one's own layer descriptor.
    /// It ran only on macOS until 2026-09-06, and the cost of that was not
    /// coverage in the abstract: `MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS` admits a
    /// kind only after an attach succeeded, so the iOS kind could not be admitted
    /// while the only attach that had ever run was `#if os(macOS)`. The
    /// iOS-simulator leg of `apple-sdk.yml` was already running this bundle --
    /// `xcodebuild test -scheme Migo-Package` against a real simulator -- and
    /// reporting these two cases as skipped.
    func testAHostOwnedMetalLayerAttaches() throws {
        #if os(macOS) || os(iOS)
            let layer = CAMetalLayer()
            layer.drawableSize = CGSize(width: 256, height: 256)
            layer.frame = CGRect(x: 0, y: 0, width: 256, height: 256)

            var payload = LayerPayload()
            payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
            payload.abi_version = MIGO_ABI_VERSION_CURRENT
            payload.platform_kind = layerKind
            payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()

            let result = attach(
                kind: layerKind,
                payload: &payload,
                payloadSize: payload.struct_size,
                hostObject: layer)

            XCTAssertEqual(
                result, MIGO_OK,
                """
                attaching a host-owned CAMetalLayer returned \(result). This is the \
                assertion that earns \(ledgerConstantName) its place in \
                MIGO_CAPI_IMPLEMENTED_PLATFORM_KINDS; until it passes on this runner, \
                that constant must not list it.

                What the engine said: \(engineErrors.summary)
                """)
            XCTAssertNotNil(attachment, "attach reported success and produced no attachment")
        #else
            throw XCTSkip("this package is built for macOS and iOS only")
        #endif
    }

    func testMetalLayerIsRetainedUntilNativeRetirementCompletes() throws {
        #if os(macOS) || os(iOS)
            weak var observedLayer: CAMetalLayer?
            let result = autoreleasepool { () -> MigoResult in
                let layer = CAMetalLayer()
                observedLayer = layer
                layer.drawableSize = CGSize(width: 256, height: 256)
                layer.frame = CGRect(x: 0, y: 0, width: 256, height: 256)

                var payload = LayerPayload()
                payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
                payload.abi_version = MIGO_ABI_VERSION_CURRENT
                payload.platform_kind = layerKind
                payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()
                return attach(
                    kind: layerKind, payload: &payload, payloadSize: payload.struct_size,
                    hostObject: layer, retainHostObject: false)
            }
            XCTAssertEqual(result, MIGO_OK, "attach failed: \(engineErrors.summary)")
            XCTAssertNotNil(observedLayer, "Migo must retain the layer before attach returns")

            let live = try XCTUnwrap(attachment)
            var release: OpaquePointer?
            let began = migo_surface_begin_detach(live, &release)
            XCTAssertEqual(began, MIGO_OK)
            if began == MIGO_OK { attachment = nil }
            let observer = try XCTUnwrap(release)

            var status = MigoSurfaceReleaseStatus()
            status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
            status.abi_version = MIGO_ABI_VERSION_CURRENT
            var released = false
            let deadline = Date().addingTimeInterval(5)
            while Date() < deadline {
                XCTAssertEqual(migo_surface_release_query(observer, &status), MIGO_OK)
                if status.state == MIGO_SURFACE_RELEASE_RELEASED {
                    released = true
                    break
                }
                RunLoop.current.run(until: Date().addingTimeInterval(0.001))
            }
            XCTAssertTrue(released, "native retirement must complete before releasing the layer")
            XCTAssertNil(observedLayer, "RELEASED must follow the engine's final layer release")

            // If it is still alive, say WHICH failure this is. The assertion above
            // cannot tell an ordering window from a retained reference, and the two
            // need different fixes: one is a publication that ran ahead of a drop,
            // the other is an owner nobody released. Waiting here changes no verdict
            // -- the test has already failed -- it only turns "the layer is still
            // alive" into something the next person can act on.
            //
            // Added because this assertion started failing on loaded CI runners while
            // passing in two seconds on an idle one, and a red that reports only the
            // symptom cost a full lane iteration to learn nothing from.
            if observedLayer != nil {
                // Hand the "who was holding it" question to teardown, which is the
                // only place that runs after `migo_engine_destroy` -- the point
                // every remaining piece of Migo is gone. The closure captures the
                // weak binding, so holding it here keeps nothing alive.
                layerThatOutlivedRelease = { observedLayer != nil }

                // The strong binding is scoped, and that matters: held across the
                // poll below it would keep the layer alive itself and make the
                // ordering-versus-leak answer always say "leak" -- a diagnostic
                // that reports its own effect.
                if let stillAlive = observedLayer {
                    // How many references are left, and whose. Only meaningful in a
                    // branch that has already failed, which is why it is here and not
                    // in the assertion: CFGetRetainCount is explicitly not a number to
                    // reason about in working code. As a clue it is the strongest one
                    // available -- it separates "one other owner" from "several", and
                    // the next question is which.
                    //
                    // One of the references counted is this binding: `observedLayer` is
                    // weak, and binding it takes a strong one.
                    let counted = CFGetRetainCount(stillAlive)
                    XCTFail(
                        "at the moment RELEASED was observed the layer had \(counted) reference(s), "
                            + "one of which is this test's own binding. Everything in Migo's own "
                            + "graph is accounted for -- SurfaceResource::drop releases the anchor "
                            + "before publishing, and the canvas manager holds exactly one "
                            + "PreparedEglSurfaceRef and clears it before release_onscreen returns "
                            + "-- so an owner outside that graph is the remaining candidate, ANGLE's "
                            + "own retain on the CAMetalLayer for its window surface being the first "
                            + "to check")
                }

                let observationStart = Date()
                let observationDeadline = observationStart.addingTimeInterval(2)
                while observedLayer != nil, Date() < observationDeadline {
                    RunLoop.current.run(until: Date().addingTimeInterval(0.005))
                }
                let waited = Int(Date().timeIntervalSince(observationStart) * 1000)
                if observedLayer == nil {
                    XCTFail(
                        "the layer cleared \(waited) ms AFTER RELEASED was observed, so this is an "
                            + "ordering window and not a leak: something published completion "
                            + "before the last reference went. SurfaceResource::drop orders the "
                            + "anchor drop before complete(), so the reference that outlived it is "
                            + "held somewhere else")
                } else {
                    XCTFail(
                        "the layer was still alive 2 s after RELEASED, so this is a retained "
                            + "reference rather than an ordering window: some owner was never "
                            + "released, and RELEASED reported a retirement that did not happen")
                }
            }
            XCTAssertEqual(migo_surface_release_destroy(observer), MIGO_OK)
        #else
            throw XCTSkip("this package is built for macOS and iOS only")
        #endif
    }

    /// The other half of the same decision: a view is refused, not resolved.
    ///
    /// `MigoMacosNsViewDescriptor` and `MigoIosUiViewDescriptor` are payloads the
    /// ABI parses and the platform module deliberately declines, because ANGLE
    /// handed a plain `CALayer` succeeds and quietly takes ownership of a metal
    /// layer it created itself. A refusal is the only outcome that leaves the
    /// host in charge of its own drawable, so it is asserted rather than assumed
    /// -- and asserting it here is what separates "attach works" from "attach
    /// accepts whatever it is given".
    ///
    /// WHICH refusal this reaches, precisely: the capability mask's, not the
    /// platform module's match arm. `parse_for_platforms` is handed
    /// `supported_platform_kinds()`, which carries only this build's layer kind,
    /// so a view descriptor is turned away before any Apple code sees it. The
    /// module's own arm is covered by `a_view_descriptor_is_refused_rather_than_
    /// resolved_to_a_layer` in `capi/src/platform/apple.rs`, which runs on Linux.
    /// Two independent refusals for one rule, which is the intent -- but worth
    /// naming, so nobody reads this test as proof of the arm it does not reach.
    func testAHostViewIsRefusedRatherThanResolvedToALayer() throws {
        #if os(macOS) || os(iOS)
            let view = HostView(frame: CGRect(x: 0, y: 0, width: 256, height: 256))

            var payload = ViewPayload()
            payload.struct_size = UInt32(MemoryLayout<ViewPayload>.size)
            payload.abi_version = MIGO_ABI_VERSION_CURRENT
            payload.platform_kind = viewKind
            #if os(macOS)
                payload.ns_view = Unmanaged.passUnretained(view).toOpaque()
            #else
                payload.ui_view = Unmanaged.passUnretained(view).toOpaque()
            #endif

            let result = attach(
                kind: viewKind,
                payload: &payload,
                payloadSize: payload.struct_size,
                hostObject: view)

            XCTAssertEqual(
                result, MIGO_ERROR_UNSUPPORTED_PLATFORM,
                "a \(viewTypeName) must be declined so the host keeps ownership of its drawable")
            XCTAssertNil(attachment, "a declined attach must produce no attachment")
        #else
            throw XCTSkip("this package is built for macOS and iOS only")
        #endif
    }
}
