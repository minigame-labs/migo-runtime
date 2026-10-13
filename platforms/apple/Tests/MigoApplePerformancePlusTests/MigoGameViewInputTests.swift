import Foundation
import MigoEngine
import XCTest

@testable import MigoApplePerformancePlus
@testable import MigoAppleRenderer

#if os(iOS)
    import UIKit

    /// The iPad's keyboard and pointer before the engine: each key's DOM name
    /// and position, the buttons a pointer event holds, and the window service
    /// -- the pointer's style, the window it does not size, and pointer lock
    /// against a fake system side, so no test needs a scene that can lock.
    final class MigoGameViewInputTests: XCTestCase {

        // MARK: - keys

        private func key(_ usage: Int, _ characters: String, _ plain: String, _ flags: UIKeyModifierFlags = [])
            -> [String]?
        {
            MigoKeyboardHIDCodes.key(
                usage: usage, characters: characters, charactersIgnoringModifiers: plain, modifiers: flags
            ).map { [$0.key, $0.code] }
        }

        /// `code` is where the key is, whatever the layout; `key` is what it
        /// typed, with Shift, and without Control's control character.
        func testAKeyIsItsPositionAndWhatItTyped() {
            XCTAssertEqual(key(0x04, "a", "a"), ["a", "KeyA"])
            XCTAssertEqual(key(0x04, "A", "a", .shift), ["A", "KeyA"])
            // On a French layout the US layout's Q position types "a".
            XCTAssertEqual(key(0x14, "a", "a"), ["a", "KeyQ"])
            XCTAssertEqual(key(0x04, "\u{1}", "a", .control), ["a", "KeyA"])
            XCTAssertEqual(key(0x04, "\u{1}", "a", [.control, .shift]), ["A", "KeyA"])
            XCTAssertEqual(key(0x04, "a", "a", .command), ["a", "KeyA"])
            XCTAssertEqual(key(0x1E, "!", "1", .shift), ["!", "Digit1"])
            XCTAssertEqual(key(0x27, "0", "0"), ["0", "Digit0"])
            XCTAssertEqual(key(0x2C, " ", " "), [" ", "Space"])
            XCTAssertEqual(key(0x59, "1", "1"), ["1", "Numpad1"])
            XCTAssertEqual(key(0x62, "0", "0"), ["0", "Numpad0"])
            XCTAssertEqual(key(0x64, "<", "<"), ["<", "IntlBackslash"])
            XCTAssertEqual(key(0x32, "#", "#"), ["#", "Backslash"], "the ISO key beside Enter")
        }

        /// A key that types nothing is named by where it is, both sides of a
        /// modifier by one name, the keypad's Enter as Enter.
        func testAKeyThatTypesNothingIsNamed() {
            XCTAssertEqual(key(0x28, "\r", "\r"), ["Enter", "Enter"])
            XCTAssertEqual(key(0x58, "\r", "\r"), ["Enter", "NumpadEnter"])
            XCTAssertEqual(key(0x29, "\u{1B}", "\u{1B}"), ["Escape", "Escape"])
            XCTAssertEqual(key(0x2A, "\u{8}", "\u{8}"), ["Backspace", "Backspace"])
            XCTAssertEqual(key(0x52, "UIKeyInputUpArrow", "UIKeyInputUpArrow"), ["ArrowUp", "ArrowUp"])
            XCTAssertEqual(key(0xE1, "", ""), ["Shift", "ShiftLeft"])
            XCTAssertEqual(key(0xE5, "", ""), ["Shift", "ShiftRight"])
            XCTAssertEqual(key(0xE7, "", ""), ["Meta", "MetaRight"])
            XCTAssertEqual(key(0xE2, "", ""), ["Alt", "AltLeft"])
            XCTAssertEqual(key(0x39, "", ""), ["CapsLock", "CapsLock"])
            XCTAssertEqual(key(0x3A, "", ""), ["F1", "F1"])
            XCTAssertEqual(key(0x45, "", ""), ["F12", "F12"])
            XCTAssertEqual(key(0x68, "", ""), ["F13", "F13"])
            XCTAssertEqual(key(0x73, "", ""), ["F24", "F24"])
            XCTAssertEqual(key(0x4C, "", ""), ["Delete", "Delete"])
            XCTAssertEqual(key(0x90, "", ""), ["Unidentified", "Lang1"])
            XCTAssertNil(key(0x01, "", ""), "ErrorRollOver is no key")
        }

        func testTheModifiersAreTheABIsBits() {
            XCTAssertEqual(MigoKeyboardHIDCodes.modifiers([]), MIGO_KEY_MODIFIER_NONE)
            XCTAssertEqual(
                MigoKeyboardHIDCodes.modifiers([.shift, .command]), MIGO_KEY_MODIFIER_SHIFT | MIGO_KEY_MODIFIER_META)
            XCTAssertEqual(
                MigoKeyboardHIDCodes.modifiers([.control, .alternate, .alphaShift]),
                MIGO_KEY_MODIFIER_CONTROL | MIGO_KEY_MODIFIER_ALT)
        }

        // MARK: - buttons

        /// UIKit numbers the middle button third, the DOM second.
        func testAButtonMaskIsTheDOMsButtons() {
            XCTAssertEqual(MigoPointerButtons.buttons(in: []), [])
            XCTAssertEqual(MigoPointerButtons.buttons(in: .primary), [0])
            XCTAssertEqual(MigoPointerButtons.buttons(in: .secondary), [2])
            XCTAssertEqual(MigoPointerButtons.buttons(in: .button(3)), [1])
            XCTAssertEqual(MigoPointerButtons.buttons(in: [.primary, .button(4), .button(5)]), [0, 3, 4])
        }

        /// UIKit's masks and GameController's buttons are one set: each press
        /// and each release is heard once, whichever source says it first.
        func testEachPressIsHeardOnceWhicheverSourceSaysItFirst() {
            var buttons = MigoPointerButtons()
            XCTAssertEqual(buttons.hold([0]).pressed, [0])
            XCTAssertFalse(buttons.set(0, down: true), "the mouse reporting the same press")
            XCTAssertTrue(buttons.set(2, down: true))
            let same = buttons.hold([0, 2])
            XCTAssertEqual(same.pressed, [])
            XCTAssertEqual(same.released, [])
            XCTAssertEqual(buttons.moving, 0, "a move reports the primary button first")
            XCTAssertEqual(buttons.hold([2]).released, [0])
            XCTAssertEqual(buttons.moving, 2)
            XCTAssertTrue(buttons.set(2, down: false))
            XCTAssertFalse(buttons.set(2, down: false))
            XCTAssertEqual(buttons.moving, 0)
        }

        // MARK: - window service

        private final class Recorder {
            var events: [String] = []
            var updates: [Bool] = []
            var outcomes: [MigoHostServiceReply.Outcome] = []
        }

        private let recorder = Recorder()
        private var view: UIView!

        override func setUp() {
            super.setUp()
            view = UIView(frame: CGRect(x: 0, y: 0, width: 200, height: 100))
        }

        /// A service whose scene can lock (`available`) and whose view
        /// controller passes the view's preference on (`forwards`).
        private func service(available: Bool = true, forwards: Bool = true) -> MigoWindowService {
            let recorder = recorder
            let system = MigoWindowService.PointerLockSystem(
                isAvailable: { available },
                update: { prefersLocked in
                    recorder.updates.append(prefersLocked)
                    // A controller that does not ask the view prefers no lock.
                    return forwards || !prefersLocked
                })
            return MigoWindowService(view: view, system: system) { event, object in
                let json = try! JSONSerialization.data(withJSONObject: object, options: .sortedKeys)
                recorder.events.append("\(event) \(String(decoding: json, as: UTF8.self))")
            }
        }

        private func request(_ method: UInt32, _ object: [String: Any] = [:], answered: Bool = false)
            -> MigoHostServiceRequest
        {
            let recorder = recorder
            return MigoHostServiceRequest(
                service: .window, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
                reply: answered ? MigoHostServiceReply { recorder.outcomes.append($0) } : nil)
        }

        private typealias Method = MigoHostServiceID.Window
        private let locked = "1 {\"locked\":true}"
        private let released = "1 {\"locked\":false}"

        /// Asked of the controller and taken when the scene reports it: then
        /// content hears it, its pointer stands where the pointer did and moves
        /// by the mouse's motion; released by content, it hears that at once.
        func testALockIsTakenWhenTheSceneReportsItAndMovesByTheMouse() {
            let service = service()
            service.pointerLocation = CGPoint(x: 50, y: 40)
            service.handle(request(Method.requestPointerLock))
            XCTAssertTrue(service.prefersLocked)
            XCTAssertEqual(recorder.updates, [true])
            XCTAssertEqual(recorder.events, [], "nothing until the scene says")
            service.systemLockChanged(true)
            XCTAssertEqual(recorder.events, [locked])
            XCTAssertEqual(service.lockedPosition, CGPoint(x: 50, y: 40))
            XCTAssertEqual(service.movePointer(by: CGVector(dx: 5, dy: -3)), CGPoint(x: 55, y: 37))
            XCTAssertEqual(service.movePointer(by: CGVector(dx: -100, dy: 0)), CGPoint(x: -45, y: 37))
            service.handle(request(Method.requestPointerLock))
            XCTAssertEqual(recorder.updates, [true], "held already")
            service.handle(request(Method.exitPointerLock))
            XCTAssertEqual(recorder.events, [locked, released])
            XCTAssertEqual(recorder.updates, [true, false])
            XCTAssertFalse(service.prefersLocked)
            XCTAssertNil(service.movePointer(by: CGVector(dx: 1, dy: 1)))
            service.systemLockChanged(false)
            XCTAssertEqual(recorder.events, [locked, released], "the scene catching up is not a second release")
        }

        /// The system takes a lock back -- the app left the screen, the scene
        /// left full screen -- and content hears it and must ask again.
        func testALockTheSystemTakesBackIsHeardAndNotAskedForAgain() {
            let service = service()
            service.pointerLocation = CGPoint(x: 1, y: 2)
            service.handle(request(Method.requestPointerLock))
            service.systemLockChanged(true)
            service.systemLockChanged(false)
            XCTAssertEqual(recorder.events, [locked, released])
            XCTAssertFalse(service.prefersLocked)
            XCTAssertEqual(recorder.updates, [true, false])
            service.systemLockChanged(true)
            XCTAssertEqual(recorder.events, [locked, released], "a lock content no longer asks for is not content's")
        }

        /// Refused at once: with the pointer away from the game, where no scene
        /// can lock (an iPhone), and where the app's controller does not pass
        /// the view's preference on -- iPadOS would never be asked.
        func testALockIsRefusedWhereItCouldNeverBeTaken() {
            let away = service()
            away.handle(request(Method.requestPointerLock))
            let phone = service(available: false)
            phone.pointerLocation = CGPoint(x: 1, y: 1)
            phone.handle(request(Method.requestPointerLock))
            XCTAssertEqual(recorder.updates, [])
            let unwired = service(forwards: false)
            unwired.pointerLocation = CGPoint(x: 1, y: 1)
            unwired.handle(request(Method.requestPointerLock))
            XCTAssertFalse(unwired.prefersLocked)
            XCTAssertEqual(recorder.updates, [true, false])
            XCTAssertEqual(recorder.events, [released, released, released])
        }

        /// A request still waiting on the system is dropped quietly: content
        /// never held a lock, so it hears no release, and a lock the system
        /// grants afterwards is not taken.
        func testARequestStillWaitingIsDroppedQuietly() {
            let service = service()
            service.pointerLocation = CGPoint(x: 1, y: 1)
            service.handle(request(Method.requestPointerLock))
            service.handle(request(Method.exitPointerLock))
            service.systemLockChanged(true)
            XCTAssertEqual(recorder.events, [])
            XCTAssertEqual(recorder.updates, [true, false])
            XCTAssertNil(service.lockedPosition)
        }

        /// The cursor is a keyword's style; an image, which iPadOS cannot show
        /// as the pointer, leaves it as it was.
        func testTheCursorIsAKeywordsStyleAndAnImageLeavesItAlone() {
            let service = service()
            XCTAssertTrue(view.interactions.contains { $0 is UIPointerInteraction })
            XCTAssertEqual(service.appearance, .system)
            service.handle(request(Method.setCursor, ["keyword": "crosshair"]))
            XCTAssertEqual(service.appearance, .crosshair)
            service.handle(request(Method.setCursor, ["path": "/tmp/aim.png", "x": 1, "y": 2]))
            XCTAssertEqual(service.appearance, .crosshair)
            for (keyword, appearance) in [
                ("none", MigoWindowService.PointerAppearance.hidden), ("text", .textBeam),
                ("vertical-text", .verticalTextBeam), ("pointer", .system), ("default", .system),
                ("nwse-resize", .system),
            ] {
                service.handle(request(Method.setCursor, ["keyword": keyword]))
                XCTAssertEqual(service.appearance, appearance, keyword)
                _ = MigoWindowService.style(for: appearance)
            }
            service.handle(request(Method.setCursor, ["keyword": "crosshair"]))
            service.close()
            XCTAssertEqual(service.appearance, .system)
            XCTAssertFalse(view.interactions.contains { $0 is UIPointerInteraction }, "the game's style goes with it")
        }

        /// The game ending gives the lock back.
        func testTheGameEndingGivesTheLockBack() {
            let service = service()
            service.pointerLocation = CGPoint(x: 1, y: 1)
            service.handle(request(Method.requestPointerLock))
            service.systemLockChanged(true)
            service.close()
            XCTAssertFalse(service.prefersLocked)
            XCTAssertNil(service.lockedPosition)
            XCTAssertEqual(recorder.updates, [true, false])
        }

        /// The scene's size is the system's to give.
        func testTheWindowIsNotTheGamesToSize() {
            let service = service()
            service.handle(request(Method.setWindowSize, ["width": 100, "height": 100], answered: true))
            XCTAssertEqual(recorder.outcomes, [.failure(message: "not supported", code: nil)])
        }
    }
#endif
