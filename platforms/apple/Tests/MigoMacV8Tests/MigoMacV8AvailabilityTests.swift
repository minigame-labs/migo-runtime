import MigoAppleCore
import XCTest

@testable import MigoMacV8

#if os(macOS)

    /// The measurement half, against the process actually running it.
    ///
    /// `MigoMacLaneSelectionTests` covers the decision with every combination of
    /// inputs, because a decision is a pure function. What it cannot cover is
    /// whether the inputs arrive at all: `SecCodeCopySelf` on a real binary,
    /// through a real signature, is a question only a real process answers --
    /// and until this target existed no lane had ever EXECUTED `MigoMacV8`.
    final class MigoMacV8AvailabilityTests: XCTestCase {

        func testAProcessCanReadItsOwnSignature() {
            // The whole reason `unknown` exists is that this can fail. It is
            // asserted rather than tolerated because on macOS it should not:
            // every process can inspect its own code object, signed or not, and
            // a run where both answers came back `unknown` would mean the
            // resolver is deciding on no information at all -- which is a
            // permanent WebKit lane nobody asked for.
            let signature = MigoMacV8Availability.observeSelf()
            XCTAssertNotEqual(
                signature.hardenedRuntime, .unknown,
                "this process could not read its own signing flags; the resolver would refuse "
                    + "the V8 lane for a reason that is about this machine rather than about "
                    + "the app")
            XCTAssertNotEqual(
                signature.jitEntitlement, .unknown,
                "this process could not read its own entitlements")
        }

        func testTheTestRunnerIsNotOfferedTheV8Lane() {
            // The process this reads is Apple's `xctest` runner, not this
            // bundle: an XCTest bundle is loaded INTO that runner, and
            // `SecCodeCopySelf` answers for the process. Measured rather than
            // assumed -- ad-hoc signing the bundle, and then the Mach-O inside
            // it, with `com.apple.security.cs.allow-jit` changes nothing that
            // this test can see, while the same signature on a standalone
            // executable reads back fine.
            //
            // So the positive branch is NOT reachable from here, and this test
            // does not pretend otherwise. It asserts the branch that is: the
            // runner carries no such entitlement, and the resolver refuses.
            //
            // The positive branch is covered where it can be: exhaustively in
            // `MigoMacLaneSelectionTests` against the decision table, and on a
            // real signed process by `scripts/test-macos-archive-runs-js.sh`,
            // which ad-hoc signs a host with and without the entitlement and
            // measures what V8 actually does under each.
            let signature = MigoMacV8Availability.observeSelf()
            XCTAssertEqual(
                signature.jitEntitlement, .no,
                "the xctest runner carries com.apple.security.cs.allow-jit, which would mean "
                    + "Apple started shipping it entitled and this test is reading a different "
                    + "process than it thinks")
            let decision = MigoMacV8Availability.resolve()
            XCTAssertEqual(decision.profile, .macosWebKitFull)
            XCTAssertEqual(decision.reason, .capabilityProbeFailed)
        }

        func testTheLaneReportsItselfCompiledInBecauseItIsRunning() {
            // Step 2 of the profile policy is "reported from what the artifact
            // actually compiled, never from what the enum can name". This code
            // executing IS the report, and the `#if os(macOS)` around the
            // implementation is what keeps that true rather than assumed.
            XCTAssertTrue(MigoMacV8Availability.laneIsCompiledIn)
        }

        func testResolveNeverContradictsTheDecisionTable() {
            // The two halves are separate types on purpose, and this is the
            // seam: whatever this process observes, `resolve()` must return
            // exactly what `MigoMacLaneSelection.decide` returns for it. A
            // resolver that "helpfully" adjusted the answer would be a second
            // decision nobody tests.
            let signature = MigoMacV8Availability.observeSelf()
            XCTAssertEqual(
                MigoMacV8Availability.resolve(),
                MigoMacLaneSelection.decide(
                    v8LaneCompiledIn: MigoMacV8Availability.laneIsCompiledIn,
                    signature: signature))
        }
    }
#endif
