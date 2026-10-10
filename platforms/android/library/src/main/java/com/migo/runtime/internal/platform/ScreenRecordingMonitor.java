package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.os.Build;
import android.view.WindowManager;

import com.migo.runtime.internal.NativeMethods;

import java.util.concurrent.ConcurrentHashMap;
import java.util.function.Consumer;

/**
 * Whether the game's window is being recorded, and the switch that keeps it out of
 * screenshots and recordings.
 *
 * <p>Recording is visible to an app from Android 15 (API 35), through
 * {@link WindowManager#addScreenRecordingCallback}; before that there is no API that
 * says, so the state is reported as not supported rather than guessed. Keeping the
 * window out of captures is {@code FLAG_SECURE}, available on every version.
 *
 * @hide
 */
public final class ScreenRecordingMonitor {

    /** {@link #currentState}: the window is being recorded. */
    public static final int RECORDED = 1;
    /** {@link #currentState}: it is not. */
    public static final int NOT_RECORDED = 0;
    /** {@link #currentState}: this Android version cannot tell. */
    public static final int UNSUPPORTED = -1;

    private static final class Observer {
        final WindowManager windowManager;
        final Consumer<Integer> callback;

        Observer(WindowManager windowManager, Consumer<Integer> callback) {
            this.windowManager = windowManager;
            this.callback = callback;
        }
    }

    private static final ConcurrentHashMap<Integer, Observer> sObservers = new ConcurrentHashMap<>();

    private ScreenRecordingMonitor() {}

    /** Whether {@code activity}'s window is being recorded now. */
    public static int currentState(Activity activity) {
        if (Build.VERSION.SDK_INT < 35) {
            return UNSUPPORTED;
        }
        WindowManager windowManager = activity.getWindowManager();
        // Registering answers the current state; the callback itself is not needed.
        Consumer<Integer> probe = state -> { };
        int state = windowManager.addScreenRecordingCallback(Runnable::run, probe);
        windowManager.removeScreenRecordingCallback(probe);
        return state == WindowManager.SCREEN_RECORDING_STATE_VISIBLE ? RECORDED : NOT_RECORDED;
    }

    /** Report each change in whether the session's window is recorded, until {@link #stop}. */
    public static boolean start(int sessionId, Activity activity) {
        if (Build.VERSION.SDK_INT < 35) {
            return false;
        }
        WindowManager windowManager = activity.getWindowManager();
        Consumer<Integer> callback = state -> NativeMethods.onScreenRecordingStateChanged(
                sessionId,
                state == WindowManager.SCREEN_RECORDING_STATE_VISIBLE
                        ? "{\"state\":\"on\"}"
                        : "{\"state\":\"off\"}");
        Observer previous = sObservers.put(sessionId, new Observer(windowManager, callback));
        if (previous != null) {
            previous.windowManager.removeScreenRecordingCallback(previous.callback);
        }
        windowManager.addScreenRecordingCallback(Runnable::run, callback);
        return true;
    }

    /** Stop reporting for the session. */
    public static void stop(int sessionId) {
        Observer observer = sObservers.remove(sessionId);
        if (observer != null && Build.VERSION.SDK_INT >= 35) {
            observer.windowManager.removeScreenRecordingCallback(observer.callback);
        }
    }

    /** Keep the window out of screenshots and recordings, or let it back in. */
    public static void setHiddenFromCapture(final Activity activity, final boolean hidden) {
        activity.runOnUiThread(() -> {
            if (hidden) {
                activity.getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
            } else {
                activity.getWindow().clearFlags(WindowManager.LayoutParams.FLAG_SECURE);
            }
        });
    }
}
