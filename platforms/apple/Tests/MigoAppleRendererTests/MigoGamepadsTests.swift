import Foundation
import GameController
import MigoEngine
import XCTest

@testable import MigoAppleRenderer

/// A controller as content's gamepad: its slot, the standard layout read off a
/// snapshot pad whose elements the test sets, and the effects its motors play.
final class MigoGamepadsTests: XCTestCase {
    func testAPadTakesTheLowestSlotNoPadHolds() {
        XCTAssertEqual(MigoGamepads.slot(in: []), 0)
        XCTAssertEqual(MigoGamepads.slot(in: [true, true]), 2)
        // A pad that left keeps its slot empty until the next one arrives.
        XCTAssertEqual(MigoGamepads.slot(in: [true, false, true]), 1)
    }

    func testThePadReadsInTheStandardLayout() throws {
        let pad = try XCTUnwrap(GCController.withExtendedGamepad().extendedGamepad)
        pad.buttonA.setValue(1)
        pad.rightShoulder.setValue(1)
        pad.leftTrigger.setValue(0.5)
        pad.dpad.setValueForXAxis(-1, yAxis: 0)
        pad.leftThumbstick.setValueForXAxis(0.25, yAxis: 0.5)
        pad.rightThumbstick.setValueForXAxis(-1, yAxis: -0.75)
        let snapshot = MigoGamepads.snapshot(of: pad)
        XCTAssertEqual(snapshot.axes.count, MigoGamepads.axisCount)
        XCTAssertEqual(snapshot.buttons.count, MigoGamepads.buttonCount)
        // Up on the stick is negative, as on the web.
        XCTAssertEqual(snapshot.axes, [0.25, -0.5, -1, 0.75])
        let pressed = MigoGamepadButtonFlags(MIGO_GAMEPAD_BUTTON_FLAG_PRESSED)
        let isPressed = snapshot.buttons.map { $0.flags & pressed != 0 }
        XCTAssertEqual(isPressed.enumerated().filter(\.element).map(\.offset), [0, 5, 6, 14])
        XCTAssertEqual(snapshot.buttons[6].value, 0.5)
        XCTAssertEqual(snapshot.buttons[1].value, 0)
    }

    func testTheEffectsAPadsMotorsPlay() {
        let dual = MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE)
        let trigger = MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_TRIGGER_RUMBLE)
        XCTAssertEqual(MigoGamepads.haptics(for: []), MigoGamepadHaptics(MIGO_GAMEPAD_HAPTICS_NONE))
        XCTAssertEqual(MigoGamepads.haptics(for: [.handles]), dual)
        XCTAssertEqual(MigoGamepads.haptics(for: [.default]), dual)
        XCTAssertEqual(
            MigoGamepads.haptics(for: [.leftHandle, .rightHandle, .leftTrigger, .rightTrigger]), dual | trigger)
        // One trigger motor is not trigger rumble.
        XCTAssertEqual(MigoGamepads.haptics(for: [.handles, .leftTrigger]), dual)
    }
}
