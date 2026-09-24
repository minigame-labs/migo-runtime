import Foundation
import MigoAppleCore
import MigoEngine
import QuartzCore
import Security

#if os(macOS)
    import AppKit
    private typealias LayerPayload = MigoMacosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_MACOS_CA_METAL_LAYER
#elseif os(iOS)
    import UIKit
    private typealias LayerPayload = MigoIosMetalLayerDescriptor
    private let layerKind = MIGO_PLATFORM_IOS_CA_METAL_LAYER
#endif

/// One engine, one session, and the layer it presents to, driven through the C
/// ABI in the order `include/migo/session.h` and `surface.h` require.
///
/// Both native lanes need exactly this and nothing lane-specific: the macOS V8
/// product runs content in process on it, and the iOS Performance+ product
/// runs content in WebContent and hands this session the frames. What differs
/// between them -- who evaluates the game -- is above this type, in each lane's
/// `MigoGameView`. So it lives here, in the target both lanes already share,
/// rather than as two copies that would each get the teardown order right or
/// wrong on their own.
///
/// ## Threads
///
/// Main thread only. Every host callback is dispatched onto the main queue --
/// the dispatcher is the host's choice under the ABI, and the main queue is the
/// only one a view can act on -- and the display link that answers frame
/// requests ticks there too, so the session is never driven from two threads
/// and the ABI's "calls through a single Session are the host's to serialise"
/// holds by construction.
///
/// ## Lifetime
///
/// `close(completion:)` is the only way out, and it is asynchronous because the
/// ABI's is: a surface is retired, the renderer reports it RELEASED some time
/// later, and only then may the session and the engine be destroyed and the
/// layer let go. `deinit` does not do it -- a deinit cannot wait, and one that
/// destroyed a session with a surface still PENDING would be refused and leak,
/// silently. A session dropped without `close` is a programming error and is
/// reported as one.
public final class MigoEngineSession {

    /// Why a call did not happen.
    public enum Failure: Error, CustomStringConvertible, Equatable {
        /// The engine refused a call; its name and what it returned.
        case call(String, MigoResult)
        /// A signing key that is not 32 bytes, or is all zero.
        case invalidSigningKey(Int)
        /// The system could not supply the 16 random bytes that identify a session.
        case noRandomness(OSStatus)
        /// A method was called after `close`.
        case closed

        public var description: String {
            switch self {
            case .call(let name, let result): return "\(name) returned \(result)"
            case .noRandomness(let status):
                return "SecRandomCopyBytes failed (\(status)); a session identity cannot be guessed"
            case .closed: return "the session has been closed"
            case .invalidSigningKey(let count):
                return "a content signing key is 32 raw Ed25519 bytes and not all zero; got \(count) bytes"
            }
        }
    }

    /// What the engine told the host, on the main queue.
    public enum Event: Equatable {
        /// The content was evaluated.
        case ready
        /// A runtime error or a pressure notice. `recoverable` is the engine's
        /// `MIGO_ERROR_FLAG_RECOVERABLE`.
        case error(code: MigoResult, message: String, recoverable: Bool)
        /// Content called `migo.exitMiniProgram()`.
        case exitRequested
        /// The surface of this generation stopped being presentable.
        case surfaceLost(generation: UInt64, reason: MigoSurfaceLossReason)
    }

    /// Where the engine keeps what it writes.
    ///
    /// The engine's own layout below each root -- installed games under
    /// `files/migo/games/<id>/code`, each game's user data beside it -- is
    /// `MigoContentDescriptor`'s, so a host that moves these moves every game's
    /// saves with them.
    public struct Directories: Equatable, Sendable {
        public var files: URL
        public var cache: URL
        public var codeCache: URL

        public init(files: URL, cache: URL, codeCache: URL) {
            self.files = files
            self.cache = cache
            self.codeCache = codeCache
        }

        /// Application Support for what must survive, Caches for what the
        /// system may reclaim: a player's saves are the first and a compiled-
        /// code cache is the second, and putting them in one place would either
        /// back up megabytes of regenerable code or let the system delete saves.
        public static func standard(namespace: String = "Migo") throws -> Directories {
            let manager = FileManager.default
            let support = try manager.url(
                for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil,
                create: true)
            let caches = try manager.url(
                for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            return Directories(
                files: support.appendingPathComponent(namespace, isDirectory: true)
                    .appendingPathComponent("files", isDirectory: true),
                cache: caches.appendingPathComponent(namespace, isDirectory: true)
                    .appendingPathComponent("cache", isDirectory: true),
                codeCache: caches.appendingPathComponent(namespace, isDirectory: true)
                    .appendingPathComponent("code-cache", isDirectory: true))
        }

        /// Where the engine looks for an installed game's code.
        public func installedCode(contentID: String) -> URL {
            files.appendingPathComponent("migo/games", isDirectory: true)
                .appendingPathComponent(contentID, isDirectory: true)
                .appendingPathComponent("code", isDirectory: true)
        }
    }

    /// Whether content must be signed, and with what.
    ///
    /// A choice with no default, because both defaults are wrong for someone:
    /// verifying with no key starts nothing (the engine fails closed), and not
    /// verifying runs whatever is in the install directory.
    public enum ContentSigning: Equatable, Sendable {
        /// Every package must carry `manifest.json` (each file's SHA-256) and
        /// `manifest.sig` (the raw Ed25519 signature of its bytes) made with
        /// the private half of this 32-byte raw public key. The engine verifies
        /// a package in full on its first launch and seals the result.
        case verified(publicKey: Data)
        /// Load content with no signature. Right for packages that ship inside
        /// the app's own signed bundle, which the platform already verifies.
        case unsigned
    }

    /// Size of a surface, in the units the ABI asks for.
    public struct SurfaceSize: Equatable, Sendable {
        public var widthPixels: UInt32
        public var heightPixels: UInt32
        /// Physical pixels per CSS pixel: what input is divided by.
        public var scale: Float

        public init(widthPixels: UInt32, heightPixels: UInt32, scale: Float) {
            self.widthPixels = widthPixels
            self.heightPixels = heightPixels
            self.scale = scale
        }
    }

    /// The raw session, for the lane above this type -- the Performance+ frame
    /// channel is bound to it. Not for a host: nothing a host needs is missing
    /// from this type's own methods.
    public let session: OpaquePointer

    /// The 16 bytes the session was created with. The Performance+ producer
    /// stamps them on every packet; the V8 lane has no producer and ignores them.
    public let launchNonce: [UInt8]

    public let directories: Directories

    /// Called on the main queue for everything the engine reports.
    public var onEvent: ((Event) -> Void)?

    /// The generation of the attached surface, or 0 before the first attach.
    public private(set) var surfaceGeneration: UInt64 = 0
    /// What the attached surface was last told it is.
    public private(set) var surfaceSize: SurfaceSize?

    private let engine: OpaquePointer
    private let relay: CallbackRelay
    private let relayHandle: Unmanaged<CallbackRelay>
    private let clock: MigoSessionFrameClock
    private var attachment: OpaquePointer?
    private var layer: CAMetalLayer?
    private var metricsGeneration: UInt64 = 0
    private var isClosed = false

    /// Create the engine and the session and install the host callbacks.
    ///
    /// Callbacks are installed here and nowhere else because the ABI accepts
    /// them once, before the first attach; a type that let them be set later
    /// would be a type whose second setter silently did nothing.
    public init(
        directories: Directories, contentSigning: ContentSigning, preferredFramesPerSecond: Int? = nil
    ) throws {
        var signingKey = [UInt8](repeating: 0, count: 32)
        var engineFlags = MIGO_ENGINE_FLAG_NONE
        switch contentSigning {
        case .verified(let key):
            guard key.count == 32, key.contains(where: { $0 != 0 }) else {
                throw Failure.invalidSigningKey(key.count)
            }
            signingKey = Array(key)
        case .unsigned:
            engineFlags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT
        }
        for directory in [directories.files, directories.cache, directories.codeCache] {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        }
        self.directories = directories

        var nonce = [UInt8](repeating: 0, count: 16)
        let status = SecRandomCopyBytes(kSecRandomDefault, nonce.count, &nonce)
        guard status == errSecSuccess else { throw Failure.noRandomness(status) }
        // All zero is the ABI's "no producer". Sixteen random bytes land there
        // with probability 2^-128; checked anyway, because the cost of the check
        // is nothing and the cost of that session is a lane that refuses every frame.
        if nonce.allSatisfy({ $0 == 0 }) { nonce[0] = 1 }
        self.launchNonce = nonce

        var created: OpaquePointer?
        var engineResult = MIGO_OK
        directories.files.path.withCString { files in
            directories.cache.path.withCString { cache in
                directories.codeCache.path.withCString { codeCache in
                    var config = MigoEngineConfig()
                    config.struct_size = UInt32(MemoryLayout<MigoEngineConfig>.size)
                    config.abi_version = MIGO_ABI_VERSION_CURRENT
                    config.files_dir_utf8 = files
                    config.cache_dir_utf8 = cache
                    config.code_cache_dir_utf8 = codeCache
                    config.flags = engineFlags
                    withUnsafeMutableBytes(of: &config.code_signing_public_key) { $0.copyBytes(from: signingKey) }
                    engineResult = migo_engine_create(&config, &created)
                }
            }
        }
        guard engineResult == MIGO_OK else { throw Failure.call("migo_engine_create", engineResult) }
        guard let engine = created else { throw Failure.call("migo_engine_create", MIGO_ERROR_INTERNAL) }

        var sessionConfig = MigoSessionConfig()
        sessionConfig.struct_size = UInt32(MemoryLayout<MigoSessionConfig>.size)
        sessionConfig.abi_version = MIGO_ABI_VERSION_CURRENT
        withUnsafeMutableBytes(of: &sessionConfig.launch_nonce) { $0.copyBytes(from: nonce) }
        var startedSession: OpaquePointer?
        let sessionResult = migo_session_create(engine, &sessionConfig, &startedSession)
        guard sessionResult == MIGO_OK, let session = startedSession else {
            _ = migo_engine_destroy(engine)
            throw Failure.call("migo_session_create", sessionResult == MIGO_OK ? MIGO_ERROR_INTERNAL : sessionResult)
        }
        self.engine = engine
        self.session = session
        self.clock = MigoSessionFrameClock(
            session: session, decision: Self.displayLinkDecision(preferredFramesPerSecond))

        // Retained for as long as the session can call back, and released only
        // after it is destroyed -- see `close`. The relay holds this object
        // weakly, so it never keeps a closed session alive.
        let relay = CallbackRelay()
        self.relay = relay
        self.relayHandle = Unmanaged.passRetained(relay)
        relay.clock = clock
        relay.owner = self

        var callbacks = MigoHostCallbacks()
        callbacks.struct_size = UInt32(MemoryLayout<MigoHostCallbacks>.size)
        callbacks.abi_version = MIGO_ABI_VERSION_CURRENT
        callbacks.user_data = relayHandle.toOpaque()
        callbacks.dispatch = migoDispatchToMain
        callbacks.on_ready = migoOnReady
        callbacks.on_error = migoOnError
        callbacks.on_exit_requested = migoOnExitRequested
        callbacks.on_surface_lost = migoOnSurfaceLost
        callbacks.on_request_frame = migoOnRequestFrame
        callbacks.on_surface_released = migoOnSurfaceReleased
        let installed = migo_session_set_host_callbacks(session, &callbacks)
        guard installed == MIGO_OK else {
            _ = migo_session_destroy(session)
            _ = migo_engine_destroy(engine)
            relayHandle.release()
            throw Failure.call("migo_session_set_host_callbacks", installed)
        }
    }

    deinit {
        if !isClosed {
            // Not recoverable from here: see the type's lifetime note.
            NSLog("MigoEngineSession was released without close(); its engine is leaked")
        }
    }

    // MARK: - surface

    /// Attach `layer` as this session's surface.
    ///
    /// One attachment at a time, as the ABI allows. The layer is retained until
    /// the renderer reports it released, because destroying it earlier is a
    /// use-after-free inside the driver the engine cannot see.
    public func attach(layer: CAMetalLayer, size: SurfaceSize) throws {
        guard !isClosed else { throw Failure.closed }
        let generation = surfaceGeneration + 1
        var payload = LayerPayload()
        payload.struct_size = UInt32(MemoryLayout<LayerPayload>.size)
        payload.abi_version = MIGO_ABI_VERSION_CURRENT
        payload.platform_kind = layerKind
        payload.ca_metal_layer = Unmanaged.passUnretained(layer).toOpaque()
        let payloadSize = payload.struct_size

        var produced: OpaquePointer?
        let result = withUnsafeMutablePointer(to: &payload) { raw -> MigoResult in
            var descriptor = MigoSurfaceDescriptor()
            descriptor.struct_size = UInt32(MemoryLayout<MigoSurfaceDescriptor>.size)
            descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
            descriptor.generation = generation
            descriptor.platform_kind = layerKind
            descriptor.width_pixels = size.widthPixels
            descriptor.height_pixels = size.heightPixels
            descriptor.scale_factor = size.scale
            descriptor.color_space = MIGO_COLOR_SPACE_SRGB
            descriptor.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
            descriptor.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
            descriptor.platform_descriptor_size = payloadSize
            descriptor.platform_descriptor = UnsafeRawPointer(raw)
            return migo_session_attach_surface(session, &descriptor, &produced)
        }
        guard result == MIGO_OK, let produced else {
            throw Failure.call("migo_session_attach_surface", result == MIGO_OK ? MIGO_ERROR_INTERNAL : result)
        }
        attachment = produced
        self.layer = layer
        surfaceGeneration = generation
        surfaceSize = size
        metricsGeneration = 0
    }

    /// Tell the renderer the attached surface changed size or scale.
    ///
    /// A no-op when nothing changed, so a host can call it from every layout
    /// pass. `migo_surface_update` numbers updates per attachment; the counter
    /// restarts with each attach.
    public func resize(to size: SurfaceSize) throws {
        guard !isClosed else { throw Failure.closed }
        guard let attachment, size != surfaceSize else { return }
        metricsGeneration += 1
        var metrics = MigoSurfaceMetrics()
        metrics.struct_size = UInt32(MemoryLayout<MigoSurfaceMetrics>.size)
        metrics.abi_version = MIGO_ABI_VERSION_CURRENT
        metrics.generation = metricsGeneration
        metrics.width_pixels = size.widthPixels
        metrics.height_pixels = size.heightPixels
        metrics.scale_factor = size.scale
        metrics.color_space = MIGO_COLOR_SPACE_SRGB
        metrics.alpha_mode = MIGO_ALPHA_MODE_OPAQUE
        metrics.preferred_presentation_mode = MIGO_PRESENTATION_MODE_DEFAULT
        let result = migo_surface_update(attachment, &metrics)
        guard result == MIGO_OK else { throw Failure.call("migo_surface_update", result) }
        surfaceSize = size
    }

    // MARK: - content

    /// Evaluate (V8) or mount (Performance+) an installed game.
    ///
    /// After `attach`: on the V8 lane content is evaluated on the thread the
    /// render target owns, and the engine refuses a load with no surface.
    public func loadContent(id: String, entry: String) throws {
        guard !isClosed else { throw Failure.closed }
        let result = id.withCString { idText in
            entry.withCString { entryText -> MigoResult in
                var descriptor = MigoContentDescriptor()
                descriptor.struct_size = UInt32(MemoryLayout<MigoContentDescriptor>.size)
                descriptor.abi_version = MIGO_ABI_VERSION_CURRENT
                descriptor.content_id_utf8 = idText
                descriptor.entry_utf8 = entryText
                return migo_session_load_content(session, &descriptor)
            }
        }
        guard result == MIGO_OK else { throw Failure.call("migo_session_load_content", result) }
    }

    // MARK: - lifecycle

    /// Start answering the engine's frame requests from the display.
    #if os(iOS)
        public func startFrames() {
            guard !isClosed else { return }
            clock.start()
        }
    #elseif os(macOS)
        public func startFrames(view: NSView?) {
            guard !isClosed else { return }
            clock.start(view: view)
        }
    #endif

    public func stopFrames() {
        clock.stop()
    }

    /// What the frame clock has done, for a host explaining a frame rate.
    public var frameStatistics: MigoSessionFrameClock.Statistics { clock.currentStatistics }

    /// RUNNING or PAUSED: content hears `onShow`/`onHide`, and audio stops.
    public func setRunning(_ running: Bool) {
        guard !isClosed else { return }
        _ = migo_session_set_lifecycle(session, running ? MIGO_LIFECYCLE_RUNNING : MIGO_LIFECYCLE_PAUSED)
    }

    public func setVisible(_ visible: Bool) {
        guard !isClosed else { return }
        _ = migo_session_set_visibility(session, visible ? 1 : 0)
    }

    /// Losing focus retracts every touch and key still down, which is what a
    /// system gesture or an alert over the game needs.
    public func setFocused(_ focused: Bool) {
        guard !isClosed else { return }
        _ = migo_session_set_focus(session, focused ? 1 : 0)
    }

    // MARK: - input

    /// One touch event, points in CSS pixels. Returns the engine's answer;
    /// `MIGO_ERROR_WOULD_BLOCK` is backpressure, not a fault.
    @discardableResult
    public func sendTouch(
        _ type: MigoTouchType, points: [MigoTouchPoint], timestampMilliseconds: Int64
    ) -> MigoResult {
        guard !isClosed, !points.isEmpty, points.count <= Int(MIGO_TOUCH_MAX_POINTS) else {
            return MIGO_ERROR_INVALID_ARGUMENT
        }
        var event = MigoTouchEvent()
        event.struct_size = UInt32(MemoryLayout<MigoTouchEvent>.size)
        event.abi_version = MIGO_ABI_VERSION_CURRENT
        event.type = type
        event.point_count = UInt32(points.count)
        event.timestamp_ms = timestampMilliseconds
        return points.withUnsafeBufferPointer { buffer -> MigoResult in
            event.points = buffer.baseAddress
            return migo_session_send_touch(session, &event)
        }
    }

    /// One mouse event, in CSS pixels. `button` is DOM `MouseEvent.button`.
    @discardableResult
    public func sendPointer(
        _ type: MigoPointerEventType, button: UInt32, x: Float, y: Float, timestampMilliseconds: Double
    ) -> MigoResult {
        guard !isClosed else { return MIGO_ERROR_INVALID_STATE }
        var event = MigoPointerEvent()
        event.struct_size = UInt32(MemoryLayout<MigoPointerEvent>.size)
        event.abi_version = MIGO_ABI_VERSION_CURRENT
        event.event_type = type
        event.button = button
        event.x = x
        event.y = y
        event.timestamp_ms = timestampMilliseconds
        return migo_session_send_pointer_event(session, &event)
    }

    /// One wheel event, deltas in CSS pixels. The wheel carries no position:
    /// content reads it from the pointer stream, as the DOM does.
    @discardableResult
    public func sendWheel(deltaX: Double, deltaY: Double, timestampMilliseconds: Double) -> MigoResult {
        guard !isClosed else { return MIGO_ERROR_INVALID_STATE }
        var event = MigoWheelEvent()
        event.struct_size = UInt32(MemoryLayout<MigoWheelEvent>.size)
        event.abi_version = MIGO_ABI_VERSION_CURRENT
        event.delta_mode = MIGO_WHEEL_DELTA_MODE_PIXEL
        event.delta_x = deltaX
        event.delta_y = deltaY
        event.timestamp_ms = timestampMilliseconds
        return migo_session_send_wheel_event(session, &event)
    }

    /// One physical key. `key` and `code` are DOM values -- `"a"`/`"KeyA"` --
    /// and are not interchangeable; see `input.h`.
    @discardableResult
    public func sendKey(
        _ type: MigoKeyEventType, key: String, code: String, modifiers: MigoKeyModifiers,
        isRepeat: Bool, timestampMilliseconds: Double
    ) -> MigoResult {
        guard !isClosed, !code.isEmpty else { return MIGO_ERROR_INVALID_ARGUMENT }
        var keyBytes = Array(key.utf8)
        var codeBytes = Array(code.utf8)
        return keyBytes.withUnsafeMutableBufferPointer { keyBuffer in
            codeBytes.withUnsafeMutableBufferPointer { codeBuffer -> MigoResult in
                var event = MigoKeyEvent()
                event.struct_size = UInt32(MemoryLayout<MigoKeyEvent>.size)
                event.abi_version = MIGO_ABI_VERSION_CURRENT
                event.event_type = type
                // A zero-length key still needs a pointer the engine may read
                // nothing from; the code buffer is non-empty by the guard above.
                event.key_utf8 = UnsafeRawPointer(keyBuffer.baseAddress ?? codeBuffer.baseAddress!)
                    .assumingMemoryBound(to: CChar.self)
                event.key_length = UInt32(keyBuffer.count)
                event.code_utf8 = UnsafeRawPointer(codeBuffer.baseAddress!).assumingMemoryBound(to: CChar.self)
                event.code_length = UInt32(codeBuffer.count)
                event.timestamp_ms = timestampMilliseconds
                event.modifiers = modifiers
                event.flags = isRepeat ? MIGO_KEY_EVENT_FLAG_REPEAT : MIGO_KEY_EVENT_FLAG_NONE
                return migo_session_send_key_event(session, &event)
            }
        }
    }

    // MARK: - teardown

    /// Retire the surface, wait for the renderer to release it, then destroy the
    /// session and the engine. `completion` runs on the main queue with whether
    /// the surface reached RELEASED within `timeout`; when it did not, the
    /// session and the engine are deliberately leaked rather than destroyed,
    /// because destroying them would be refused and freeing the layer would be
    /// the use-after-free the ABI warns about.
    public func close(timeout: TimeInterval = 10, completion: ((Bool) -> Void)? = nil) {
        guard !isClosed else {
            completion?(true)
            return
        }
        isClosed = true
        clock.stop()
        onEvent = nil
        guard let live = attachment else {
            finish(released: true, completion: completion)
            return
        }
        attachment = nil
        var observer: OpaquePointer?
        let begun = migo_surface_begin_detach(live, &observer)
        guard begun == MIGO_OK, let observer else {
            NSLog("MigoEngineSession: migo_surface_begin_detach returned \(begun)")
            finish(released: false, completion: completion)
            return
        }
        let deadline = Date().addingTimeInterval(timeout)
        waitForRelease(observer, deadline: deadline, completion: completion)
    }

    /// Polled on the main run loop rather than blocked on: the renderer's
    /// release can need the main thread (Core Animation hands drawables back
    /// there), so a main thread that sleeps here can wait on itself.
    private func waitForRelease(
        _ observer: OpaquePointer, deadline: Date, completion: ((Bool) -> Void)?
    ) {
        var status = MigoSurfaceReleaseStatus()
        status.struct_size = UInt32(MemoryLayout<MigoSurfaceReleaseStatus>.size)
        status.abi_version = MIGO_ABI_VERSION_CURRENT
        let queried = migo_surface_release_query(observer, &status)
        if queried == MIGO_OK, status.state == MIGO_SURFACE_RELEASE_RELEASED {
            _ = migo_surface_release_destroy(observer)
            finish(released: true, completion: completion)
            return
        }
        if queried != MIGO_OK || Date() >= deadline {
            NSLog("MigoEngineSession: the surface was not released (query \(queried)); leaking the session")
            finish(released: false, completion: completion)
            return
        }
        // The release edge callback wakes this early; the timer is the floor.
        relay.releaseWaiter = { [weak self] in
            self?.waitForRelease(observer, deadline: deadline, completion: completion)
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(4)) { [weak self] in
            guard let self, let waiter = self.relay.releaseWaiter else { return }
            self.relay.releaseWaiter = nil
            waiter()
        }
    }

    private func finish(released: Bool, completion: ((Bool) -> Void)?) {
        relay.releaseWaiter = nil
        if released {
            _ = migo_session_destroy(session)
            let destroyed = migo_engine_destroy(engine)
            if destroyed != MIGO_OK { NSLog("MigoEngineSession: migo_engine_destroy returned \(destroyed)") }
            layer = nil
            // Tasks already queued on the main queue were cancelled by the
            // destroy, but they still run -- to find out they were cancelled --
            // and they carry the relay's address. One more turn of the queue
            // and they have all run.
            let handle = relayHandle
            DispatchQueue.main.async { handle.release() }
        }
        completion?(released)
    }

    // MARK: - callbacks

    fileprivate func deliver(_ event: Event) {
        guard !isClosed else { return }
        onEvent?(event)
    }

    private static func displayLinkDecision(_ preferred: Int?) -> MigoDisplayLinkPolicy.Decision {
        let major = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        #if os(iOS)
            let maximum = UIScreen.main.maximumFramesPerSecond
            let minimumDisabled =
                (Bundle.main.object(forInfoDictionaryKey: "CADisableMinimumFrameDurationOnPhone") as? Bool) == true
            return MigoDisplayLinkPolicy.decide(
                .init(
                    platform: .iOS, osMajor: major, promotionRequested: (preferred ?? 60) > 60,
                    targetFramesPerSecond: preferred, displayMaximumFramesPerSecond: maximum,
                    minimumFrameDurationDisabled: minimumDisabled))
        #else
            let maximum: Int
            if #available(macOS 12.0, *) {
                maximum = NSScreen.main?.maximumFramesPerSecond ?? 60
            } else {
                maximum = 60
            }
            return MigoDisplayLinkPolicy.decide(
                .init(
                    platform: .macOS, osMajor: major, promotionRequested: (preferred ?? 60) > 60,
                    targetFramesPerSecond: preferred, displayMaximumFramesPerSecond: maximum))
        #endif
    }
}

/// What the C callbacks reach. Separate from the session so the session can be
/// held weakly: the engine holds `user_data` for as long as it lives, and a
/// strong pointer there would keep a closed session alive through it.
private final class CallbackRelay {
    weak var owner: MigoEngineSession?
    var clock: MigoSessionFrameClock?
    var releaseWaiter: (() -> Void)?
}

private func relay(_ userData: UnsafeMutableRawPointer?) -> CallbackRelay? {
    guard let userData else { return nil }
    return Unmanaged<CallbackRelay>.fromOpaque(userData).takeUnretainedValue()
}

/// Every task onto the main queue. The ABI lets the host pick the thread and
/// requires exactly one invocation; `async` is exactly one.
private let migoDispatchToMain: MigoDispatchFn = { _, task, context in
    guard let task else { return MIGO_ERROR_DISPATCH_REJECTED }
    let address = UInt(bitPattern: context)
    DispatchQueue.main.async { task(UnsafeMutableRawPointer(bitPattern: address)) }
    return MIGO_OK
}

private let migoOnReady: MigoOnReadyFn = { userData, _ in
    relay(userData)?.owner?.deliver(.ready)
}

private let migoOnError: MigoOnErrorFn = { userData, _, error in
    guard let error else { return }
    let record = error.pointee
    var message = ""
    if let text = record.message_utf8, record.message_length > 0 {
        let bytes = UnsafeRawBufferPointer(start: text, count: Int(record.message_length))
        message = String(decoding: bytes, as: UTF8.self)
    }
    relay(userData)?.owner?.deliver(
        .error(
            code: record.code, message: message,
            recoverable: record.flags & MIGO_ERROR_FLAG_RECOVERABLE != 0))
}

private let migoOnExitRequested: MigoOnExitRequestedFn = { userData, _ in
    relay(userData)?.owner?.deliver(.exitRequested)
}

private let migoOnSurfaceLost: MigoOnSurfaceLostFn = { userData, _, generation, reason in
    relay(userData)?.owner?.deliver(.surfaceLost(generation: generation, reason: reason))
}

private let migoOnRequestFrame: MigoOnRequestFrameFn = { userData, _ in
    relay(userData)?.clock?.requestFrame()
}

private let migoOnSurfaceReleased: MigoOnSurfaceReleasedFn = { userData, _, _ in
    guard let relay = relay(userData), let waiter = relay.releaseWaiter else { return }
    relay.releaseWaiter = nil
    waiter()
}
