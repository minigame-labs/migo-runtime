import XCTest

@testable import MigoAppleCore

final class MigoVsyncTimestampTests: XCTestCase {

    private func nanoseconds(_ seconds: Double) -> Result<Int64, MigoVsyncTimestamp.Rejection> {
        MigoVsyncTimestamp.nanoseconds(fromSeconds: seconds)
    }

    func testASecondIsABillionNanoseconds() {
        XCTAssertEqual(nanoseconds(1), .success(1_000_000_000))
        XCTAssertEqual(nanoseconds(0), .success(0))
    }

    func testATypicalUptimeTimestampSurvivesTheConversion() {
        // CACurrentMediaTime on a machine up for a week, to the microsecond.
        XCTAssertEqual(nanoseconds(604_800.123_456), .success(604_800_123_456_000))
    }

    func testTheConversionRoundsRatherThanTruncating() {
        // A 60 Hz interval is not representable, and truncating biases every
        // tick the same direction -- an interval a fraction short at every
        // cadence, which reads as a clock that drifts.
        XCTAssertEqual(nanoseconds(1.0 / 60.0), .success(16_666_667))
        XCTAssertEqual(nanoseconds(0.000_000_001_5), .success(2))
    }

    func testNotANumberIsRefusedRatherThanConverted() {
        // `Int64(Double.nan)` traps in Swift, so this is the difference between
        // a rejected tick and a crashed frame.
        XCTAssertEqual(nanoseconds(.nan), .failure(.notFinite))
        XCTAssertEqual(nanoseconds(.infinity), .failure(.notFinite))
        XCTAssertEqual(nanoseconds(-.infinity), .failure(.notFinite))
    }

    func testANegativeTimestampIsRefusedHereRatherThanByTheEngine() {
        // The ABI answers MIGO_ERROR_INVALID_ARGUMENT for this. Spending a call
        // to be told so, once per frame, is the thing being avoided.
        XCTAssertEqual(nanoseconds(-0.000_001), .failure(.negative))
    }

    func testAValueTooLargeForTheIntegerIsNamedRatherThanSaturated() {
        // 2^63 nanoseconds is 292 years of uptime: unreachable, and refused
        // rather than clamped, because a clamped timestamp is a frame time that
        // stops advancing without saying so.
        XCTAssertEqual(nanoseconds(1e12), .failure(.tooLarge))
        // The boundary itself: exactly 2^63 nanoseconds is one past the top.
        XCTAssertEqual(nanoseconds(9_223_372_036.854_775_808), .failure(.tooLarge))
    }

    func testTheLargestRepresentableTimestampIsAccepted() {
        let justUnder = 9_223_372_036.0
        guard case .success(let value) = nanoseconds(justUnder) else {
            return XCTFail("9223372036 s is inside the range")
        }
        XCTAssertEqual(value, 9_223_372_036_000_000_000)
    }
}
