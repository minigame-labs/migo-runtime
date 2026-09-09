import XCTest

@testable import MigoAppleCore

/// The WebContent termination policy, as behaviour rather than as a comment.
///
/// Every case here is a way a session recovers into a worse state than the one it
/// crashed from: a rebuild that accepts the dead view's messages, a crash loop that
/// never trips its own limit, a stop that leaves a generation looking live.
final class MigoWebContentRecoveryTests: XCTestCase {

    func testTheFirstTerminationRebuildsUnderANewGeneration() {
        var recovery = MigoWebContentRecovery(terminationBudget: 3)
        let before = recovery.generation
        XCTAssertEqual(recovery.webContentTerminated(), .rebuild(generation: before + 1))
        XCTAssertFalse(recovery.isVoided, "a rebuilt session has a live generation to talk to")
        XCTAssertTrue(recovery.accepts(generation: before + 1))
    }

    func testTheDeadGenerationStopsBeingAddressable() {
        var recovery = MigoWebContentRecovery(terminationBudget: 3)
        let dead = recovery.generation
        _ = recovery.webContentTerminated()
        XCTAssertFalse(
            recovery.accepts(generation: dead),
            "a late reply from the process that just died is indistinguishable from a fresh one "
                + "unless the generation is compared, and it arrives after the replacement exists")
    }

    func testGenerationsAreNeverReused() {
        var recovery = MigoWebContentRecovery(terminationBudget: 10)
        var seen: Set<UInt64> = [recovery.generation]
        for _ in 0..<8 {
            guard case .rebuild(let generation) = recovery.webContentTerminated() else {
                return XCTFail("the budget was not reached and no rebuild was offered")
            }
            XCTAssertTrue(
                seen.insert(generation).inserted,
                "generation \(generation) came round again, so a stale message addressed to the "
                    + "earlier one is now deliverable")
            recovery.contentBecameReady(generation: generation)
        }
    }

    func testAStaleReadyDoesNotClearTheStreakThatIsAboutToStopTheLoop() {
        // The bug this test exists for. A dying web view can finish sending its
        // ready message after the replacement has been built. Accepting it resets
        // the counter, every crash becomes the first crash, and the budget is never
        // reached however many times content dies.
        var recovery = MigoWebContentRecovery(terminationBudget: 2)
        let dead = recovery.generation
        guard case .rebuild = recovery.webContentTerminated() else {
            return XCTFail("the first termination should rebuild")
        }
        XCTAssertFalse(
            recovery.contentBecameReady(generation: dead),
            "a ready from the dead generation was accepted")
        XCTAssertEqual(recovery.consecutiveTerminations, 1)
        XCTAssertEqual(
            recovery.webContentTerminated(), .stop(afterConsecutiveTerminations: 2),
            "the loop has to end; a stale ready must not have made this the first crash again")
    }

    func testALiveReadyMakesTheNextTerminationTheFirstAgain() {
        var recovery = MigoWebContentRecovery(terminationBudget: 2)
        guard case .rebuild(let live) = recovery.webContentTerminated() else {
            return XCTFail("the first termination should rebuild")
        }
        XCTAssertTrue(recovery.contentBecameReady(generation: live))
        XCTAssertEqual(recovery.consecutiveTerminations, 0)
        // Content did start once, so this is a new failure and not a continuing one.
        guard case .rebuild = recovery.webContentTerminated() else {
            return XCTFail("a session that had reached ready should be rebuilt after a crash")
        }
    }

    func testACrashLoopTerminates() {
        // Whatever the budget, a sequence of terminations with no successful start
        // must reach a stop. Expressed over a range rather than one number so a
        // budget change cannot quietly make this vacuous.
        for budget in 1...5 {
            var recovery = MigoWebContentRecovery(terminationBudget: budget)
            var outcomes: [MigoWebContentRecovery.Outcome] = []
            for _ in 0..<(budget + 3) {
                outcomes.append(recovery.webContentTerminated())
            }
            guard let first = outcomes.firstIndex(where: { if case .stop = $0 { return true } else { return false } })
            else {
                return XCTFail("budget \(budget) never stopped after \(outcomes.count) terminations")
            }
            XCTAssertEqual(
                first, budget - 1,
                "budget \(budget) stopped at termination \(first + 1)")
        }
    }

    func testAStoppedSessionLeavesNoGenerationLookingLive() {
        var recovery = MigoWebContentRecovery(terminationBudget: 1)
        let last = recovery.generation
        XCTAssertEqual(recovery.webContentTerminated(), .stop(afterConsecutiveTerminations: 1))
        XCTAssertTrue(recovery.isVoided)
        XCTAssertFalse(
            recovery.accepts(generation: last),
            "nothing is being rebuilt, so every generation is dead and a message addressed to any "
                + "of them has nowhere to go")
        XCTAssertFalse(recovery.accepts(generation: last + 1))
        XCTAssertFalse(
            recovery.contentBecameReady(generation: last),
            "a stopped session cannot be talked back into life by a message from the process that "
                + "stopped it")
    }
}
