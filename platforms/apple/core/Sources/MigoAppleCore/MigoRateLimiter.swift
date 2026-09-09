import Foundation

/// A fixed-window bound on how often content may cross into the host.
///
/// The one that exists today bounds `diagnostics.report`, which crosses the
/// script-message boundary and calls a host delegate on the main thread: content in a
/// failure loop can spend the frame budget telling the host it has no frame budget.
///
/// **Why the clock is a parameter.** A limiter that reads `Date()` itself can only be
/// tested by sleeping, so it gets tested with one call and a comment. Passing the
/// instant in makes every boundary case a plain assertion, and the caller passing
/// `Date()` is the whole cost.
///
/// **Why a fixed window and not a token bucket.** A fixed window admits up to twice
/// the rate across a boundary -- the tail of one window plus the head of the next --
/// and that is stated rather than hidden, because for this purpose it does not matter:
/// the bound exists to stop an unbounded flood, not to shape a stream. A bucket would
/// be the right answer for something metered, and this is not that.
///
/// **Why refusals are counted.** A dropped report that nobody mentions is a gap in the
/// host's log that looks like quiet. The count of what was refused travels with the
/// next admission, so the host learns it is being throttled by reading the thing it
/// did receive.
public struct MigoRateLimiter: Sendable, Equatable {

    public enum Admission: Sendable, Equatable {
        /// Let it through. `droppedSinceLast` is how many were refused since the
        /// previous admission, and zero on a quiet stream.
        case admitted(droppedSinceLast: Int)
        case refused
    }

    public let perWindow: Int
    public let window: TimeInterval

    private var windowStart: Date
    private var admittedInWindow: Int
    private var droppedSinceLastAdmission: Int

    public init(perWindow: Int, window: TimeInterval = 1) {
        precondition(perWindow >= 1, "a limiter that admits nothing is a mute channel")
        precondition(window > 0, "a window of zero admits everything or nothing, depending on ties")
        self.perWindow = perWindow
        self.window = window
        // `distantPast` so the first call opens a window rather than joining one that
        // began at an arbitrary construction time -- otherwise a limiter built and used
        // a second later starts half-spent.
        self.windowStart = .distantPast
        self.admittedInWindow = 0
        self.droppedSinceLastAdmission = 0
    }

    public mutating func admit(at now: Date) -> Admission {
        if now.timeIntervalSince(windowStart) >= window {
            windowStart = now
            admittedInWindow = 0
        }
        guard admittedInWindow < perWindow else {
            droppedSinceLastAdmission += 1
            return .refused
        }
        admittedInWindow += 1
        let dropped = droppedSinceLastAdmission
        droppedSinceLastAdmission = 0
        return .admitted(droppedSinceLast: dropped)
    }

    /// How many have been refused and not yet reported. Exposed so a host can see the
    /// backlog without waiting for an admission that a sustained flood never reaches.
    public var pendingDropCount: Int { droppedSinceLastAdmission }
}
