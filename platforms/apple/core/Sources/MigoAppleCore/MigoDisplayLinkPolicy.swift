import Foundation

/// Which vsync source the presenter uses, and at what cadence.
///
/// **What this is not.** G0 has to choose the Performance+ *frame clock* between a
/// feature-detected Worker `requestAnimationFrame`, a Window rAF relay, and a host
/// display-link relay -- and that choice is a measurement nobody has taken. This is
/// the other clock: the one the native presenter needs to know when a drawable is
/// due, on macOS V8 native and on the rendering side of Performance+ regardless of
/// which arm wins. Implementing it is not implementing an arm.
///
/// **Why the cadence is an input and not a constant.** `contracts/apple/profile-policy.json`
/// has `apple_promotion: host_opt_in`, so the host asks. The host also knows its
/// content's target frame rate and this package does not, and inventing one here
/// would put an unmeasured performance number in the one place nobody would look for
/// it. So the host states a target, and the only decisions made here are whether the
/// request is admissible and what to say when it is not.
///
/// **The failure this exists to make visible.** On iPhone, a display link may exceed
/// 60 Hz only if the host app's `Info.plist` carries
/// `CADisableMinimumFrameDurationOnPhone`. Without it iOS silently clamps, and a team
/// that asked for 120 gets 60 with nothing anywhere saying why -- which is a
/// performance investigation into a plist key. The decision below names it.
public enum MigoDisplayLinkPolicy {

    public enum Platform: String, Sendable, CaseIterable {
        case iOS
        case macOS
    }

    /// The API that delivers the tick.
    public enum Mechanism: String, Sendable, Equatable, CaseIterable {
        /// `CADisplayLink`. On iOS always; on macOS through `NSView.displayLink`,
        /// which arrived in macOS 14.
        case caDisplayLink
        /// `CVDisplayLink`, for macOS 11 to 13.
        ///
        /// The deployment floor is macOS 11 and this branch is why raising it to 13
        /// buys nothing: the branch is needed up to and including 13 either way, so
        /// the only thing a higher floor removes is users. That reasoning is in
        /// `contracts/apple/deployment-floor.json` and this is the code it describes.
        case cvDisplayLink
    }

    /// What the host asks for.
    public struct Request: Sendable, Equatable {
        public var platform: Platform
        public var osMajor: Int
        /// The host opted in to a rate above the default. `apple_promotion` is
        /// `host_opt_in`, so an absent opt-in is not a slow default -- it is the
        /// host's answer.
        public var promotionRequested: Bool
        /// Frames per second the host wants, when it has a target. `nil` leaves the
        /// cadence to the system.
        public var targetFramesPerSecond: Int?
        /// What the display can actually do (`UIScreen.maximumFramesPerSecond`, or a
        /// macOS display's refresh rate).
        public var displayMaximumFramesPerSecond: Int
        /// Whether the host app's `Info.plist` carries
        /// `CADisableMinimumFrameDurationOnPhone`.
        public var minimumFrameDurationDisabled: Bool

        public init(
            platform: Platform, osMajor: Int, promotionRequested: Bool = false,
            targetFramesPerSecond: Int? = nil, displayMaximumFramesPerSecond: Int = 60,
            minimumFrameDurationDisabled: Bool = false
        ) {
            self.platform = platform
            self.osMajor = osMajor
            self.promotionRequested = promotionRequested
            self.targetFramesPerSecond = targetFramesPerSecond
            self.displayMaximumFramesPerSecond = displayMaximumFramesPerSecond
            self.minimumFrameDurationDisabled = minimumFrameDurationDisabled
        }
    }

    /// The cadence to ask the system for.
    public enum Cadence: Sendable, Equatable {
        /// Ask for nothing and take what the system gives. Not a fallback: it is the
        /// correct answer whenever the host expressed no target, and it lets the
        /// system lower the rate under thermal pressure without being overridden.
        case systemDefault
        /// A `CAFrameRateRange`. `minimum` is deliberately below `preferred`: pinning
        /// them together forbids the system from dropping the rate when the device
        /// is hot, which trades a frame-rate number for a thermal one.
        case range(minimum: Int, preferred: Int, maximum: Int)
    }

    public struct Decision: Sendable, Equatable {
        public let mechanism: Mechanism
        public let cadence: Cadence
        /// What was decided and why, in one sentence, for the host to log. A cadence
        /// without a reason is a number somebody will later have to reverse-engineer
        /// from behaviour.
        public let reason: String
        /// True when the host asked for a rate it will not get. Separate from the
        /// reason string so a host can assert on it in its own tests rather than
        /// matching prose.
        public let requestWasReduced: Bool
    }

    public static func decide(_ request: Request) -> Decision {
        let mechanism: Mechanism
        switch request.platform {
        case .iOS:
            mechanism = .caDisplayLink
        case .macOS:
            mechanism = request.osMajor >= 14 ? .caDisplayLink : .cvDisplayLink
        }

        guard let target = request.targetFramesPerSecond, target > 0 else {
            return Decision(
                mechanism: mechanism, cadence: .systemDefault,
                reason: "the host stated no target frame rate, so the system's cadence stands",
                requestWasReduced: false)
        }

        let displayMaximum = max(1, request.displayMaximumFramesPerSecond)

        // The plist gate applies to iPhone and to a rate above 60. It is checked
        // before the display capability because it is the one an operator can fix.
        if request.platform == .iOS, target > 60, !request.minimumFrameDurationDisabled {
            return Decision(
                mechanism: mechanism, cadence: .range(minimum: 30, preferred: 60, maximum: 60),
                reason:
                    "the host asked for \(target) fps and the app's Info.plist does not carry "
                    + "CADisableMinimumFrameDurationOnPhone, so iOS clamps to 60 whatever is "
                    + "requested; asking for 60 instead of \(target) is the same result stated "
                    + "honestly",
                requestWasReduced: true)
        }

        if !request.promotionRequested, target > 60 {
            return Decision(
                mechanism: mechanism, cadence: .systemDefault,
                reason:
                    "the host asked for \(target) fps without opting in to a high refresh rate "
                    + "(profile-policy's apple_promotion is host_opt_in), so the system cadence "
                    + "stands",
                requestWasReduced: true)
        }

        if target > displayMaximum {
            return Decision(
                mechanism: mechanism,
                cadence: .range(
                    minimum: minimumRate(for: displayMaximum), preferred: displayMaximum,
                    maximum: displayMaximum),
                reason:
                    "the host asked for \(target) fps on a display that reports \(displayMaximum); "
                    + "asking for more than the display can present spends a frame budget on "
                    + "frames nobody sees",
                requestWasReduced: true)
        }

        return Decision(
            mechanism: mechanism,
            cadence: .range(
                minimum: minimumRate(for: target), preferred: target, maximum: target),
            reason: "the host asked for \(target) fps and the display reports \(displayMaximum)",
            requestWasReduced: false)
    }

    /// The floor of a requested range.
    ///
    /// Half the preferred rate, rounded down, and never below 1. Half rather than
    /// the preferred rate itself because a range whose minimum equals its maximum
    /// tells the system it may not slow down -- and the thing it would otherwise slow
    /// down for is heat. Half rather than 1 because a floor of 1 permits a stutter
    /// the system considers satisfied.
    static func minimumRate(for preferred: Int) -> Int {
        max(1, preferred / 2)
    }
}
