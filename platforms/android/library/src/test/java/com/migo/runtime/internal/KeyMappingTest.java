package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import android.view.KeyCharacterMap;
import android.view.KeyEvent;

import org.junit.Test;

/** Android key codes as DOM {@code code} and {@code key}. */
public final class KeyMappingTest {

    @Test
    public void code_names_the_physical_key() {
        assertEquals("KeyA", KeyMapping.code(KeyEvent.KEYCODE_A));
        assertEquals("KeyZ", KeyMapping.code(KeyEvent.KEYCODE_Z));
        assertEquals("Digit7", KeyMapping.code(KeyEvent.KEYCODE_7));
        assertEquals("Numpad7", KeyMapping.code(KeyEvent.KEYCODE_NUMPAD_7));
        assertEquals("ArrowLeft", KeyMapping.code(KeyEvent.KEYCODE_DPAD_LEFT));
        assertEquals("Space", KeyMapping.code(KeyEvent.KEYCODE_SPACE));
        assertEquals("F12", KeyMapping.code(KeyEvent.KEYCODE_F12));
        assertEquals("ShiftRight", KeyMapping.code(KeyEvent.KEYCODE_SHIFT_RIGHT));
        assertNull(KeyMapping.code(KeyEvent.KEYCODE_VOLUME_UP));
        assertNull(KeyMapping.code(-1));
        assertNull(KeyMapping.code(100_000));
    }

    @Test
    public void key_is_what_the_key_produces() {
        assertEquals("a", KeyMapping.key(KeyEvent.KEYCODE_A, 'a'));
        assertEquals("A", KeyMapping.key(KeyEvent.KEYCODE_A, 'A'));
        assertEquals(" ", KeyMapping.key(KeyEvent.KEYCODE_SPACE, ' '));
        assertEquals("ArrowUp", KeyMapping.key(KeyEvent.KEYCODE_DPAD_UP, 0));
        assertEquals("Enter", KeyMapping.key(KeyEvent.KEYCODE_NUMPAD_ENTER, '\n'));
        assertEquals("Shift", KeyMapping.key(KeyEvent.KEYCODE_SHIFT_LEFT, 0));
        assertEquals("Dead", KeyMapping.key(KeyEvent.KEYCODE_GRAVE,
                KeyCharacterMap.COMBINING_ACCENT | 0x300));
        assertEquals("Unidentified", KeyMapping.key(KeyEvent.KEYCODE_A, 0));
    }

    @Test
    public void modifiers_are_the_dom_mask() {
        assertEquals(0, KeyMapping.modifiers(0));
        assertEquals(KeyMapping.MODIFIER_CONTROL | KeyMapping.MODIFIER_SHIFT,
                KeyMapping.modifiers(KeyEvent.META_CTRL_ON | KeyEvent.META_SHIFT_LEFT_ON
                        | KeyEvent.META_SHIFT_ON));
        assertEquals(KeyMapping.MODIFIER_ALT | KeyMapping.MODIFIER_META,
                KeyMapping.modifiers(KeyEvent.META_ALT_ON | KeyEvent.META_META_ON));
    }
}
