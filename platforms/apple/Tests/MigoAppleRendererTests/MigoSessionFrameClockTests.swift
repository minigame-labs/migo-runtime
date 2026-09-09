import MigoAppleCore
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

/// The pacing, without an engine.
///
/// Everything here is about when a vsync becomes a frame, which is a decision
/// this class makes on its own; a live session would only make the same
/// assertions slower and flakier. What genuinely needs a session -- that the ABI
/// accepts the timestamp -- is `MigoSurfaceAttachTests`' business.
final class MigoSessionFrameClockTests: XCTestCase {

    private let decision = MigoDisplayLinkPolicy.decide(
        MigoDisplayLinkPolicy.Request(platform: .iOS, osMajor: 17, targetFramesPerSecond: 60))

    private func makeClock(
        notify: @escaping (Int64) -> MigoResult = { _ in MIGO_OK }
    ) -> (MigoSessionFrameClock, () -> [Int64]) {
        let box = Box()
        let clock = MigoSessionFrameClock(decision: decision) { nanoseconds in
            box.timestamps.append(nanoseconds)
            return notify(nanoseconds)
        }
        return (clock, { box.timestamps })
    }

    private final class Box {
        var timestamps: [Int64] = []
    }

    func testATickWithNoRequestOutstandingDeliversNothing() {
        let (clock, delivered) = makeClock()
        clock.tick(targetTimestamp: 1.0)
        XCTAssertEqual(delivered(), [])
        XCTAssertEqual(clock.currentStatistics.ticks, 1)
        XCTAssertEqual(clock.currentStatistics.idle, 1)
        XCTAssertEqual(clock.currentStatistics.delivered, 0)
    }

    func testARequestedFrameIsDeliveredOnceAndOnlyOnce() {
        // The ABI's unit is one requested frame. A second tick serving the same
        // request would present the same content against a newer clock.
        let (clock, delivered) = makeClock()
        clock.requestFrame()
        clock.tick(targetTimestamp: 2.0)
        clock.tick(targetTimestamp: 2.016_666_667)
        XCTAssertEqual(delivered(), [2_000_000_000])
        XCTAssertEqual(clock.currentStatistics.delivered, 1)
        XCTAssertEqual(clock.currentStatistics.idle, 1)
    }

    func testTheTimestampDeliveredIsTheTargetNotTheTickCount() {
        let (clock, delivered) = makeClock()
        clock.requestFrame()
        clock.tick(targetTimestamp: 604_800.123_456)
        XCTAssertEqual(delivered(), [604_800_123_456_000])
    }

    func testTwoRequestsBeforeATickCollapseIntoOneFrame() {
        let (clock, delivered) = makeClock()
        clock.requestFrame()
        clock.requestFrame()
        clock.tick(targetTimestamp: 3.0)
        clock.tick(targetTimestamp: 3.016)
        XCTAssertEqual(delivered().count, 1, "a queue of frame requests is a queue of stale times")
        XCTAssertEqual(clock.currentStatistics.coalescedRequests, 1)
    }

    func testATimestampTheAbiCannotTakeLeavesTheFrameStillRequested() {
        // The engine asked for a frame and this tick could not carry it. Dropping
        // the request would leave the engine waiting for a request it has already
        // made -- the shape of "content loaded, reported ready, and not one frame
        // was drawn" that the host-kit header already records once.
        let (clock, delivered) = makeClock()
        clock.requestFrame()
        clock.tick(targetTimestamp: .nan)
        XCTAssertEqual(delivered(), [])
        XCTAssertEqual(clock.currentStatistics.unusableTimestamps, 1)
        clock.tick(targetTimestamp: 4.0)
        XCTAssertEqual(delivered(), [4_000_000_000], "the outstanding request survived the bad tick")
    }

    func testARefusalIsRecordedAndDoesNotRearmTheFrame() {
        let (clock, delivered) = makeClock { _ in MIGO_ERROR_INVALID_STATE }
        clock.requestFrame()
        clock.tick(targetTimestamp: 5.0)
        XCTAssertEqual(delivered(), [5_000_000_000])
        XCTAssertEqual(clock.currentStatistics.refused, 1)
        XCTAssertEqual(clock.currentStatistics.lastRefusal, MIGO_ERROR_INVALID_STATE)
        // A refusal means the session cannot take frames right now -- retrying it
        // every vsync would spend the frame budget being told so.
        clock.tick(targetTimestamp: 5.016)
        XCTAssertEqual(delivered().count, 1)
    }

    func testStoppingForgetsAnOutstandingRequest() {
        // The request was for a frame at a timestamp that has passed. Serving it
        // after a restart presents content paced against a clock that stopped.
        let (clock, delivered) = makeClock()
        clock.requestFrame()
        clock.stop()
        clock.tick(targetTimestamp: 6.0)
        XCTAssertEqual(delivered(), [])
        XCTAssertEqual(clock.currentStatistics.idle, 1)
    }

    func testTheClockDoesNotOutliveItsOwnLastReference() {
        // The display link holds the tick closure and this object holds the link.
        // A strong capture there closes the cycle and the clock -- and whatever a
        // host hung off it -- never deallocates. That failure is invisible except
        // as a display link still firing after the screen it drew is gone.
        weak var weakClock: MigoSessionFrameClock?
        autoreleasepool {
            let clock = MigoSessionFrameClock(decision: decision) { _ in MIGO_OK }
            clock.requestFrame()
            clock.tick(targetTimestamp: 7.0)
            weakClock = clock
            XCTAssertNotNil(weakClock)
        }
        XCTAssertNil(weakClock, "the clock is kept alive by its own display link")
    }
}
