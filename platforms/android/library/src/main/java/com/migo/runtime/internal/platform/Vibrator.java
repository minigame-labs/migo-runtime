package com.migo.runtime.internal.platform;

import android.content.Context;
import android.os.Build;
import android.os.VibrationEffect;

/**
 * Vibration utilities.
 * <p>
 * Provides short and long vibration patterns compatible with Android API 26+.
 * Uses VibrationEffect on API 26+ for better haptic feedback control.
 *
 * @hide
 */
public final class Vibrator {

    /** Short vibration duration in milliseconds (spec: 15ms). */
    private static final long SHORT_DURATION_MS = 15;

    /** Long vibration duration in milliseconds (spec: 400ms). */
    private static final long LONG_DURATION_MS = 400;

    private Vibrator() {}

    /**
     * Trigger a short vibration (15ms).
     * <p>
     * On API 26+, uses VibrationEffect with intensity based on type parameter.
     * The SDK minimum API is 26, so the VibrationEffect path is used.
     *
     * @param context The context
     * @param type    Vibration type: "heavy", "medium", or "light"
     * @return 0 on success, -1 if vibrator unavailable, -2 if type not supported
     */
    public static int vibrateShort(Context context, String type) {
        if (context == null) return -1;

        android.os.Vibrator vibrator = getVibrator(context);
        if (vibrator == null || !vibrator.hasVibrator()) {
            return -1;
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            // API 26+: Use VibrationEffect with amplitude
            int amplitude = getAmplitudeForType(type);
            if (amplitude < 0) {
                return -2; // Type not supported
            }
            VibrationEffect effect = VibrationEffect.createOneShot(SHORT_DURATION_MS, amplitude);
            vibrator.vibrate(effect);
        } else {
            // Defensive fallback for API levels below 26; unreachable with this SDK minimum.
            vibrator.vibrate(SHORT_DURATION_MS);
        }

        return 0;
    }

    /**
     * Trigger a long vibration (400ms).
     *
     * @param context The context
     * @return 0 on success, -1 if vibrator unavailable
     */
    public static int vibrateLong(Context context) {
        if (context == null) return -1;

        android.os.Vibrator vibrator = getVibrator(context);
        if (vibrator == null || !vibrator.hasVibrator()) {
            return -1;
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            VibrationEffect effect = VibrationEffect.createOneShot(
                    LONG_DURATION_MS, VibrationEffect.DEFAULT_AMPLITUDE);
            vibrator.vibrate(effect);
        } else {
            vibrator.vibrate(LONG_DURATION_MS);
        }

        return 0;
    }

    /**
     * Get the system vibrator service.
     */
    private static android.os.Vibrator getVibrator(Context context) {
        try {
            return (android.os.Vibrator) context.getSystemService(Context.VIBRATOR_SERVICE);
        } catch (Exception e) {
            return null;
        }
    }

    /**
     * Map vibration type string to VibrationEffect amplitude.
     * <p>
     * Amplitude range is 1-255, with 255 being max intensity.
     *
     * @param type "heavy", "medium", or "light"
     * @return Amplitude value, or -1 if type is invalid
     */
    private static int getAmplitudeForType(String type) {
        if (type == null) return -1;
        switch (type) {
            case "heavy":
                return 255;  // Maximum intensity
            case "medium":
                return 150;  // ~60% intensity
            case "light":
                return 50;   // ~20% intensity
            default:
                return -1;   // Invalid type
        }
    }
}
