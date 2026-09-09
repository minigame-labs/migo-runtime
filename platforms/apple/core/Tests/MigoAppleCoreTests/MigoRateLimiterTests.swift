import XCTest

@testable import MigoAppleCore

/// The bound on content crossing into the host, at its boundaries.
///
/// Every case is a plain assertion because the clock is a parameter. A limiter that
/// read `Date()` itself could only be tested by sleeping, which is how a limiter ends
/// up with one test and a comment.
final class MigoRateLimiterTests: XCTestCase {

    private let epoch = Date(timeIntervalSince1970: 1_000_000)

    func testTheFirstEventOpensTheWindowRatherThanJoiningOne() {
        // Built at construction time and first used a second later, a limiter whose
        // window began at construction starts half-spent -- and the caller has no way
        // to know it did.
        var limiter = MigoRateLimiter(perWindow: 2)
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(3600)), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(3600)), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(3600)), .refused)
    }

    func testExactlyTheAllowanceIsAdmittedWithinAWindow() {
        var limiter = MigoRateLimiter(perWindow: 3)
        for index in 0..<3 {
            XCTAssertEqual(
                limiter.admit(at: epoch.addingTimeInterval(Double(index) * 0.1)),
                .admitted(droppedSinceLast: 0), "event \(index) was refused inside the allowance")
        }
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.4)), .refused)
    }

    func testTheWindowReopensExactlyOnTheBoundary() {
        var limiter = MigoRateLimiter(perWindow: 1)
        XCTAssertEqual(limiter.admit(at: epoch), .admitted(droppedSinceLast: 0))
        // Just inside: still refused.
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.999)), .refused)
        // Exactly on the boundary: a new window. The comparison is `>=`, and which
        // side of it the boundary falls on is the kind of thing a reader has to be
        // able to check rather than infer.
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(1.0)), .admitted(droppedSinceLast: 1))
    }

    func testTheDropCountTravelsWithTheNextAdmissionAndThenClears() {
        var limiter = MigoRateLimiter(perWindow: 1)
        XCTAssertEqual(limiter.admit(at: epoch), .admitted(droppedSinceLast: 0))
        for _ in 0..<5 { XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.1)), .refused) }
        XCTAssertEqual(limiter.pendingDropCount, 5)
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(1.1)), .admitted(droppedSinceLast: 5),
            "a gap in the host's log that nobody mentions looks like quiet")
        XCTAssertEqual(limiter.pendingDropCount, 0)
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(2.2)), .admitted(droppedSinceLast: 0),
            "the count was reported twice")
    }

    func testASustainedFloodStillReportsItsBacklogWithoutAnAdmission() {
        var limiter = MigoRateLimiter(perWindow: 1)
        _ = limiter.admit(at: epoch)
        for index in 0..<100 {
            XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(Double(index) * 0.001)), .refused)
        }
        XCTAssertEqual(
            limiter.pendingDropCount, 100,
            "a flood that never pauses reaches no admission, so the backlog has to be readable "
                + "without one")
    }

    func testAWindowBoundaryCanAdmitTwiceTheRateAndThatIsTheStatedTradeoff() {
        // Documented rather than fixed: a fixed window admits the tail of one window
        // and the head of the next. The bound exists to stop an unbounded flood, and
        // pretending otherwise would be a comment that does not match the code.
        var limiter = MigoRateLimiter(perWindow: 2)
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.98)), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.99)), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(1.99)), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(2.0)), .admitted(droppedSinceLast: 0),
            "four admissions inside about one second, which is the fixed window's known cost")
    }

    func testAShorterWindowIsHonoured() {
        var limiter = MigoRateLimiter(perWindow: 1, window: 0.25)
        XCTAssertEqual(limiter.admit(at: epoch), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(limiter.admit(at: epoch.addingTimeInterval(0.2)), .refused)
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(0.25)), .admitted(droppedSinceLast: 1))
    }

    func testTimeGoingBackwardsDoesNotOpenAWindow() {
        // Clocks move backwards: an NTP correction, a user changing the time. A
        // limiter that treats a negative interval as "a new window" hands a flood a
        // free pass every time the clock is adjusted.
        var limiter = MigoRateLimiter(perWindow: 1)
        XCTAssertEqual(limiter.admit(at: epoch), .admitted(droppedSinceLast: 0))
        XCTAssertEqual(
            limiter.admit(at: epoch.addingTimeInterval(-3600)), .refused,
            "a backwards clock reopened the window")
    }
}
