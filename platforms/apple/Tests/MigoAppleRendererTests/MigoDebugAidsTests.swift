import Foundation
import XCTest

@testable import MigoAppleRenderer

/// The debugging panel's model, and the screen service handing content's
/// `setEnableDebug` to the session.
final class MigoDebugAidsTests: XCTestCase {
    private func entry(_ level: Int, _ message: String) -> MigoEngineSession.ConsoleEntry {
        MigoEngineSession.ConsoleEntry(level: level, date: Date(timeIntervalSince1970: 0), message: message)
    }

    /// The panel keeps the latest lines, as the session's buffer keeps its
    /// latest, and marks each by its level.
    func testThePanelKeepsTheLatestLines() {
        var aids = MigoDebugAids()
        aids.append((0..<(MigoDebugAids.lineLimit + 5)).map { entry(1, "line \($0)") })
        XCTAssertEqual(aids.lines.count, MigoDebugAids.lineLimit)
        XCTAssertEqual(aids.lines.first?.message, "line 5")
        XCTAssertEqual(aids.lines.last?.message, "line \(MigoDebugAids.lineLimit + 4)")
        XCTAssertEqual(MigoDebugAids.text(of: entry(3, "boom")), "E boom")
        XCTAssertEqual(MigoDebugAids.text(of: entry(2, "careful")), "W careful")
        XCTAssertEqual(MigoDebugAids.text(of: entry(0, "detail")), "D detail")
        XCTAssertEqual(MigoDebugAids.text(of: entry(1, "said")), "I said")
    }

    /// Frames a second are the delivered frames since the last sample over the
    /// time between them; nothing until two samples.
    func testFramesASecondAreDeliveredFramesOverTime() {
        var aids = MigoDebugAids()
        XCTAssertEqual(aids.header, "-- frames/s")
        aids.sample(delivered: 100, at: 10)
        XCTAssertNil(aids.framesPerSecond)
        aids.sample(delivered: 130, at: 10.5)
        XCTAssertEqual(aids.framesPerSecond ?? 0, 60, accuracy: 0.001)
        XCTAssertEqual(aids.header, "60.0 frames/s")
        // A clock that went back -- a new session's -- is no rate.
        aids.sample(delivered: 3, at: 11)
        XCTAssertEqual(aids.framesPerSecond ?? 0, 60, accuracy: 0.001)
    }

    /// Content's request reaches whoever turns the aids: the session.
    func testTheScreenServiceHandsTheRequestToTheSession() {
        let screen = MigoSystemScreen(post: { _, _ in })
        var turned: [Bool] = []
        screen.debugAids = { turned.append($0) }
        for on in [true, false] {
            screen.handle(
                MigoHostServiceRequest(
                    service: .screen, method: MigoHostServiceID.Screen.setEnableDebug,
                    payload: try! JSONSerialization.data(withJSONObject: ["enableDebug": on]), reply: nil))
        }
        XCTAssertEqual(turned, [true, false])
    }
}
