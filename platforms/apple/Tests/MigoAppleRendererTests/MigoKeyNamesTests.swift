import XCTest

@testable import MigoAppleRenderer

/// The DOM `key` both Apple game views give a key once they know its `code`:
/// a name for a key that types nothing, what it typed otherwise.
final class MigoKeyNamesTests: XCTestCase {

    func testAKeyThatTypesNothingIsNamedByItsCode() {
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "Enter"), "Enter")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "NumpadEnter"), "Enter")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "ArrowLeft"), "ArrowLeft")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "AudioVolumeUp"), "AudioVolumeUp")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "F1"), "F1")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "F24"), "F24")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "ShiftRight"), "Shift")
        XCTAssertEqual(MigoKeyNames.namedKey(forCode: "MetaLeft"), "Meta")
        XCTAssertNil(MigoKeyNames.namedKey(forCode: "F25"))
        XCTAssertNil(MigoKeyNames.namedKey(forCode: "KeyF"))
        XCTAssertNil(MigoKeyNames.namedKey(forCode: "Space"), "a space is typed")
    }

    func testBothSidesOfAModifierShareItsName() {
        XCTAssertEqual(MigoKeyNames.modifierKey(forCode: "ControlLeft"), "Control")
        XCTAssertEqual(MigoKeyNames.modifierKey(forCode: "ControlRight"), "Control")
        XCTAssertEqual(MigoKeyNames.modifierKey(forCode: "AltRight"), "Alt")
        XCTAssertEqual(MigoKeyNames.modifierKey(forCode: "CapsLock"), "CapsLock")
        XCTAssertNil(MigoKeyNames.modifierKey(forCode: "Enter"))
    }

    /// What was typed, with Shift and Option; Control's control character is
    /// not a key, so the key's own character stands in, in Shift's case.
    func testAKeyThatTypesIsWhatItTyped() {
        XCTAssertEqual(MigoKeyNames.textKey(characters: "a", ignoringModifiers: "a", shift: false), "a")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "A", ignoringModifiers: "a", shift: true), "A")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "å", ignoringModifiers: "a", shift: false), "å")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "\u{1}", ignoringModifiers: "a", shift: false), "a")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "\u{1}", ignoringModifiers: "a", shift: true), "A")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "\u{7F}", ignoringModifiers: "\u{7F}", shift: false), "Unidentified")
        // AppKit's private-use code for a function key is no text.
        XCTAssertEqual(MigoKeyNames.textKey(characters: "\u{F704}", ignoringModifiers: "\u{F704}", shift: false), "Unidentified")
        XCTAssertEqual(MigoKeyNames.textKey(characters: "", ignoringModifiers: "", shift: false), "Unidentified")
    }
}
