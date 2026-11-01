import XCTest

@testable import MigoMacV8

#if os(macOS)
    /// The macOS field behind `migo.showKeyboard` counts maxLength as a
    /// JavaScript string's length does -- UTF-16 units, as Android's field and
    /// the Linux Host Kit's do -- and never cuts a character in two.
    final class MigoKeyboardFieldTests: XCTestCase {
        func testMaxLengthCountsUTF16Units() {
            XCTAssertEqual(KeyboardField.prefix(of: "abcdef", utf16Units: 4), "abcd")
            XCTAssertEqual(KeyboardField.prefix(of: "abc", utf16Units: 4), "abc")
            // One character, two units: as long as "ab", where counting characters made it as long as "a".
            XCTAssertEqual(KeyboardField.prefix(of: "a\u{1F600}b", utf16Units: 3), "a\u{1F600}")
        }

        func testACharacterThatDoesNotFitWholeIsLeftOut() {
            XCTAssertEqual(KeyboardField.prefix(of: "a\u{1F600}b", utf16Units: 2), "a")
            // A family is one character of eight units.
            XCTAssertEqual(KeyboardField.prefix(of: "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x", utf16Units: 7), "")
        }
    }
#endif
