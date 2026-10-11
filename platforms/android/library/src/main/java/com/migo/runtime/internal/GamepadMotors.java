package com.migo.runtime.internal;

import android.annotation.TargetApi;
import android.os.Build;
import android.os.CombinedVibration;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.view.InputDevice;

/**
 * A gamepad's motors, as {@code vibrationActuator} drives them: which effects
 * they play, and one instruction at a time -- these magnitudes for this long,
 * replacing whatever the pad was playing. The engine times every effect, so
 * nothing here keeps state.
 *
 * <p>Android exposes an input device's force feedback as vibrators. From API 31
 * a pad's {@link VibratorManager} lists them in the order of its rumble
 * channels -- the strong (low-frequency) motor first, then the weak one -- and
 * a pad with one motor drives it from the larger magnitude. Before 31 the pad
 * has one {@link Vibrator} for all its motors. Neither has trigger motors.
 *
 * @hide
 */
final class GamepadMotors {

    /** {@code MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE}. */
    static final int DUAL_RUMBLE = 1;

    private GamepadMotors() {}

    /** The effects the device's motors play: dual rumble when it has any. */
    static int haptics(InputDevice device) {
        try {
            if (Build.VERSION.SDK_INT >= 31) {
                return device.getVibratorManager().getVibratorIds().length > 0 ? DUAL_RUMBLE : 0;
            }
            return device.getVibrator().hasVibrator() ? DUAL_RUMBLE : 0;
        } catch (RuntimeException unavailable) {
            return 0;
        }
    }

    /**
     * Run the motors at these magnitudes (0..1) for {@code durationMs}, or stop
     * them when it is 0. Main thread.
     */
    static void play(InputDevice device, float strong, float weak, int durationMs) {
        try {
            if (Build.VERSION.SDK_INT >= 31) {
                play(device.getVibratorManager(), strong, weak, durationMs);
            } else {
                play(device.getVibrator(), Math.max(strong, weak), durationMs);
            }
        } catch (RuntimeException refused) {
            // A pad unplugged between the engine's instruction and this, or a
            // vibrator service that refused: the effect is lost, as it would be
            // on the device itself.
            android.util.Log.w("MigoGamepad", "rumble refused: " + refused);
        }
    }

    @TargetApi(31)
    private static void play(VibratorManager manager, float strong, float weak, int durationMs) {
        int[] ids = manager.getVibratorIds();
        if (ids.length == 0) return;
        if (ids.length == 1) {
            VibrationEffect effect = effect(manager.getVibrator(ids[0]), Math.max(strong, weak),
                    durationMs);
            if (effect == null) {
                manager.cancel();
            } else {
                manager.vibrate(CombinedVibration.createParallel(effect));
            }
            return;
        }
        // A motor left out of the combination is still, so a silent channel is
        // simply not added -- Android has no zero-amplitude one-shot.
        VibrationEffect strongEffect = effect(manager.getVibrator(ids[0]), strong, durationMs);
        VibrationEffect weakEffect = effect(manager.getVibrator(ids[1]), weak, durationMs);
        if (strongEffect == null && weakEffect == null) {
            manager.cancel();
            return;
        }
        CombinedVibration.ParallelCombination combination = CombinedVibration.startParallel();
        if (strongEffect != null) combination.addVibrator(ids[0], strongEffect);
        if (weakEffect != null) combination.addVibrator(ids[1], weakEffect);
        manager.vibrate(combination.combine());
    }

    private static void play(Vibrator vibrator, float magnitude, int durationMs) {
        if (!vibrator.hasVibrator()) return;
        VibrationEffect effect = effect(vibrator, magnitude, durationMs);
        if (effect == null) {
            vibrator.cancel();
        } else {
            vibrator.vibrate(effect);
        }
    }

    /** One motor's effect, or null for one that should be still. */
    private static VibrationEffect effect(Vibrator vibrator, float magnitude, int durationMs) {
        int amplitude = amplitude(magnitude, vibrator.hasAmplitudeControl());
        return durationMs > 0 && amplitude != 0
                ? VibrationEffect.createOneShot(durationMs, amplitude)
                : null;
    }

    /**
     * A 0..1 magnitude as Android's amplitude: 0 is still, anything above runs
     * the motor at 1..255 -- or, for a motor with no amplitude control, at its
     * one strength.
     */
    static int amplitude(float magnitude, boolean amplitudeControl) {
        if (!(magnitude > 0f)) return 0;
        if (!amplitudeControl) return VibrationEffect.DEFAULT_AMPLITUDE;
        return Math.max(1, Math.min(255, Math.round(magnitude * 255f)));
    }
}
