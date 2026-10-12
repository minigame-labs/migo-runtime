import Foundation
import XCTest

@testable import MigoAppleRenderer

#if os(macOS)
    import AppKit

    /// The screen service on a Mac: the game's window kept out of screenshots
    /// and recordings, and what a Mac has no public means for refused.
    final class MigoSystemScreenTests: XCTestCase {
        private final class Recorder: @unchecked Sendable {
            var outcomes: [MigoHostServiceReply.Outcome] = []
        }

        private func request(_ method: UInt32, _ object: [String: Any] = [:], _ recorder: Recorder)
            -> MigoHostServiceRequest
        {
            MigoHostServiceRequest(
                service: .screen, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                reply: MigoHostServiceReply { recorder.outcomes.append($0) })
        }

        func testHiddenKeepsTheWindowOutOfCapturesAndNoneLetsItBack() {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 100, height: 100), styleMask: [.titled], backing: .buffered,
                defer: false)
            window.isReleasedWhenClosed = false
            let screen = MigoSystemScreen(post: { _, _ in }, window: { window })
            let recorder = Recorder()
            let method = MigoHostServiceID.Screen.setVisualEffectOnCapture
            screen.handle(request(method, ["visualEffect": "hidden"], recorder))
            XCTAssertEqual(window.sharingType, NSWindow.SharingType.none)
            screen.handle(request(method, ["visualEffect": "none"], recorder))
            XCTAssertEqual(window.sharingType, .readOnly)
            screen.handle(request(method, ["visualEffect": "blurred"], recorder))
            XCTAssertEqual(
                recorder.outcomes,
                [
                    .success(json: Data("{}".utf8)), .success(json: Data("{}".utf8)),
                    .failure(message: "visualEffect is neither none nor hidden", code: nil),
                ])
        }

        func testWhatAMacHasNoPublicMeansForIsNotSupported() {
            let screen = MigoSystemScreen(post: { _, _ in }, window: { nil })
            let recorder = Recorder()
            typealias Method = MigoHostServiceID.Screen
            screen.handle(request(Method.getBrightness, [:], recorder))
            screen.handle(request(Method.setBrightness, ["value": 0.5], recorder))
            screen.handle(request(Method.getRecordingState, [:], recorder))
            screen.handle(request(Method.setDeviceOrientation, ["value": "landscape"], recorder))
            screen.handle(request(Method.setVisualEffectOnCapture, ["visualEffect": "hidden"], recorder))
            XCTAssertEqual(
                recorder.outcomes,
                Array(repeating: .failure(message: "not supported", code: nil), count: 4)
                    + [.failure(message: "there is no window to protect", code: nil)])
        }
    }
#endif
