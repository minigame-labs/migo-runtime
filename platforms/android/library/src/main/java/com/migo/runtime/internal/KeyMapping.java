package com.migo.runtime.internal;

import android.view.KeyCharacterMap;
import android.view.KeyEvent;

/**
 * Android key codes as the DOM's {@code KeyboardEvent.code} and {@code key}.
 *
 * <p>{@code code} names the physical key ({@code "KeyA"}, {@code "ArrowLeft"}) and
 * {@code key} what it produces with the modifiers held ({@code "a"}, {@code "A"},
 * {@code "ArrowLeft"}). Translating them is the host's work, as the C ABI's
 * {@code MigoKeyEvent} says: the engine takes DOM values only.
 *
 * @hide
 */
public final class KeyMapping {

    private KeyMapping() {}

    /** {@code MIGO_KEY_MODIFIER_*}. */
    static final int MODIFIER_CONTROL = 1;
    static final int MODIFIER_SHIFT = 1 << 1;
    static final int MODIFIER_ALT = 1 << 2;
    static final int MODIFIER_META = 1 << 3;

    /** Above every key code Android defines; a code past it has no DOM name here. */
    private static final int KEY_CODE_LIMIT = 512;
    private static final String[] CODES = new String[KEY_CODE_LIMIT];
    /** The {@code key} of a key that produces no character. */
    private static final String[] NAMED = new String[KEY_CODE_LIMIT];

    private static void key(int keyCode, String code, String named) {
        CODES[keyCode] = code;
        NAMED[keyCode] = named;
    }

    static {
        for (int i = 0; i < 26; i++) {
            CODES[KeyEvent.KEYCODE_A + i] = "Key" + (char) ('A' + i);
        }
        for (int i = 0; i < 10; i++) {
            CODES[KeyEvent.KEYCODE_0 + i] = "Digit" + i;
            CODES[KeyEvent.KEYCODE_NUMPAD_0 + i] = "Numpad" + i;
        }
        for (int i = 0; i < 12; i++) {
            key(KeyEvent.KEYCODE_F1 + i, "F" + (i + 1), "F" + (i + 1));
        }
        key(KeyEvent.KEYCODE_DPAD_UP, "ArrowUp", "ArrowUp");
        key(KeyEvent.KEYCODE_DPAD_DOWN, "ArrowDown", "ArrowDown");
        key(KeyEvent.KEYCODE_DPAD_LEFT, "ArrowLeft", "ArrowLeft");
        key(KeyEvent.KEYCODE_DPAD_RIGHT, "ArrowRight", "ArrowRight");
        key(KeyEvent.KEYCODE_ENTER, "Enter", "Enter");
        key(KeyEvent.KEYCODE_NUMPAD_ENTER, "NumpadEnter", "Enter");
        key(KeyEvent.KEYCODE_DEL, "Backspace", "Backspace");
        key(KeyEvent.KEYCODE_FORWARD_DEL, "Delete", "Delete");
        key(KeyEvent.KEYCODE_TAB, "Tab", "Tab");
        key(KeyEvent.KEYCODE_ESCAPE, "Escape", "Escape");
        key(KeyEvent.KEYCODE_SHIFT_LEFT, "ShiftLeft", "Shift");
        key(KeyEvent.KEYCODE_SHIFT_RIGHT, "ShiftRight", "Shift");
        key(KeyEvent.KEYCODE_CTRL_LEFT, "ControlLeft", "Control");
        key(KeyEvent.KEYCODE_CTRL_RIGHT, "ControlRight", "Control");
        key(KeyEvent.KEYCODE_ALT_LEFT, "AltLeft", "Alt");
        key(KeyEvent.KEYCODE_ALT_RIGHT, "AltRight", "Alt");
        key(KeyEvent.KEYCODE_META_LEFT, "MetaLeft", "Meta");
        key(KeyEvent.KEYCODE_META_RIGHT, "MetaRight", "Meta");
        key(KeyEvent.KEYCODE_CAPS_LOCK, "CapsLock", "CapsLock");
        key(KeyEvent.KEYCODE_NUM_LOCK, "NumLock", "NumLock");
        key(KeyEvent.KEYCODE_SCROLL_LOCK, "ScrollLock", "ScrollLock");
        key(KeyEvent.KEYCODE_FUNCTION, "Fn", "Fn");
        key(KeyEvent.KEYCODE_MOVE_HOME, "Home", "Home");
        key(KeyEvent.KEYCODE_MOVE_END, "End", "End");
        key(KeyEvent.KEYCODE_PAGE_UP, "PageUp", "PageUp");
        key(KeyEvent.KEYCODE_PAGE_DOWN, "PageDown", "PageDown");
        key(KeyEvent.KEYCODE_INSERT, "Insert", "Insert");
        key(KeyEvent.KEYCODE_SYSRQ, "PrintScreen", "PrintScreen");
        key(KeyEvent.KEYCODE_BREAK, "Pause", "Pause");
        CODES[KeyEvent.KEYCODE_SPACE] = "Space";
        CODES[KeyEvent.KEYCODE_MINUS] = "Minus";
        CODES[KeyEvent.KEYCODE_EQUALS] = "Equal";
        CODES[KeyEvent.KEYCODE_LEFT_BRACKET] = "BracketLeft";
        CODES[KeyEvent.KEYCODE_RIGHT_BRACKET] = "BracketRight";
        CODES[KeyEvent.KEYCODE_BACKSLASH] = "Backslash";
        CODES[KeyEvent.KEYCODE_SEMICOLON] = "Semicolon";
        CODES[KeyEvent.KEYCODE_APOSTROPHE] = "Quote";
        CODES[KeyEvent.KEYCODE_GRAVE] = "Backquote";
        CODES[KeyEvent.KEYCODE_COMMA] = "Comma";
        CODES[KeyEvent.KEYCODE_PERIOD] = "Period";
        CODES[KeyEvent.KEYCODE_SLASH] = "Slash";
        CODES[KeyEvent.KEYCODE_NUMPAD_DIVIDE] = "NumpadDivide";
        CODES[KeyEvent.KEYCODE_NUMPAD_MULTIPLY] = "NumpadMultiply";
        CODES[KeyEvent.KEYCODE_NUMPAD_SUBTRACT] = "NumpadSubtract";
        CODES[KeyEvent.KEYCODE_NUMPAD_ADD] = "NumpadAdd";
        CODES[KeyEvent.KEYCODE_NUMPAD_DOT] = "NumpadDecimal";
        CODES[KeyEvent.KEYCODE_NUMPAD_COMMA] = "NumpadComma";
        CODES[KeyEvent.KEYCODE_NUMPAD_EQUALS] = "NumpadEqual";
        CODES[KeyEvent.KEYCODE_NUMPAD_LEFT_PAREN] = "NumpadParenLeft";
        CODES[KeyEvent.KEYCODE_NUMPAD_RIGHT_PAREN] = "NumpadParenRight";
    }

    /** The DOM {@code code} of an Android key, or null for a key the DOM has none for. */
    public static String code(int keyCode) {
        return keyCode >= 0 && keyCode < CODES.length ? CODES[keyCode] : null;
    }

    /**
     * The DOM {@code key}: a named key's name, the character a key produces with
     * {@code metaState}, {@code "Dead"} for a dead key, else {@code "Unidentified"}.
     */
    public static String key(int keyCode, int unicodeChar) {
        String named = keyCode >= 0 && keyCode < NAMED.length ? NAMED[keyCode] : null;
        if (named != null) return named;
        if ((unicodeChar & KeyCharacterMap.COMBINING_ACCENT) != 0) return "Dead";
        if (unicodeChar >= 0x20 && unicodeChar != 0x7f) return new String(Character.toChars(unicodeChar));
        return "Unidentified";
    }

    /** {@code MIGO_KEY_MODIFIER_*} of an Android meta state. */
    public static int modifiers(int metaState) {
        int mask = 0;
        if ((metaState & KeyEvent.META_CTRL_ON) != 0) mask |= MODIFIER_CONTROL;
        if ((metaState & KeyEvent.META_SHIFT_ON) != 0) mask |= MODIFIER_SHIFT;
        if ((metaState & KeyEvent.META_ALT_ON) != 0) mask |= MODIFIER_ALT;
        if ((metaState & KeyEvent.META_META_ON) != 0) mask |= MODIFIER_META;
        return mask;
    }
}
