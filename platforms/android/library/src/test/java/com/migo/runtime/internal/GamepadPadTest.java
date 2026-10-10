package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;

import android.view.KeyEvent;

import org.junit.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

/**
 * A pad on the standard mapping: Android key codes and axes in their standard
 * places, written in the layout {@code onGamepadState} reads.
 */
public final class GamepadPadTest {

    private static ByteBuffer written(GamepadPad pad) {
        ByteBuffer buffer = ByteBuffer.allocate(GamepadPad.STATE_BYTES).order(ByteOrder.nativeOrder());
        pad.write(buffer);
        assertEquals(GamepadPad.STATE_BYTES, buffer.limit());
        return buffer;
    }

    private static int flags(ByteBuffer state, int button) {
        return state.getInt(GamepadPad.AXES * 4 + button * 8);
    }

    private static float value(ByteBuffer state, int button) {
        return state.getFloat(GamepadPad.AXES * 4 + button * 8 + 4);
    }

    @Test
    public void the_face_buttons_shoulders_and_d_pad_take_their_standard_places() {
        assertEquals(0, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_A));
        assertEquals(3, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_Y));
        assertEquals(5, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_R1));
        assertEquals(8, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_SELECT));
        assertEquals(9, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_START));
        assertEquals(11, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_THUMBR));
        assertEquals(15, GamepadPad.buttonOf(KeyEvent.KEYCODE_DPAD_RIGHT));
        assertEquals(16, GamepadPad.buttonOf(KeyEvent.KEYCODE_BUTTON_MODE));
        // Back stays Android's: the player's way out.
        assertEquals(-1, GamepadPad.buttonOf(KeyEvent.KEYCODE_BACK));
    }

    @Test
    public void sticks_and_buttons_are_written_axes_first() {
        GamepadPad pad = new GamepadPad(9, 0);
        pad.sticks(0.5f, -1f, 0f, 0.25f);
        pad.key(0, true);

        ByteBuffer state = written(pad);
        assertEquals(0.5f, state.getFloat(0), 0f);
        assertEquals(-1f, state.getFloat(4), 0f);
        assertEquals(0.25f, state.getFloat(12), 0f);
        assertEquals(3, flags(state, 0));
        assertEquals(1f, value(state, 0), 0f);
        assertEquals(0, flags(state, 1));

        pad.key(0, false);
        assertEquals(0, flags(written(pad), 0));
    }

    @Test
    public void an_analogue_trigger_is_pressed_past_the_threshold() {
        GamepadPad pad = new GamepadPad(9, 0);
        pad.triggers(0.05f, 0.9f);

        ByteBuffer state = written(pad);
        // Travel short of the threshold: touched, not pressed.
        assertEquals(2, flags(state, GamepadPad.BUTTON_L2));
        assertEquals(0.05f, value(state, GamepadPad.BUTTON_L2), 0f);
        assertEquals(3, flags(state, GamepadPad.BUTTON_R2));
    }

    @Test
    public void a_trigger_with_keys_of_its_own_is_pressed_by_them() {
        GamepadPad pad = new GamepadPad(9, 0);
        pad.key(GamepadPad.BUTTON_R2, true);
        pad.triggers(0f, 0.1f);
        ByteBuffer state = written(pad);
        assertEquals(3, flags(state, GamepadPad.BUTTON_R2));
        assertEquals(0.1f, value(state, GamepadPad.BUTTON_R2), 0f);

        pad.key(GamepadPad.BUTTON_R2, false);
        pad.triggers(0f, 0.9f);
        assertEquals(2, flags(written(pad), GamepadPad.BUTTON_R2));
    }

    @Test
    public void a_hat_switch_is_the_d_pad() {
        GamepadPad pad = new GamepadPad(9, 0);
        pad.hat(-1f, 1f);
        ByteBuffer state = written(pad);
        assertEquals(3, flags(state, GamepadPad.BUTTON_LEFT));
        assertEquals(3, flags(state, GamepadPad.BUTTON_DOWN));
        assertEquals(0, flags(state, GamepadPad.BUTTON_UP));
        assertEquals(0, flags(state, GamepadPad.BUTTON_RIGHT));

        pad.hat(0f, 0f);
        state = written(pad);
        assertEquals(0, flags(state, GamepadPad.BUTTON_LEFT));
        assertEquals(0f, value(state, GamepadPad.BUTTON_DOWN), 0f);
    }
}
