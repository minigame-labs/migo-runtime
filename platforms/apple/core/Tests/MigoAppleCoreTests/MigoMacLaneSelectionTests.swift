import XCTest

@testable import MigoAppleCore

/// The sentence `Sources/MigoMacV8/README.md` has promised since the lane was
/// specified, executed.
///
/// It promised that a missing JIT entitlement selects a WebKit lane and does not
/// silently become a jitless V8. Until this existed the promise was checked by
/// nobody -- `MigoMacV8` was a single `Placeholder.swift` -- and
/// `scripts/test-macos-archive-runs-js.sh` says so in its own output.
final class MigoMacLaneSelectionTests: XCTestCase {

    private typealias Selection = MigoMacLaneSelection
    private typealias Signature = MigoMacLaneSelection.ProcessSignature

    func testTheEntitlementSelectsTheNativeLane() {
        let decision = Selection.decide(
            v8LaneCompiledIn: true,
            signature: Signature(hardenedRuntime: .yes, jitEntitlement: .yes))
        XCTAssertEqual(decision.profile, .macosV8Native)
        XCTAssertTrue(decision.explanation.contains("hardened runtime"))
    }

    func testAMissingEntitlementSelectsWebKitAndNeverAJitlessV8() {
        // The whole promise, in one assertion pair: the profile is a WebKit
        // lane, and `MigoRuntimeProfile` has no jitless case for it to have
        // fallen into even if somebody wanted one.
        let decision = Selection.decide(
            v8LaneCompiledIn: true,
            signature: Signature(hardenedRuntime: .yes, jitEntitlement: .no))
        XCTAssertEqual(decision.profile, .macosWebKitFull)
        XCTAssertEqual(decision.reason, .capabilityProbeFailed)
        XCTAssertTrue(
            decision.explanation.contains("WebAssembly"),
            "the explanation has to say what a jitless V8 would cost, or the next person "
                + "reading it will propose one as the fallback")
    }

    func testAnUnhardenedProcessWithoutTheEntitlementIsStillRefused() {
        // JIT would in fact work here today. It is refused anyway: an app that
        // ships unhardened is an app that will be hardened before notarisation,
        // and a runtime that worked in development and died in the notarised
        // build is the worst time to find out.
        let decision = Selection.decide(
            v8LaneCompiledIn: true,
            signature: Signature(hardenedRuntime: .no, jitEntitlement: .no))
        XCTAssertEqual(decision.profile, .macosWebKitFull)
    }

    func testAnUnreadableSignatureIsNotReadAsUnsigned() {
        // `unknown` and `no` are different answers. A host that could not read
        // its own signature has not learned "unsigned"; it has learned nothing,
        // and starting V8 on that guess is a process that dies at its first
        // compile.
        let unknown = Selection.decide(
            v8LaneCompiledIn: true,
            signature: Signature(hardenedRuntime: .unknown, jitEntitlement: .unknown))
        XCTAssertEqual(unknown.profile, .macosWebKitFull)
        XCTAssertTrue(unknown.explanation.contains("could not be read"))

        let absent = Selection.decide(
            v8LaneCompiledIn: true,
            signature: Signature(hardenedRuntime: .yes, jitEntitlement: .no))
        XCTAssertNotEqual(
            unknown.explanation, absent.explanation,
            "both refuse, and a host that cannot tell 'no entitlement' from 'could not ask' "
                + "cannot tell a packaging mistake from a broken signature")
    }

    func testABuildWithoutTheLaneSaysSoRatherThanBlamingTheDevice() {
        // The external-frames build of this SDK contains no engine at all.
        // Reporting that as a capability failure would send whoever reads it to
        // the entitlements, which are fine.
        let decision = Selection.decide(
            v8LaneCompiledIn: false,
            signature: Signature(hardenedRuntime: .yes, jitEntitlement: .yes))
        XCTAssertEqual(decision.profile, .macosWebKitFull)
        XCTAssertEqual(decision.reason, .initializationFailed)
        XCTAssertNotEqual(decision.reason, .capabilityProbeFailed)
    }

    func testEveryOutcomeIsALaneThatExistsOnThisPlatform() {
        // iOS lanes are not answers to a macOS question, and a jitless V8 is not
        // an answer to any question. Derived rather than listed, so a new case
        // in `MigoRuntimeProfile` fails here instead of silently becoming
        // reachable.
        let macLanes: Set<MigoRuntimeProfile> = [.macosV8Native, .macosWebKitFull]
        for compiledIn in [true, false] {
            for hardened in [Selection.Observation.yes, .no, .unknown] {
                for jit in [Selection.Observation.yes, .no, .unknown] {
                    let decision = Selection.decide(
                        v8LaneCompiledIn: compiledIn,
                        signature: Signature(hardenedRuntime: hardened, jitEntitlement: jit))
                    XCTAssertTrue(
                        macLanes.contains(decision.profile),
                        "compiledIn=\(compiledIn) hardened=\(hardened) jit=\(jit) selected "
                            + "\(decision.profile), which is not a macOS lane")
                }
            }
        }
    }
}
