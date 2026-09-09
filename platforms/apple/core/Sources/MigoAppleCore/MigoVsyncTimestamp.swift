import Foundation

/// The arithmetic between a display link's tick and the engine's vsync ABI.
///
/// WHY THIS IS ITS OWN TYPE. `migo_session_notify_vsync` takes nanoseconds as a
/// signed 64-bit integer and refuses a negative one with
/// `MIGO_ERROR_INVALID_ARGUMENT`; a display link reports `CFTimeInterval`
/// seconds. The conversion between them looks like one multiplication and has
/// four ways to be wrong -- a not-a-number, an infinity, a negative, and a value
/// past what the integer holds -- each of which turns into a plausible-looking
/// number rather than an error when written inline. `Int64(Double.nan)` is not
/// even defined behaviour to reason about: in Swift it traps.
///
/// It lives in the engine-free package because that is the package a pull
/// request compiles and tests on every commit, for both platforms. The half that
/// needs the engine is the one call it feeds.
public enum MigoVsyncTimestamp {

    /// Why a tick could not be expressed as a vsync timestamp.
    public enum Rejection: Equatable, Sendable {
        /// The clock reported something that is not a number, which is what a
        /// display link does on the tick after its display goes away.
        case notFinite
        /// Time before the clock's own zero. The ABI refuses it, and a host that
        /// passed it would spend a frame budget being told so.
        case negative
        /// Past what nanoseconds in a signed 64-bit integer can hold. Unreachable
        /// from an uptime clock -- 2^63 nanoseconds is 292 years -- and named
        /// rather than saturated, because a saturated timestamp is a frame time
        /// that silently stops advancing.
        case tooLarge
    }

    /// Nanoseconds for `migo_session_notify_vsync`, or why the tick is not one.
    ///
    /// Rounded rather than truncated: a display link's target timestamps are
    /// evenly spaced, and truncation biases every one of them the same direction,
    /// which shows up as a frame interval a fraction short at every cadence.
    public static func nanoseconds(fromSeconds seconds: CFTimeInterval) -> Result<Int64, Rejection>
    {
        guard seconds.isFinite else { return .failure(.notFinite) }
        guard seconds >= 0 else { return .failure(.negative) }
        let nanoseconds = (seconds * 1_000_000_000).rounded()
        // Compared against the exact power of two rather than Int64.max: the
        // literal Int64.max is not representable as a Double, so the comparison
        // would be made against the next value up and let one unrepresentable
        // case through into a trapping conversion.
        guard nanoseconds < 9_223_372_036_854_775_808.0 else { return .failure(.tooLarge) }
        return .success(Int64(nanoseconds))
    }
}
