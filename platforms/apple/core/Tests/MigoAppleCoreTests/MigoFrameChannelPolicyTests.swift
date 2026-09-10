import XCTest

@testable import MigoAppleCore

/// The threshold the transport measurement bought.
///
/// These assertions are the measurement written down so it cannot drift back
/// into prose. Each one names the reading behind it, because a threshold whose
/// provenance lives only in a document is a threshold somebody will tune.
final class MigoFrameChannelPolicyTests: XCTestCase {

    private typealias Policy = MigoFrameChannelPolicy

    func testTheSocketCarriesEverythingUpToAndIncludingSixtyFourKibibytes() {
        // Measured on iPhone 12 / iOS 17.0.3: below 64 KiB the socket is 1.9-2.2x
        // faster and costs 4x less host CPU.
        for bytes in [0, 1, 64, 4096, 65_535, 65_536] {
            XCTAssertEqual(
                Policy.channel(forPacketOf: bytes), .loopbackWebSocket,
                "\(bytes) bytes should go over the socket")
        }
    }

    func testTheSchemeCarriesEverythingAbove() {
        // At 128 KiB both metrics have turned: latency 0.74 and CPU 0.89 in the
        // scheme's favour, rising to 0.23 on both at a mebibyte.
        for bytes in [65_537, 131_072, 1_048_576, 4 * 1_048_576] {
            XCTAssertEqual(
                Policy.channel(forPacketOf: bytes), .schemeRequest,
                "\(bytes) bytes should go over the scheme request")
        }
    }

    /// The boundary is inclusive, and that is a measurement rather than a
    /// convention.
    ///
    /// At exactly 64 KiB latency was level (0.99) and host CPU still favoured the
    /// socket (1.18x). Rounding the other way would hand the scheme a size where
    /// it was measured to be worse on one metric and no better on the other.
    func testTheBoundaryItselfBelongsToTheSocket() {
        XCTAssertEqual(Policy.socketCeilingBytes, 65_536)
        XCTAssertEqual(Policy.channel(forPacketOf: Policy.socketCeilingBytes), .loopbackWebSocket)
        XCTAssertEqual(
            Policy.channel(forPacketOf: Policy.socketCeilingBytes + 1), .schemeRequest)
    }

    /// The decision depends on the packet and on nothing else.
    ///
    /// Not a style point. A transport that changed channel for reasons the
    /// producer cannot see -- how busy the socket was, what the last packet
    /// carried -- would produce latency nobody can attribute to a cause, which is
    /// the shape of every frame-pacing bug this project has spent a week on.
    func testTheSameSizeAlwaysAnswersTheSame() {
        for bytes in [0, 1024, 65_536, 65_537, 1_048_576] {
            let first = Policy.channel(forPacketOf: bytes)
            for _ in 0..<8 {
                XCTAssertEqual(Policy.channel(forPacketOf: bytes), first)
            }
        }
    }

    /// The reported cost is a measured class and never an interpolation.
    func testTheReportedCostIsAMeasuredReadingOrNothing() {
        XCTAssertEqual(
            Policy.measuredRoundTripMilliseconds(forPacketOf: 65_536, on: .loopbackWebSocket),
            0.425)
        XCTAssertEqual(
            Policy.measuredRoundTripMilliseconds(forPacketOf: 1_048_576, on: .schemeRequest),
            0.935)
        // Between two classes: the class at or below, not a number between them.
        XCTAssertEqual(
            Policy.measuredRoundTripMilliseconds(forPacketOf: 100_000, on: .loopbackWebSocket),
            0.425)
        // Below every measured class there is no reading, and nil says so rather
        // than borrowing the smallest one.
        XCTAssertNil(
            Policy.measuredRoundTripMilliseconds(forPacketOf: 8, on: .loopbackWebSocket))
    }

}
