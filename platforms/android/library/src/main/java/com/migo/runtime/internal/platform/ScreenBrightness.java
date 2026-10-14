package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.provider.Settings;
import android.view.Window;
import android.view.WindowManager;

/**
 * Screen brightness and keep-screen-on utilities.
 * <p>
 * Controls window-level brightness and FLAG_KEEP_SCREEN_ON.
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class ScreenBrightness {

    private ScreenBrightness() {}

    /** The largest value of {@link Settings.System#SCREEN_BRIGHTNESS}, the system's brightness setting. */
    private static final float SYSTEM_BRIGHTNESS_MAX = 255f;

    /**
     * The screen's brightness, 0.0-1.0: the game window's own where it set one, the system's setting where the window
     * follows the system (its override is {@code BRIGHTNESS_OVERRIDE_NONE}, -1 -- which is not a brightness). With
     * auto-brightness on, the setting is the last manual value, not the adaptive one of the moment, as the reference
     * documents for this call.
     *
     * @param activity The activity
     * @return Brightness 0.0-1.0, or {@link Float#NaN} when there is no window to ask
     */
    public static float getBrightness(Activity activity) {
        if (activity == null) return Float.NaN;
        try {
            Window window = activity.getWindow();
            if (window == null) return Float.NaN;
            float own = window.getAttributes().screenBrightness;
            if (own >= 0f) return Math.min(own, 1f);
            int system = Settings.System.getInt(activity.getContentResolver(), Settings.System.SCREEN_BRIGHTNESS);
            return Math.max(0f, Math.min(system / SYSTEM_BRIGHTNESS_MAX, 1f));
        } catch (Exception e) {
            return Float.NaN;
        }
    }

    /**
     * Set screen brightness.
     * <p>
     * Value range:
     * <ul>
     *   <li>0.0 - 1.0: Manual brightness (0 = darkest, 1 = brightest)</li>
     *   <li>-1: Follow system brightness (restore auto-brightness behavior)</li>
     * </ul>
     *
     * @param activity The activity
     * @param value    Brightness value (0.0-1.0) or -1 for system default
     * @return 0 on success, -1 on failure
     */
    public static int setBrightness(final Activity activity, float value) {
        if (activity == null) return -1;

        // Clamp value to valid range
        final float brightness;
        if (value < 0) {
            brightness = WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE; // -1
        } else {
            brightness = Math.max(0.01f, Math.min(value, 1.0f)); // Avoid 0 which turns off screen on some devices
        }

        try {
            activity.runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    try {
                        Window window = activity.getWindow();
                        if (window != null) {
                            WindowManager.LayoutParams params = window.getAttributes();
                            params.screenBrightness = brightness;
                            window.setAttributes(params);
                        }
                    } catch (Exception ignored) {
                    }
                }
            });
            return 0;
        } catch (Exception e) {
            return -1;
        }
    }

    /**
     * Set whether to keep screen on.
     * <p>
     * When enabled, the screen will not turn off while the activity is visible.
     * The setting is automatically cleared when the activity is destroyed.
     *
     * @param activity     The activity
     * @param keepScreenOn true to keep screen on, false to allow normal timeout
     * @return 0 on success, -1 on failure
     */
    public static int setKeepScreenOn(final Activity activity, final boolean keepScreenOn) {
        if (activity == null) return -1;

        try {
            activity.runOnUiThread(new Runnable() {
                @Override
                public void run() {
                    try {
                        Window window = activity.getWindow();
                        if (window != null) {
                            if (keepScreenOn) {
                                window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                            } else {
                                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
                            }
                        }
                    } catch (Exception ignored) {
                    }
                }
            });
            return 0;
        } catch (Exception e) {
            return -1;
        }
    }

}
