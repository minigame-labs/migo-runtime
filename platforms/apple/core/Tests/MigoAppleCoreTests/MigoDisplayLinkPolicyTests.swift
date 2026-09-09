import XCTest

@testable import MigoAppleCore

/// The vsync decision, which is mostly a decision about what to say.
///
/// The cadence itself is the host's number; what this type owns is whether the
/// request is admissible and whether the host is told when it is not. A silent clamp
/// is the failure being tested for: it turns a missing `Info.plist` key into a
/// performance investigation.
final class MigoDisplayLinkPolicyTests: XCTestCase {

    private typealias Policy = MigoDisplayLinkPolicy

    // MARK: - mechanism

    func testIOSAlwaysUsesTheDisplayLink() {
        for major in [15, 17, 18, 26] {
            let decision = Policy.decide(.init(platform: .iOS, osMajor: major))
            XCTAssertEqual(decision.mechanism, .caDisplayLink)
        }
    }

    func testMacOSCrossesToTheDisplayLinkAtFourteen() {
        // The branch the deployment floor's reasoning turns on: raising the floor
        // from 11 to 13 would not remove it, because it is needed up to and
        // including 13. So a higher floor buys nothing and costs coverage.
        for major in [11, 12, 13] {
            XCTAssertEqual(
                Policy.decide(.init(platform: .macOS, osMajor: major)).mechanism, .cvDisplayLink,
                "macOS \(major) has no NSView.displayLink")
        }
        for major in [14, 15, 26] {
            XCTAssertEqual(
                Policy.decide(.init(platform: .macOS, osMajor: major)).mechanism, .caDisplayLink)
        }
    }

    // MARK: - cadence

    func testNoTargetMeansTheSystemDecides() {
        // Not a fallback: it is the right answer when the host said nothing, and it
        // leaves the system free to lower the rate under thermal pressure.
        let decision = Policy.decide(.init(platform: .iOS, osMajor: 18))
        XCTAssertEqual(decision.cadence, .systemDefault)
        XCTAssertFalse(decision.requestWasReduced)
        XCTAssertTrue(decision.reason.contains("no target"), decision.reason)
    }

    func testAnOrdinaryTargetIsAskedForAsARange() {
        let decision = Policy.decide(
            .init(
                platform: .iOS, osMajor: 18, targetFramesPerSecond: 60,
                displayMaximumFramesPerSecond: 60))
        XCTAssertEqual(decision.cadence, .range(minimum: 30, preferred: 60, maximum: 60))
        XCTAssertFalse(decision.requestWasReduced)
    }

    func testTheRangeFloorIsBelowThePreferredRate() {
        // A range whose minimum equals its maximum tells the system it may not slow
        // down, and what it would slow down for is heat.
        guard
            case .range(let minimum, let preferred, _) = Policy.decide(
                .init(
                    platform: .iOS, osMajor: 18, promotionRequested: true,
                    targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 120,
                    minimumFrameDurationDisabled: true)
            ).cadence
        else { return XCTFail("a legal high-rate request produced no range") }
        XCTAssertLessThan(minimum, preferred)
        XCTAssertGreaterThan(minimum, 0)
    }

    // MARK: - the failures a host cannot otherwise see

    func testAMissingPlistKeyIsNamedRatherThanSilentlyClamped() {
        // The whole reason this type exists. Without the key iOS clamps to 60 and
        // says nothing, so a team that asked for 120 investigates its renderer.
        let decision = Policy.decide(
            .init(
                platform: .iOS, osMajor: 18, promotionRequested: true,
                targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 120,
                minimumFrameDurationDisabled: false))
        XCTAssertEqual(decision.cadence, .range(minimum: 30, preferred: 60, maximum: 60))
        XCTAssertTrue(decision.requestWasReduced)
        XCTAssertTrue(
            decision.reason.contains("CADisableMinimumFrameDurationOnPhone"),
            "the reason must name the key, because the key is the fix: \(decision.reason)")
    }

    func testWithTheKeyPresentTheHighRateIsActuallyRequested() {
        let decision = Policy.decide(
            .init(
                platform: .iOS, osMajor: 18, promotionRequested: true,
                targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 120,
                minimumFrameDurationDisabled: true))
        XCTAssertEqual(decision.cadence, .range(minimum: 60, preferred: 120, maximum: 120))
        XCTAssertFalse(decision.requestWasReduced)
    }

    func testAHighRateWithoutTheOptInIsRefusedAndSaysWhichOptIn() {
        // `apple_promotion` is host_opt_in in profile-policy.json, so an absent
        // opt-in is the host's answer and not an oversight to work around.
        let decision = Policy.decide(
            .init(
                platform: .iOS, osMajor: 18, promotionRequested: false,
                targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 120,
                minimumFrameDurationDisabled: true))
        XCTAssertEqual(decision.cadence, .systemDefault)
        XCTAssertTrue(decision.requestWasReduced)
        XCTAssertTrue(decision.reason.contains("apple_promotion"), decision.reason)
    }

    func testAskingForMoreThanTheDisplayCanPresentIsReducedToWhatItCan() {
        let decision = Policy.decide(
            .init(
                platform: .macOS, osMajor: 14, promotionRequested: true,
                targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 60))
        XCTAssertEqual(decision.cadence, .range(minimum: 30, preferred: 60, maximum: 60))
        XCTAssertTrue(decision.requestWasReduced)
        XCTAssertTrue(decision.reason.contains("nobody sees"), decision.reason)
    }

    func testTheMacOSPlistGateDoesNotApply() {
        // The key is an iPhone thing. Applying it on macOS would clamp a Mac to 60
        // for a reason that does not exist there.
        let decision = Policy.decide(
            .init(
                platform: .macOS, osMajor: 14, promotionRequested: true,
                targetFramesPerSecond: 120, displayMaximumFramesPerSecond: 120,
                minimumFrameDurationDisabled: false))
        XCTAssertEqual(decision.cadence, .range(minimum: 60, preferred: 120, maximum: 120))
        XCTAssertFalse(decision.requestWasReduced)
    }

    // MARK: - the link's own lifetime

    func testALinkDroppedWithoutBeingStoppedIsStillDeallocated() {
        // The bug this asserts against was in the first version of MigoDisplayLink,
        // on both platforms. `CADisplayLink` retains its target and the CVDisplayLink
        // callback context retained the owner, so the owner held the link and the link
        // held the owner -- and `deinit`, the one place that stops it, could never run.
        // A host that forgets `stop()` then has a display link firing for the life of
        // the process, which on a phone is a battery complaint with nothing to blame.
        weak var observed: MigoDisplayLink?
        autoreleasepool {
            let link = MigoDisplayLink(
                decision: Policy.decide(.init(platform: .macOS, osMajor: 14)),
                onTick: { _, _ in })
            observed = link
            #if os(iOS)
                link.start()
            #elseif os(macOS)
                link.start()
            #endif
        }
        XCTAssertNil(
            observed,
            "the link kept itself alive, so it runs until the process ends and deinit never fires")
    }

    func testStoppingTwiceAndStoppingWithoutStartingAreBothHarmless() {
        // The legacy path retains a context and releases it in `stop`. A second
        // release would be an over-release, which is a crash rather than an error.
        let link = MigoDisplayLink(
            decision: Policy.decide(.init(platform: .macOS, osMajor: 12)), onTick: { _, _ in })
        link.stop()
        link.stop()
        XCTAssertFalse(link.isRunning)
    }

    func testEveryDecisionCarriesAReason() {
        // Walked over a cross product rather than listed, so a branch added without
        // a reason is caught by the shape of the type rather than by review.
        for platform in Policy.Platform.allCases {
            for major in [11, 13, 14, 15, 18] {
                for target in [nil, 30, 60, 120, 240] as [Int?] {
                    for promotion in [false, true] {
                        for plist in [false, true] {
                            for displayMax in [60, 120] {
                                let decision = Policy.decide(
                                    .init(
                                        platform: platform, osMajor: major,
                                        promotionRequested: promotion,
                                        targetFramesPerSecond: target,
                                        displayMaximumFramesPerSecond: displayMax,
                                        minimumFrameDurationDisabled: plist))
                                XCTAssertFalse(
                                    decision.reason.isEmpty,
                                    "\(platform) \(major) target=\(String(describing: target)) "
                                        + "produced a decision with no reason")
                                if case .range(let minimum, let preferred, let maximum) =
                                    decision.cadence
                                {
                                    XCTAssertLessThanOrEqual(minimum, preferred)
                                    XCTAssertLessThanOrEqual(preferred, maximum)
                                    XCTAssertGreaterThan(minimum, 0)
                                    XCTAssertLessThanOrEqual(
                                        maximum, max(displayMax, 60),
                                        "a range above what the display reports was requested")
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
