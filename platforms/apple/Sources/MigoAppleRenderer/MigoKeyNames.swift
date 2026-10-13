/// DOM `KeyboardEvent.key` for the keys that produce no text, by their DOM
/// `KeyboardEvent.code`: the half of a key event that does not depend on the
/// platform's key codes. Each Apple game view maps its own codes -- the Mac's
/// virtual key codes, the iPad's HID usages -- to `code` and asks here for the
/// name; a key that is not named here produces text, and the event's own
/// characters are its `key`.
package enum MigoKeyNames {

    /// The DOM `key` for a key that produces no text, or `nil` when it does.
    package static func namedKey(forCode code: String) -> String? {
        if let modifier = modifierKey(forCode: code) { return modifier }
        if let named = namedKeys[code] { return named }
        if code.hasPrefix("F"), let number = Int(code.dropFirst()), (1...24).contains(number) { return code }
        return nil
    }

    /// The DOM `key` for a modifier: both sides of each share one name.
    package static func modifierKey(forCode code: String) -> String? {
        switch code {
        case "ShiftLeft", "ShiftRight": return "Shift"
        case "ControlLeft", "ControlRight": return "Control"
        case "AltLeft", "AltRight": return "Alt"
        case "MetaLeft", "MetaRight": return "Meta"
        case "CapsLock": return "CapsLock"
        default: return nil
        }
    }

    /// The DOM `key` for a key that produces text: what it typed, with Shift
    /// and the layout's Option applied. Control and Command do not change
    /// `key` -- Control-A is "a" -- so where one made a control character, the
    /// key's character without modifiers is the answer, with Shift's case.
    /// What gives neither is "Unidentified".
    package static func textKey(characters: String, ignoringModifiers: String, shift: Bool) -> String {
        if isText(characters) { return characters }
        let plain = shift ? ignoringModifiers.uppercased() : ignoringModifiers
        return isText(plain) ? plain : "Unidentified"
    }

    /// Something typed: not empty, and no control character or the private-use
    /// codes AppKit gives the function keys.
    private static func isText(_ text: String) -> Bool {
        !text.isEmpty
            && !text.unicodeScalars.contains { scalar in
                scalar.value < 0x20 || (0x7F...0x9F).contains(scalar.value) || (0xF700...0xF8FF).contains(scalar.value)
            }
    }

    /// Keys whose `key` is their `code`, but for the keypad's Enter.
    private static let namedKeys: [String: String] = {
        var names = [
            "Enter", "Tab", "Backspace", "Escape", "Delete", "Insert", "Home", "End", "PageUp", "PageDown",
            "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Help", "ContextMenu", "PrintScreen",
            "ScrollLock", "Pause", "NumLock", "Power", "Undo", "Cut", "Copy", "Paste", "Find", "Again",
            "Select", "AudioVolumeMute", "AudioVolumeUp", "AudioVolumeDown", "KanaMode", "Convert",
            "NonConvert",
        ].reduce(into: [String: String]()) { names, key in names[key] = key }
        names["NumpadEnter"] = "Enter"
        return names
    }()
}
