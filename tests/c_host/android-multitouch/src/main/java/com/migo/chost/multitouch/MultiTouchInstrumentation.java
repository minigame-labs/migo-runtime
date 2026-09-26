package com.migo.chost.multitouch;

import android.app.Activity;
import android.app.Instrumentation;
import android.app.UiAutomation;
import android.content.Intent;
import android.graphics.Bitmap;
import android.os.Bundle;
import android.os.SystemClock;
import android.view.InputDevice;
import android.view.MotionEvent;

/**
 * Drives a real two-finger gesture into the C host running touch-probe and
 * reads the verdict back as pixels.
 *
 * WHY THIS EXISTS. Multi-pointer delivery through the C ABI had never run on a
 * device: a two-finger gesture cannot be synthesized from a shell there
 * (`sendevent` is refused by SELinux, `input motionevent` carries one pointer).
 * `UiAutomation.injectInputEvent` injects at the input dispatcher with the
 * shell's authority, so the event reaches the NativeActivity window exactly as
 * a finger would -- and it is part of the framework, so this APK adds no
 * dependency to a build whose every artifact is locked and verified.
 *
 * It instruments itself rather than the host: the host declares
 * android:hasCode="false", which the system will not instrument.
 *
 * WHAT IS ASSERTED. touch-probe paints the whole screen one colour chosen by
 * `e.touches.length` in JS: red untouched, green one pointer, magenta two,
 * blue once every finger has lifted. So each screenshot below is evidence of
 * the pointer count that crossed AMotionEvent -> migo_session_send_touch ->
 * the engine -> JS:
 *
 *   two down          -> magenta   both pointers delivered, distinct ids
 *   second one lifted -> green     POINTER_UP removed exactly one pointer
 *   first one lifted  -> blue      the last UP ended the gesture
 *
 * The middle step is the one a single-pointer host cannot check: a host that
 * marks the wrong pointer as changed on POINTER_UP leaves two in `e.touches`
 * (still magenta) or none (blue).
 *
 * The result is reported through `am instrument`'s status bundle; the harness
 * (scripts/verify-android-c-host-multitouch.sh) reads INSTRUMENTATION_CODE.
 */
public final class MultiTouchInstrumentation extends Instrumentation {
    private static final String HOST_PACKAGE = "com.migo.chost";
    private static final int RED = 0xc00000;
    private static final int GREEN = 0x00c000;
    private static final int MAGENTA = 0xc000c0;
    private static final int BLUE = 0x0000c0;

    /** The probe's first paint follows engine start and content load. */
    private static final long READY_TIMEOUT_MS = 30_000;
    /** A colour change is one touch event plus one frame; generous for a slow device. */
    private static final long SETTLE_TIMEOUT_MS = 5_000;

    private final StringBuilder log = new StringBuilder();

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    @Override
    public void onStart() {
        super.onStart();
        Bundle result = new Bundle();
        int code;
        try {
            run();
            code = Activity.RESULT_OK;
            note("PASS");
        } catch (Throwable failure) {
            code = Activity.RESULT_CANCELED;
            note("FAIL: " + failure.getMessage());
        }
        result.putString(Instrumentation.REPORT_KEY_STREAMRESULT, log.toString());
        finish(code, result);
    }

    private void run() throws Exception {
        // The host runs in its own process: it has no code for an
        // instrumentation to live in (see this module's build.gradle).
        Intent launch = new Intent(Intent.ACTION_MAIN);
        launch.setClassName(HOST_PACKAGE, "android.app.NativeActivity");
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
        getContext().startActivity(launch);

        UiAutomation automation = getUiAutomation();
        awaitColour(automation, RED, READY_TIMEOUT_MS, "touch-probe's first paint");

        // Screen coordinates: injected events and screenshots both address the
        // display, and the probe paints every pixel of its window, so the
        // centre row is inside it whatever insets the device draws.
        Bitmap frame = automation.takeScreenshot();
        int width = frame.getWidth();
        int height = frame.getHeight();
        frame.recycle();
        float y = height * 0.5f;
        float x0 = width * 0.35f;
        float x1 = width * 0.65f;

        long down = SystemClock.uptimeMillis();
        inject(automation, down, MotionEvent.ACTION_DOWN, new float[] {x0}, new float[] {y});
        inject(automation, down, pointerAction(MotionEvent.ACTION_POINTER_DOWN, 1),
                new float[] {x0, x1}, new float[] {y, y});
        inject(automation, down, MotionEvent.ACTION_MOVE,
                new float[] {x0 + 4, x1 - 4}, new float[] {y + 4, y + 4});
        awaitColour(automation, MAGENTA, SETTLE_TIMEOUT_MS, "two pointers held");

        inject(automation, down, pointerAction(MotionEvent.ACTION_POINTER_UP, 1),
                new float[] {x0 + 4, x1 - 4}, new float[] {y + 4, y + 4});
        awaitColour(automation, GREEN, SETTLE_TIMEOUT_MS, "the second pointer lifted");

        inject(automation, down, MotionEvent.ACTION_UP, new float[] {x0 + 4}, new float[] {y + 4});
        awaitColour(automation, BLUE, SETTLE_TIMEOUT_MS, "every pointer lifted");
    }

    private static int pointerAction(int action, int index) {
        return action | (index << MotionEvent.ACTION_POINTER_INDEX_SHIFT);
    }

    private void inject(UiAutomation automation, long downTime, int action, float[] xs, float[] ys) {
        int count = xs.length;
        MotionEvent.PointerProperties[] properties = new MotionEvent.PointerProperties[count];
        MotionEvent.PointerCoords[] coords = new MotionEvent.PointerCoords[count];
        for (int i = 0; i < count; i++) {
            properties[i] = new MotionEvent.PointerProperties();
            properties[i].id = i;
            properties[i].toolType = MotionEvent.TOOL_TYPE_FINGER;
            coords[i] = new MotionEvent.PointerCoords();
            coords[i].x = xs[i];
            coords[i].y = ys[i];
            coords[i].pressure = 1f;
            coords[i].size = 1f;
        }
        MotionEvent event = MotionEvent.obtain(downTime, SystemClock.uptimeMillis(), action, count,
                properties, coords, 0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0);
        try {
            if (!automation.injectInputEvent(event, true)) {
                throw new IllegalStateException("injectInputEvent refused action " + action);
            }
        } finally {
            event.recycle();
        }
        note("injected action=" + (action & MotionEvent.ACTION_MASK)
                + " index=" + (action >> MotionEvent.ACTION_POINTER_INDEX_SHIFT)
                + " pointers=" + count);
    }

    /** Polls the screen centre until it shows {@code expected}, or fails naming what it saw. */
    private void awaitColour(UiAutomation automation, int expected, long timeoutMs, String what) {
        long deadline = SystemClock.uptimeMillis() + timeoutMs;
        int seen = -1;
        while (SystemClock.uptimeMillis() < deadline) {
            Bitmap shot = automation.takeScreenshot();
            if (shot != null) {
                seen = shot.getPixel(shot.getWidth() / 2, shot.getHeight() / 2) & 0xffffff;
                shot.recycle();
                if (close(seen, expected)) {
                    note(what + ": #" + hex(seen));
                    return;
                }
            }
            SystemClock.sleep(50);
        }
        throw new AssertionError(what + ": expected #" + hex(expected) + ", screen shows #" + hex(seen));
    }

    /** Screenshots go through the compositor; allow for its rounding, not for a different colour. */
    private static boolean close(int a, int b) {
        for (int shift = 0; shift <= 16; shift += 8) {
            if (Math.abs(((a >> shift) & 0xff) - ((b >> shift) & 0xff)) > 8) return false;
        }
        return true;
    }

    private static String hex(int rgb) {
        return rgb < 0 ? "none" : String.format("%06x", rgb);
    }

    private void note(String line) {
        log.append("[multitouch] ").append(line).append('\n');
    }
}
