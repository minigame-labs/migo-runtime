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
            // `swift test` produces an unsigned or ad-hoc binary with no
            // entitlements, which is exactly the shape the resolver must refuse.
            // If this ever passes the other way, something granted JIT to an
            // xctest bundle and the assumption behind the whole lane is wrong.
            let signature = MigoMacV8Availability.observeSelf()
            guard signature.jitEntitlement == .no else {
                // Not a failure: a maintainer running this under a signed host
                // with the entitlement gets the other branch, which is the one
                // below and is also worth asserting.
                XCTAssertEqual(signature.jitEntitlement, .yes)
                XCTAssertEqual(MigoMacV8Availability.resolve().profile, .macosV8Native)
                return
            }
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
