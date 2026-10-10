package com.migo.runtime.internal;

import android.view.KeyEvent;

import java.nio.ByteBuffer;

/**
 * One gamepad on the W3C standard mapping -- the layout the common mini-game
 * platform's {@code getGamepads()} follows: four axes (the sticks) and seventeen
 * buttons, from the face buttons to the home button. Android's key codes and
 * motion axes are put in their standard places here, and the state is written
 * in the layout {@code onGamepadState} reads.
 *
 * <p>Main-thread confined, like every Android input path.
 *
 * @hide
 */
final class GamepadPad {

    static final int AXES = 4;
    static final int BUTTONS = 17;
    /** Bytes {@link #write} fills: the axes, then a flags word and a value per button. */
    static final int STATE_BYTES = AXES * 4 + BUTTONS * 8;

    static final int BUTTON_L2 = 6;
    static final int BUTTON_R2 = 7;
    static final int BUTTON_UP = 12;
    static final int BUTTON_DOWN = 13;
    static final int BUTTON_LEFT = 14;
    static final int BUTTON_RIGHT = 15;

    /**
     * A trigger that reports only its travel counts as pressed past this, as
     * Chromium's gamepad backend decides for such triggers.
     */
    static final float TRIGGER_PRESS_THRESHOLD = 30f / 255f;

    final int deviceId;
    final int index;
    private final float[] axes = new float[AXES];
    private final boolean[] pressed = new boolean[BUTTONS];
    private final float[] values = new float[BUTTONS];
    /** Whether the triggers report presses of their own, as keys. */
    private boolean triggerKeys;

    GamepadPad(int deviceId, int index) {
        this.deviceId = deviceId;
        this.index = index;
    }

    /** The standard button an Android key code is, or -1. */
    static int buttonOf(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_BUTTON_A: return 0;
            case KeyEvent.KEYCODE_BUTTON_B: return 1;
            case KeyEvent.KEYCODE_BUTTON_X: return 2;
            case KeyEvent.KEYCODE_BUTTON_Y: return 3;
            case KeyEvent.KEYCODE_BUTTON_L1: return 4;
            case KeyEvent.KEYCODE_BUTTON_R1: return 5;
            case KeyEvent.KEYCODE_BUTTON_L2: return BUTTON_L2;
            case KeyEvent.KEYCODE_BUTTON_R2: return BUTTON_R2;
            // Not BACK, which some pads send for select: it stays Android's, the
            // player's way out of the game.
            case KeyEvent.KEYCODE_BUTTON_SELECT: return 8;
            case KeyEvent.KEYCODE_BUTTON_START: return 9;
            case KeyEvent.KEYCODE_BUTTON_THUMBL: return 10;
            case KeyEvent.KEYCODE_BUTTON_THUMBR: return 11;
            case KeyEvent.KEYCODE_DPAD_UP: return BUTTON_UP;
            case KeyEvent.KEYCODE_DPAD_DOWN: return BUTTON_DOWN;
            case KeyEvent.KEYCODE_DPAD_LEFT: return BUTTON_LEFT;
            case KeyEvent.KEYCODE_DPAD_RIGHT: return BUTTON_RIGHT;
            case KeyEvent.KEYCODE_BUTTON_MODE: return 16;
            default: return -1;
        }
    }

    /** A button's key going down or up. */
    void key(int button, boolean down) {
        if (button == BUTTON_L2 || button == BUTTON_R2) triggerKeys = true;
        pressed[button] = down;
        values[button] = down ? 1f : 0f;
    }

    /** The sticks, each -1..1, already past the device's dead zone. */
    void sticks(float leftX, float leftY, float rightX, float rightY) {
        axes[0] = leftX;
        axes[1] = leftY;
        axes[2] = rightX;
        axes[3] = rightY;
    }

    /** The triggers' travel, each 0..1. */
    void triggers(float left, float right) {
        trigger(BUTTON_L2, left);
        trigger(BUTTON_R2, right);
    }

    private void trigger(int button, float travel) {
        values[button] = travel;
        // A trigger with keys of its own says when it is pressed; one without
        // is pressed past the threshold.
        if (!triggerKeys) pressed[button] = travel > TRIGGER_PRESS_THRESHOLD;
    }

    /** A hat switch, each axis -1, 0 or 1 (up and left negative), as the d-pad. */
    void hat(float x, float y) {
        direction(BUTTON_LEFT, x <= -0.5f);
        direction(BUTTON_RIGHT, x >= 0.5f);
        direction(BUTTON_UP, y <= -0.5f);
        direction(BUTTON_DOWN, y >= 0.5f);
    }

    private void direction(int button, boolean down) {
        pressed[button] = down;
        values[button] = down ? 1f : 0f;
    }

    /** The state, for {@code onGamepadState}: native order, from position 0. */
    void write(ByteBuffer out) {
        out.clear();
        for (int i = 0; i < AXES; i++) out.putFloat(axes[i]);
        for (int i = 0; i < BUTTONS; i++) {
            // A pressed button is touched; a pad with no touch sensing reports
            // travel as touch.
            int flags = (pressed[i] ? 1 : 0) | (pressed[i] || values[i] > 0f ? 2 : 0);
            out.putInt(flags);
            out.putFloat(values[i]);
        }
        out.flip();
    }
}
