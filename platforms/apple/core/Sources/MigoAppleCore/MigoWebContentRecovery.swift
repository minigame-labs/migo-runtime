import Foundation

/// What happens when WebKit's content process dies under a running session.
///
/// The rule comes from `contracts/apple/profile-policy.json`'s
/// `webcontent_terminated` row: the current session voids its generation, drops
/// unacknowledged packets and shows recovery UI, and the next session rebuilds the
/// same lane -- or drops to the compatibility lane if it keeps happening. This type
/// is that row as a state machine, in the package a pull request compiles, because
/// the wiring that consumes it lives behind an xcframework that only the SDK lane
/// builds.
///
/// Two things it exists to get right, both of which have already been got wrong
/// once in this repository on the Android side:
///
///   * **Fencing.** A generation that has been voided must not accept anything
///     addressed to it. After a rebuild there are two conversations in flight and
///     only one of them is with a live web view; a late reply from the dead one is
///     indistinguishable from a fresh one unless the generation travels with it.
///     The Android manager posted events to a runtime that had already gone away,
///     and the fix there was the same: carry the generation, compare it, drop the
///     stale.
///
///   * **What resets the streak.** A crash loop is only detectable if the counter
///     survives the rebuild that follows it. Resetting on the rebuild -- the
///     obvious place, because that is where the code is -- makes every crash the
///     first crash, and the limit is then never reached however many times content
///     dies. Only content reporting itself ready clears it.
public struct MigoWebContentRecovery: Sendable, Equatable {

    /// What the host should do about a termination.
    public enum Outcome: Sendable, Equatable {
        /// Rebuild the web view under this new generation. Anything still
        /// addressed to an older one is no longer deliverable.
        case rebuild(generation: UInt64)
        /// The content process has died this many times without ever reaching
        /// ready. Rebuilding again would be the same experiment; the host shows a
        /// terminal failure and the reason code says which one.
        case stop(afterConsecutiveTerminations: Int)
    }

    /// How many terminations without an intervening ready are tolerated.
    ///
    /// Not a tuning knob with a comfortable default: content that dies twice
    /// before it ever finished starting is content that will die a third time, and
    /// a host that keeps rebuilding turns one failure into a battery complaint. The
    /// policy contract's wording is "rebuild the same lane, or ios_webkit_full if
    /// it recurs" -- in this lane there is nowhere lower to go, so recurrence stops
    /// instead of demoting.
    public let terminationBudget: Int

    /// The live generation. Strictly increasing; a value is never reused, so a
    /// stale message can always be recognised rather than merely being unlikely.
    public private(set) var generation: UInt64

    /// Terminations since content last reported itself ready.
    public private(set) var consecutiveTerminations: Int

    /// Whether the current generation has been voided and not yet rebuilt.
    public private(set) var isVoided: Bool

    public init(terminationBudget: Int = 2) {
        precondition(terminationBudget >= 1, "a budget below one never rebuilds anything")
        self.terminationBudget = terminationBudget
        self.generation = 1
        self.consecutiveTerminations = 0
        self.isVoided = false
    }

    /// The content process died. Voids the current generation and says what next.
    public mutating func webContentTerminated() -> Outcome {
        consecutiveTerminations += 1
        isVoided = true
        if consecutiveTerminations >= terminationBudget {
            // Deliberately no generation bump: nothing is being rebuilt, so a new
            // generation would be a conversation with nobody, and a later stale
            // reply would find a generation that looks current.
            return .stop(afterConsecutiveTerminations: consecutiveTerminations)
        }
        generation += 1
        isVoided = false
        return .rebuild(generation: generation)
    }

    /// Content in the live generation reported itself ready.
    ///
    /// Ignored when it arrives from a generation that is no longer live, which is
    /// the case that matters: a dying web view can finish sending a ready message
    /// after its replacement has been built, and accepting it would clear the very
    /// streak that is about to stop the loop.
    @discardableResult
    public mutating func contentBecameReady(generation reported: UInt64) -> Bool {
        guard accepts(generation: reported) else { return false }
        consecutiveTerminations = 0
        return true
    }

    /// Whether a message addressed to `reported` is still deliverable.
    public func accepts(generation reported: UInt64) -> Bool {
        !isVoided && reported == generation
    }
}
