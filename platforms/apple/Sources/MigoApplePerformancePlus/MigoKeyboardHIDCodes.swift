#if os(iOS)
    import MigoAppleRenderer
    import MigoEngine
    import UIKit

    /// A hardware keyboard's keys, as UIKit reports them (`UIKey.keyCode`, the
    /// USB HID keyboard page's usages), to DOM `KeyboardEvent.code`.
    ///
    /// A HID usage names a physical position on the keyboard whatever its
    /// layout -- exactly what `code` means -- so the table is layout-independent
    /// and `key` comes from the key's own characters (`MigoKeyNames`, shared
    /// with the Mac). The pairs are the UI Events code specification's table of
    /// USB usages.
    enum MigoKeyboardHIDCodes {

        static func code(forUsage usage: Int) -> String? { codes[usage] }

        /// The DOM `key` and `code` for one press, or `nil` for a key with no
        /// DOM code.
        static func key(
            usage: Int, characters: String, charactersIgnoringModifiers: String, modifiers: UIKeyModifierFlags
        ) -> (key: String, code: String)? {
            guard let code = code(forUsage: usage) else { return nil }
            let key =
                MigoKeyNames.namedKey(forCode: code)
                ?? MigoKeyNames.textKey(
                    characters: characters, ignoringModifiers: charactersIgnoringModifiers,
                    shift: modifiers.contains(.shift))
            return (key, code)
        }

        /// The ABI's modifier bits for UIKit's.
        static func modifiers(_ flags: UIKeyModifierFlags) -> MigoKeyModifiers {
            var modifiers = MIGO_KEY_MODIFIER_NONE
            if flags.contains(.control) { modifiers |= MIGO_KEY_MODIFIER_CONTROL }
            if flags.contains(.shift) { modifiers |= MIGO_KEY_MODIFIER_SHIFT }
            if flags.contains(.alternate) { modifiers |= MIGO_KEY_MODIFIER_ALT }
            if flags.contains(.command) { modifiers |= MIGO_KEY_MODIFIER_META }
            return modifiers
        }

        private static let codes: [Int: String] = {
            var codes: [Int: String] = [:]
            for (offset, letter) in "ABCDEFGHIJKLMNOPQRSTUVWXYZ".enumerated() {
                codes[0x04 + offset] = "Key\(letter)"
            }
            for digit in 1...9 { codes[0x1D + digit] = "Digit\(digit)" }
            codes[0x27] = "Digit0"
            for number in 1...12 { codes[0x39 + number] = "F\(number)" }
            for number in 13...24 { codes[0x5B + number] = "F\(number)" }
            for digit in 1...9 { codes[0x58 + digit] = "Numpad\(digit)" }
            let named: [Int: String] = [
                0x28: "Enter", 0x29: "Escape", 0x2A: "Backspace", 0x2B: "Tab", 0x2C: "Space",
                0x2D: "Minus", 0x2E: "Equal", 0x2F: "BracketLeft", 0x30: "BracketRight",
                // The ISO keyboard's key beside Enter (#~ on a UK layout) is the
                // US layout's Backslash position, as the specification has it.
                0x31: "Backslash", 0x32: "Backslash", 0x33: "Semicolon", 0x34: "Quote",
                0x35: "Backquote", 0x36: "Comma", 0x37: "Period", 0x38: "Slash", 0x39: "CapsLock",
                0x46: "PrintScreen", 0x47: "ScrollLock", 0x48: "Pause", 0x49: "Insert", 0x4A: "Home",
                0x4B: "PageUp", 0x4C: "Delete", 0x4D: "End", 0x4E: "PageDown", 0x4F: "ArrowRight",
                0x50: "ArrowLeft", 0x51: "ArrowDown", 0x52: "ArrowUp", 0x53: "NumLock",
                0x54: "NumpadDivide", 0x55: "NumpadMultiply", 0x56: "NumpadSubtract", 0x57: "NumpadAdd",
                0x58: "NumpadEnter", 0x62: "Numpad0", 0x63: "NumpadDecimal", 0x64: "IntlBackslash",
                0x65: "ContextMenu", 0x66: "Power", 0x67: "NumpadEqual", 0x75: "Help", 0x77: "Select",
                0x79: "Again", 0x7A: "Undo", 0x7B: "Cut", 0x7C: "Copy", 0x7D: "Paste", 0x7E: "Find",
                0x7F: "AudioVolumeMute", 0x80: "AudioVolumeUp", 0x81: "AudioVolumeDown", 0x85: "NumpadComma",
                0x87: "IntlRo", 0x88: "KanaMode", 0x89: "IntlYen", 0x8A: "Convert", 0x8B: "NonConvert",
                0x90: "Lang1", 0x91: "Lang2", 0x92: "Lang3", 0x93: "Lang4", 0x94: "Lang5",
                0xE0: "ControlLeft", 0xE1: "ShiftLeft", 0xE2: "AltLeft", 0xE3: "MetaLeft",
                0xE4: "ControlRight", 0xE5: "ShiftRight", 0xE6: "AltRight", 0xE7: "MetaRight",
            ]
            codes.merge(named) { _, new in new }
            return codes
        }()
    }
#endif
