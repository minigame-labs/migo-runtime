package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import android.os.VibrationEffect;

import org.junit.Test;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * A gamepad's motors: the effect bit this SDK announces is the header's, and a
 * magnitude becomes the amplitude Android's vibrators take.
 */
public final class GamepadMotorsTest {

    private static final String HEADER = "include/migo/input.h";

    /** The bit a pad's haptics carry across JNI is the one the engine reads. */
    @Test
    public void dual_rumble_is_the_header_s_bit() throws Exception {
        String header = new String(Files.readAllBytes(locate(HEADER)), StandardCharsets.UTF_8);
        Matcher bit = Pattern.compile(
                "#define MIGO_GAMEPAD_HAPTICS_DUAL_RUMBLE \\(1U << (\\d+)\\)").matcher(header);
        assertTrue("the header defines dual rumble", bit.find());
        assertEquals(1 << Integer.parseInt(bit.group(1)), GamepadMotors.DUAL_RUMBLE);
    }

    @Test
    public void a_magnitude_is_an_amplitude_and_zero_is_still() {
        assertEquals(0, GamepadMotors.amplitude(0f, true));
        assertEquals(0, GamepadMotors.amplitude(-0.5f, true));
        assertEquals(0, GamepadMotors.amplitude(Float.NaN, true));
        assertEquals(255, GamepadMotors.amplitude(1f, true));
        assertEquals(128, GamepadMotors.amplitude(0.5f, true));
        // The faintest magnitude still moves the motor: Android's 0 is still.
        assertEquals(1, GamepadMotors.amplitude(0.0001f, true));
    }

    /** A motor with no amplitude control runs at its one strength, or not at all. */
    @Test
    public void a_motor_without_amplitude_control_is_on_or_off() {
        assertEquals(VibrationEffect.DEFAULT_AMPLITUDE, GamepadMotors.amplitude(0.1f, false));
        assertEquals(VibrationEffect.DEFAULT_AMPLITUDE, GamepadMotors.amplitude(1f, false));
        assertEquals(0, GamepadMotors.amplitude(0f, false));
    }

    private static Path locate(String relative) {
        StringBuilder searched = new StringBuilder();
        Path directory = Paths.get("").toAbsolutePath();
        while (directory != null) {
            Path candidate = directory.resolve(relative);
            if (Files.isReadable(candidate)) {
                return candidate;
            }
            searched.append("\n  ").append(candidate);
            directory = directory.getParent();
        }
        throw new AssertionError(relative + " not found, searched:" + searched);
    }
}
