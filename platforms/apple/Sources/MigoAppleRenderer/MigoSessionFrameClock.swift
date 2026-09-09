import Foundation
import MigoAppleCore
import MigoEngine

#if os(macOS)
    import AppKit
#endif

/// Turns the display's own clock into the engine's frame boundary.
///
/// `include/migo/session.h` states the contract this implements: a host that
/// installs `on_request_frame` owns frame pacing, arms one callback with
/// whatever its platform provides, and calls `migo_session_notify_vsync` when it
/// fires. On Apple that platform callback is `CADisplayLink` (or `CVDisplayLink`
/// below macOS 14), which `MigoDisplayLink` already wraps -- what was missing was
/// everything between it and the ABI, so the display link had no caller.
///
/// **One frame per request, on a link that keeps running.** The ABI's unit is one
/// requested frame, so a tick with no request outstanding is dropped. What is
/// deliberately *not* done is stopping the link between frames: `CADisplayLink`
/// takes effect at the next vsync boundary, so invalidating and recreating it per
/// frame turns every frame into a two-frame round trip, and the request that
/// arrives mid-frame waits for a link that has not started yet. Leaving it
/// running costs one main-queue callback per vsync that returns immediately.
///
/// The alternative -- notifying on every tick whether or not a frame was asked
/// for -- is not a setting here. Android measured it and preferred it (a
/// demand-driven frame callback left the compute thread idle enough for the
/// governor to drop the clock), but that measurement was taken on a different
/// scheduler with a different frame callback, and this project's rule is that a
/// number from one platform is a hypothesis on another. It is a two-line change
/// when somebody measures it on a phone.
///
/// **Ownership.** The clock does not own the session and does not extend its
/// life: a `MigoSession *` is destroyed by whoever created it. Stop the clock
/// before `migo_session_destroy`. `deinit` stops it too, but a clock that is
/// still ticking when its session is destroyed has already lost that race.
public final class MigoSessionFrameClock {

    /// What a run of this clock did, for a host that has to explain a frame rate.
    public struct Statistics: Equatable, Sendable {
        /// Vsyncs the display delivered.
        public var ticks: Int = 0
        /// Ticks that carried a requested frame to the engine.
        public var delivered: Int = 0
        /// Ticks with no frame outstanding. A large share of these is content
        /// that is not animating, not a fault.
        public var idle: Int = 0
        /// Requests that arrived while one was already outstanding. The ABI's
        /// unit is one frame, so these collapse into the pending one rather than
        /// queueing -- a queue of frame requests is a queue of stale timestamps.
        public var coalescedRequests: Int = 0
        /// Ticks whose timestamp could not be expressed for the ABI.
        public var unusableTimestamps: Int = 0
        /// Deliveries the engine refused.
        public var refused: Int = 0
        /// The last non-`MIGO_OK` the engine returned, if any.
        public var lastRefusal: MigoResult?
    }

    /// The call this clock makes on each delivered frame.
    ///
    /// Injected so the pacing and the arithmetic are testable without an engine:
    /// what is worth testing here is when a tick becomes a frame, and a live
    /// session is not part of that question.
    typealias Notify = (Int64) -> MigoResult

    private let link: MigoDisplayLink
    private let notify: Notify

    /// Guards the request flag and the statistics.
    ///
    /// `on_request_frame` is called on the engine's thread and ticks arrive on
    /// the main queue, so the flag genuinely crosses threads. A lock and not an
    /// atomic: this is two uncontended acquisitions per vsync, which at 120 Hz is
    /// a few hundred nanoseconds a second, and an atomic here would buy nothing
    /// that could be measured while costing the ability to update the counters
    /// and the flag as one step.
    private let lock = NSLock()
    private var frameRequested = false
    private var statistics = Statistics()

    /// A clock for a live session.
    ///
    /// `session` is borrowed, not retained. See the type's ownership note.
    public convenience init(session: OpaquePointer, decision: MigoDisplayLinkPolicy.Decision) {
        self.init(decision: decision) { nanoseconds in
            migo_session_notify_vsync(session, nanoseconds)
        }
    }

    init(decision: MigoDisplayLinkPolicy.Decision, notify: @escaping Notify) {
        self.notify = notify
        var deliver: ((CFTimeInterval, CFTimeInterval) -> Void)?
        self.link = MigoDisplayLink(decision: decision) { target, duration in
            deliver?(target, duration)
        }
        // Assigned after `self` exists, and captured weakly: `MigoDisplayLink`
        // holds this closure for as long as it lives, and this object holds the
        // link. A strong capture closes that cycle and the clock never
        // deallocates -- the same cycle MigoDisplayLinkProxy exists to break one
        // level down.
        deliver = { [weak self] target, _ in
            self?.tick(targetTimestamp: target)
        }
    }

    deinit {
        link.stop()
    }

    /// Whether the display link is delivering ticks.
    public var isRunning: Bool { link.isRunning }

    /// A snapshot of what this clock has done.
    public var currentStatistics: Statistics {
        lock.lock()
        defer { lock.unlock() }
        return statistics
    }

    /// The body of the engine's `on_request_frame`.
    ///
    /// Safe from any thread, and cheap: it sets one flag. A second request
    /// before the first is served is counted and dropped, because the ABI's unit
    /// is one frame and the engine will ask again.
    public func requestFrame() {
        lock.lock()
        if frameRequested {
            statistics.coalescedRequests += 1
        }
        frameRequested = true
        lock.unlock()
    }

    #if os(iOS)
        /// Start delivering ticks. Idempotent.
        public func start() {
            link.start()
        }
    #elseif os(macOS)
        /// Start delivering ticks. Idempotent.
        ///
        /// `view` is passed through to `MigoDisplayLink`, which needs it on macOS
        /// 14 and later to follow the right display; without one it falls back to
        /// `CVDisplayLink` rather than guessing which screen the content is on.
        public func start(view: NSView? = nil) {
            link.start(view: view)
        }
    #endif

    /// Stop delivering ticks, and forget any outstanding request.
    ///
    /// The request is dropped rather than held: it was for a frame at a
    /// timestamp that has passed, and serving it on the next start would present
    /// content paced against a clock that stopped.
    public func stop() {
        link.stop()
        lock.lock()
        frameRequested = false
        lock.unlock()
    }

    /// One vsync. Called on the main queue by `MigoDisplayLink`.
    func tick(targetTimestamp: CFTimeInterval) {
        lock.lock()
        statistics.ticks += 1
        let wanted = frameRequested
        if !wanted {
            statistics.idle += 1
            lock.unlock()
            return
        }
        frameRequested = false
        lock.unlock()

        switch MigoVsyncTimestamp.nanoseconds(fromSeconds: targetTimestamp) {
        case .failure:
            // The frame stays requested: the engine asked for one and this tick
            // could not carry it, so dropping the request would make the engine
            // wait for a request it has already made.
            lock.lock()
            statistics.unusableTimestamps += 1
            frameRequested = true
            lock.unlock()
        case .success(let nanoseconds):
            let result = notify(nanoseconds)
            lock.lock()
            if result == MIGO_OK {
                statistics.delivered += 1
            } else {
                statistics.refused += 1
                statistics.lastRefusal = result
            }
            lock.unlock()
        }
    }
}
