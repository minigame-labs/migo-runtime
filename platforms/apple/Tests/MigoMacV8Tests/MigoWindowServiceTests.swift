import Foundation
import XCTest

@testable import MigoAppleRenderer
@testable import MigoMacV8

#if os(macOS)
    import AppKit

    /// The window service the macOS game view answers: the window sized around
    /// the game, the cursor, pointer lock -- against a fake system side, so no
    /// test takes the machine's mouse -- and the window's state.
    final class MigoWindowServiceTests: XCTestCase {
        private final class Recorder: @unchecked Sendable {
            var outcomes: [MigoHostServiceReply.Outcome] = []
            var events: [String] = []
            var engaged = 0
            var released = 0
        }

        /// A window whose state the test sets.
        private final class StateWindow: NSWindow {
            var inDock = false
            var atFullSize = false
            override var isMiniaturized: Bool { inDock }
            override var isZoomed: Bool { atFullSize }
        }

        private var window: StateWindow!
        private var view: NSView!
        private let recorder = Recorder()

        override func setUp() {
            super.setUp()
            window = StateWindow(
                contentRect: NSRect(x: 100, y: 100, width: 200, height: 100),
                styleMask: [.titled, .resizable, .miniaturizable], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            // The game inset by 10 points all round, as an app's layout may put it.
            view = NSView(frame: NSRect(x: 10, y: 10, width: 180, height: 80))
            window.contentView?.addSubview(view)
        }

        private func service(frontmost: Bool = true, cursorAt location: NSPoint = NSPoint(x: 50, y: 50))
            -> MigoWindowService
        {
            let recorder = recorder
            let capture = MigoWindowService.PointerCapture(
                isFrontmost: { _ in frontmost }, location: { _ in location },
                engage: { recorder.engaged += 1 }, release: { recorder.released += 1 })
            return MigoWindowService(view: view, capture: capture) { event, object in
                let json = try! JSONSerialization.data(withJSONObject: object, options: .sortedKeys)
                recorder.events.append("\(event) \(String(decoding: json, as: UTF8.self))")
            }
        }

        private func request(_ method: UInt32, _ object: [String: Any] = [:], answered: Bool = true)
            -> MigoHostServiceRequest
        {
            let recorder = recorder
            return MigoHostServiceRequest(
                service: .window, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                reply: answered ? MigoHostServiceReply { recorder.outcomes.append($0) } : nil)
        }

        private typealias Method = MigoHostServiceID.Window

        // MARK: - size

        func testTheWindowGrowsByWhatTheGameMust() {
            let service = service()
            let scale = window.backingScaleFactor
            let top = window.frame.maxY
            service.handle(request(Method.setWindowSize, ["width": 300 * scale, "height": 150 * scale]))
            XCTAssertEqual(recorder.outcomes, [.success(json: Data("{}".utf8))])
            // The game asked for 300 x 150 points and had 180 x 80: the window
            // grows by the difference, its title bar where it was.
            XCTAssertEqual(window.contentRect(forFrameRect: window.frame).size, NSSize(width: 320, height: 170))
            XCTAssertEqual(window.frame.maxY, top)
        }

        func testASizeThatIsNoSizeOrNoWindowFails() {
            let service = service()
            service.handle(request(Method.setWindowSize, ["width": 0, "height": 10]))
            service.handle(request(Method.setWindowSize, ["width": "wide", "height": 10]))
            view.removeFromSuperview()
            service.handle(request(Method.setWindowSize, ["width": 10, "height": 10]))
            XCTAssertEqual(
                recorder.outcomes,
                [
                    .failure(message: "width and height must be positive numbers", code: nil),
                    .failure(message: "width and height must be positive numbers", code: nil),
                    .failure(message: "there is no window to size", code: nil),
                ])
        }

        // MARK: - cursor

        func testKeywordsAreTheSystemsCursors() {
            let cursor = MigoWindowService.cursor(forKeyword:)
            XCTAssertTrue(cursor("pointer") === NSCursor.pointingHand)
            XCTAssertTrue(cursor("text") === NSCursor.iBeam)
            XCTAssertTrue(cursor("grab") === NSCursor.openHand)
            XCTAssertTrue(cursor("grabbing") === NSCursor.closedHand)
            XCTAssertTrue(cursor("not-allowed") === NSCursor.operationNotAllowed)
            XCTAssertTrue(cursor("crosshair") === NSCursor.crosshair)
            // What macOS has no public cursor for, and the system's own.
            XCTAssertTrue(cursor("help") === NSCursor.arrow)
            XCTAssertTrue(cursor("default") === NSCursor.arrow)
            // None is a cursor that shows nothing, not the arrow.
            XCTAssertFalse(cursor("none") === NSCursor.arrow)
            if #available(macOS 15, *) {
                XCTAssertTrue(cursor("nwse-resize") === NSCursor.frameResize(position: .topLeft, directions: .all))
                XCTAssertTrue(cursor("col-resize") === NSCursor.columnResize)
                XCTAssertTrue(cursor("zoom-in") === NSCursor.zoomIn)
            } else {
                XCTAssertTrue(cursor("ns-resize") === NSCursor.resizeUpDown)
            }
        }

        func testAnImageIsACursorWithItsHotSpotClampedIntoIt() throws {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: directory) }
            let rep = try XCTUnwrap(
                NSBitmapImageRep(
                    bitmapDataPlanes: nil, pixelsWide: 32, pixelsHigh: 32, bitsPerSample: 8, samplesPerPixel: 4,
                    hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
            let png = directory.appendingPathComponent("cursor.png")
            try XCTUnwrap(rep.representation(using: .png, properties: [:])).write(to: png)
            let text = directory.appendingPathComponent("cursor.txt")
            try Data("not an image".utf8).write(to: text)

            let cursor = try XCTUnwrap(MigoWindowService.cursor(atPath: png.path, hotSpot: CGPoint(x: 8, y: 100)))
            XCTAssertEqual(cursor.hotSpot, NSPoint(x: 8, y: 31))
            XCTAssertNil(MigoWindowService.cursor(atPath: text.path, hotSpot: .zero))

            // An unreadable image leaves the cursor as it was.
            let service = service()
            service.handle(request(Method.setCursor, ["keyword": "pointer"], answered: false))
            XCTAssertTrue(service.cursor === NSCursor.pointingHand)
            service.handle(request(Method.setCursor, ["path": text.path, "x": 0, "y": 0], answered: false))
            XCTAssertTrue(service.cursor === NSCursor.pointingHand)
            service.handle(request(Method.setCursor, ["path": png.path, "x": 4, "y": 4], answered: false))
            XCTAssertEqual(service.cursor.hotSpot, NSPoint(x: 4, y: 4))
            service.close()
            XCTAssertTrue(service.cursor === NSCursor.arrow)
        }

        // MARK: - pointer lock

        func testALockFreezesTheCursorAndTheMotionMovesThePointer() {
            let service = service(cursorAt: NSPoint(x: 50, y: 40))
            service.handle(request(Method.requestPointerLock, answered: false))
            XCTAssertTrue(service.pointerLocked)
            XCTAssertEqual(recorder.engaged, 1)
            // Where the cursor stood, in the view: (40, 30) from its origin.
            XCTAssertEqual(service.lockedPosition, CGPoint(x: 40, y: 30))
            XCTAssertEqual(service.movePointer(by: CGVector(dx: 3, dy: -2)), CGPoint(x: 43, y: 28))
            // Past the view's edges, as far as the mouse goes.
            XCTAssertEqual(service.movePointer(by: CGVector(dx: 500, dy: 0)), CGPoint(x: 543, y: 28))

            // Held already: nothing more.
            service.handle(request(Method.requestPointerLock, answered: false))
            XCTAssertEqual(recorder.engaged, 1)

            service.handle(request(Method.exitPointerLock, answered: false))
            XCTAssertFalse(service.pointerLocked)
            XCTAssertNil(service.movePointer(by: CGVector(dx: 1, dy: 1)))
            XCTAssertEqual(recorder.released, 1)
            service.handle(request(Method.exitPointerLock, answered: false))
            XCTAssertEqual(recorder.released, 1)

            let change = MigoHostServiceID.Window.Event.pointerLockChange
            XCTAssertEqual(recorder.events, ["\(change) {\"locked\":true}", "\(change) {\"locked\":false}"])
        }

        func testALockIsRefusedAwayFromTheWindowOrTheGame() {
            let change = MigoHostServiceID.Window.Event.pointerLockChange
            service(frontmost: false).handle(request(Method.requestPointerLock, answered: false))
            // The cursor over the window's margin, not the game.
            service(cursorAt: NSPoint(x: 5, y: 5)).handle(request(Method.requestPointerLock, answered: false))
            XCTAssertEqual(recorder.engaged, 0)
            XCTAssertEqual(recorder.events, Array(repeating: "\(change) {\"locked\":false}", count: 2))
        }

        func testTheLockIsReleasedWhenTheWindowLosesKeyOrTheGameEnds() {
            let service = service()
            service.handle(request(Method.requestPointerLock, answered: false))
            NotificationCenter.default.post(name: NSWindow.didResignKeyNotification, object: window)
            XCTAssertFalse(service.pointerLocked)
            XCTAssertEqual(recorder.released, 1)

            service.handle(request(Method.requestPointerLock, answered: false))
            service.close()
            XCTAssertEqual(recorder.released, 2)
            // A request that arrives after the game ended takes nothing.
            service.handle(request(Method.requestPointerLock, answered: false))
            XCTAssertEqual(recorder.engaged, 2)

            // However the service goes, the machine's mouse is let go.
            var dropped: MigoWindowService? = self.service()
            dropped?.handle(request(Method.requestPointerLock, answered: false))
            dropped = nil
            XCTAssertEqual(recorder.engaged, 3)
            XCTAssertEqual(recorder.released, 3)
        }

        // MARK: - state

        func testEachChangeOfStateIsReportedOnce() {
            let service = service()
            let state = MigoHostServiceID.Window.Event.windowStateChange
            let center = NotificationCenter.default
            // The state the game starts in is no change.
            center.post(name: NSWindow.didResizeNotification, object: window)
            window.atFullSize = true
            center.post(name: NSWindow.didResizeNotification, object: window)
            center.post(name: NSWindow.didResizeNotification, object: window)
            window.inDock = true
            center.post(name: NSWindow.didMiniaturizeNotification, object: window)
            window.inDock = false
            window.atFullSize = false
            center.post(name: NSWindow.didDeminiaturizeNotification, object: window)
            service.close()
            window.atFullSize = true
            center.post(name: NSWindow.didResizeNotification, object: window)
            XCTAssertEqual(
                recorder.events,
                [
                    "\(state) {\"state\":\"maximize\"}", "\(state) {\"state\":\"minimize\"}",
                    "\(state) {\"state\":\"normalize\"}",
                ])
        }

        // MARK: - composition

        func testTheAppsServicesComeFirst() {
            final class App: MigoHostServiceProvider {
                var services: Set<MigoHostServiceID> { [.window] }
                var handled = 0
                func handle(_ request: MigoHostServiceRequest) { handled += 1 }
            }
            let app = App()
            let mine = MigoGameViewHostServices(app: nil, window: service())
            XCTAssertEqual(mine.services, MigoGameViewHostServices.services)
            let theirs = MigoGameViewHostServices(app: app, window: service())
            theirs.handle(request(Method.requestPointerLock, answered: false))
            XCTAssertEqual(app.handled, 1)
            XCTAssertEqual(recorder.engaged, 0)
        }
    }
#endif
