package com.migo.runtime.internal;

import android.content.Context;
import android.hardware.input.InputManager;
import android.os.Handler;
import android.os.Looper;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.KeyEvent;
import android.view.MotionEvent;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Locale;
import java.util.concurrent.ConcurrentHashMap;

/**
 * The gamepads connected to the device, for one session: each takes the lowest
 * free slot when it appears and keeps it until it goes, as {@code getGamepads()}
 * indexes do, and its sticks, triggers, d-pad and buttons are pushed on the
 * standard mapping each time Android reports them. Each pad's motors play what
 * its {@code vibrationActuator} asks ({@link #rumble}).
 *
 * <p>Main-thread confined: the device listener and every input path run there,
 * and a rumble from the engine's thread is posted there.
 *
 * @hide
 */
public final class GamepadInput implements InputManager.InputDeviceListener {

    /** Where pads go: {@code NativeMethods.onGamepadConnection} / {@code onGamepadState}. */
    interface Sink {
        boolean connection(int sessionId, int index, boolean connected, String id, String mapping,
                           int axisCount, int buttonCount, int haptics);

        boolean state(int sessionId, int index, int axisCount, int buttonCount, ByteBuffer state,
                      double timeMs);
    }

    /** {@code MIGO_GAMEPAD_MAX_COUNT}. */
    static final int MAX_PADS = 16;

    /** The started sessions' pads, for the engine's rumbles to find. */
    private static final ConcurrentHashMap<Integer, GamepadInput> STARTED =
            new ConcurrentHashMap<>();

    private final int sessionId;
    private final Handler main = new Handler(Looper.getMainLooper());
    private final InputManager inputManager;
    private final Sink sink;
    private final GamepadPad[] slots = new GamepadPad[MAX_PADS];
    /** Whether the pad in a slot has been told to run its motors this session. */
    private final boolean[] driven = new boolean[MAX_PADS];
    private final ByteBuffer buffer =
            ByteBuffer.allocateDirect(GamepadPad.STATE_BYTES).order(ByteOrder.nativeOrder());
    private boolean started;

    public GamepadInput(int sessionId, Context context) {
        this.sessionId = sessionId;
        this.inputManager = (InputManager) context.getSystemService(Context.INPUT_SERVICE);
        this.sink = new Sink() {
            @Override
            public boolean connection(int session, int index, boolean connected, String id,
                                      String mapping, int axisCount, int buttonCount,
                                      int haptics) {
                return NativeMethods.onGamepadConnection(session, index, connected, id, mapping,
                        axisCount, buttonCount, haptics);
            }

            @Override
            public boolean state(int session, int index, int axisCount, int buttonCount,
                                 ByteBuffer state, double timeMs) {
                return NativeMethods.onGamepadState(session, index, axisCount, buttonCount, state,
                        timeMs);
            }
        };
    }

    /** Hear pads appear and go, and announce the ones already connected. */
    public void start() {
        if (started || inputManager == null) return;
        started = true;
        STARTED.put(sessionId, this);
        inputManager.registerInputDeviceListener(this, main);
        for (int deviceId : inputManager.getInputDeviceIds()) {
            onInputDeviceAdded(deviceId);
        }
    }

    /**
     * Stop hearing pads; every pad still in a slot leaves it, its motors
     * stopped -- no effect outlives the session that played it.
     */
    public void stop() {
        if (!started) return;
        started = false;
        STARTED.remove(sessionId, this);
        inputManager.unregisterInputDeviceListener(this);
        for (int i = 0; i < MAX_PADS; i++) {
            if (slots[i] == null) continue;
            if (driven[i]) {
                InputDevice device = inputManager.getInputDevice(slots[i].deviceId);
                if (device != null) GamepadMotors.play(device, 0f, 0f, 0);
            }
            leave(i);
        }
    }

    /**
     * The engine's instruction for the motors of the session's pad in
     * {@code index}: these magnitudes for {@code durationMs}, replacing what it
     * plays; 0 stops it. Any thread.
     */
    static void rumble(int sessionId, int index, float strong, float weak, int durationMs) {
        GamepadInput input = STARTED.get(sessionId);
        if (input == null || index < 0 || index >= MAX_PADS) return;
        input.main.post(() -> input.drive(index, strong, weak, durationMs));
    }

    private void drive(int index, float strong, float weak, int durationMs) {
        GamepadPad pad = started ? slots[index] : null;
        if (pad == null) return;
        InputDevice device = inputManager.getInputDevice(pad.deviceId);
        if (device == null) return;
        GamepadMotors.play(device, strong, weak, durationMs);
        driven[index] |= durationMs > 0;
    }

    static boolean isGamepad(InputDevice device) {
        if (device == null || device.isVirtual()) return false;
        int sources = device.getSources();
        return (sources & InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD
                || (sources & InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK;
    }

    @Override
    public void onInputDeviceAdded(int deviceId) {
        if (padOf(deviceId) == null) join(inputManager.getInputDevice(deviceId));
    }

    // The pad takes the lowest free slot -- unless the session could not take
    // its arrival, when it is not in one and its next input announces it again.
    private GamepadPad join(InputDevice device) {
        if (!isGamepad(device)) return null;
        for (int index = 0; index < MAX_PADS; index++) {
            if (slots[index] != null) continue;
            String id = String.format(Locale.ROOT, "%s (STANDARD GAMEPAD Vendor: %04x Product: %04x)",
                    device.getName(), device.getVendorId(), device.getProductId());
            if (!sink.connection(sessionId, index, true, id, "standard",
                    GamepadPad.AXES, GamepadPad.BUTTONS, GamepadMotors.haptics(device))) {
                return null;
            }
            return slots[index] = new GamepadPad(device.getId(), index);
        }
        return null;
    }

    @Override
    public void onInputDeviceRemoved(int deviceId) {
        GamepadPad pad = padOf(deviceId);
        if (pad != null) leave(pad.index);
    }

    @Override
    public void onInputDeviceChanged(int deviceId) {
        boolean gamepad = isGamepad(inputManager.getInputDevice(deviceId));
        GamepadPad pad = padOf(deviceId);
        if (pad != null && !gamepad) {
            leave(pad.index);
        } else if (pad == null && gamepad) {
            onInputDeviceAdded(deviceId);
        }
    }

    private void leave(int index) {
        slots[index] = null;
        driven[index] = false;
        sink.connection(sessionId, index, false, "", "", 0, 0, 0);
    }

    private GamepadPad padOf(int deviceId) {
        for (GamepadPad pad : slots) {
            if (pad != null && pad.deviceId == deviceId) return pad;
        }
        return null;
    }

    private GamepadPad padOf(InputEvent event) {
        GamepadPad pad = padOf(event.getDeviceId());
        return pad != null || !started ? pad : join(event.getDevice());
    }

    /** A pad's sticks, triggers and hat moving; false when the event is no pad's. */
    public boolean onMotion(MotionEvent event) {
        GamepadPad pad = padOf(event);
        InputDevice device = event.getDevice();
        if (pad == null || device == null) return false;
        int source = event.getSource();
        pad.sticks(axis(event, device, source, MotionEvent.AXIS_X),
                axis(event, device, source, MotionEvent.AXIS_Y),
                axis(event, device, source, MotionEvent.AXIS_Z),
                axis(event, device, source, MotionEvent.AXIS_RZ));
        // Only the axes the pad has: a pad whose triggers and d-pad are keys must
        // not have them released by every stick motion.
        boolean ltrigger = has(device, source, MotionEvent.AXIS_LTRIGGER);
        boolean brake = has(device, source, MotionEvent.AXIS_BRAKE);
        if (ltrigger || brake) {
            pad.triggers(
                    Math.max(ltrigger ? event.getAxisValue(MotionEvent.AXIS_LTRIGGER) : 0f,
                            brake ? event.getAxisValue(MotionEvent.AXIS_BRAKE) : 0f),
                    Math.max(event.getAxisValue(MotionEvent.AXIS_RTRIGGER),
                            event.getAxisValue(MotionEvent.AXIS_GAS)));
        }
        if (has(device, source, MotionEvent.AXIS_HAT_X)) {
            pad.hat(event.getAxisValue(MotionEvent.AXIS_HAT_X),
                    event.getAxisValue(MotionEvent.AXIS_HAT_Y));
        }
        push(pad, event.getEventTime());
        return true;
    }

    /** A pad's button; false when the key is no pad's button. */
    public boolean onKey(KeyEvent event) {
        int button = GamepadPad.buttonOf(event.getKeyCode());
        if (button < 0) return false;
        GamepadPad pad = padOf(event);
        if (pad == null) return false;
        int action = event.getAction();
        if (action == KeyEvent.ACTION_DOWN && event.getRepeatCount() > 0) return true;
        if (action != KeyEvent.ACTION_DOWN && action != KeyEvent.ACTION_UP) return true;
        pad.key(button, action == KeyEvent.ACTION_DOWN);
        push(pad, event.getEventTime());
        return true;
    }

    private static boolean has(InputDevice device, int source, int axis) {
        return device.getMotionRange(axis, source) != null;
    }

    // A stick's value past the device's dead zone; inside it, 0.
    private static float axis(MotionEvent event, InputDevice device, int source, int axis) {
        InputDevice.MotionRange range = device.getMotionRange(axis, source);
        if (range == null) return 0f;
        float value = event.getAxisValue(axis);
        return Math.abs(value) > range.getFlat() ? value : 0f;
    }

    private void push(GamepadPad pad, long eventTime) {
        pad.write(buffer);
        sink.state(sessionId, pad.index, GamepadPad.AXES, GamepadPad.BUTTONS, buffer, eventTime);
    }
}
