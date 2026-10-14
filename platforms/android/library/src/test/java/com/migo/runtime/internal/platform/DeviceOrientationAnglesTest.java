package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertArrayEquals;

import org.junit.Test;

/**
 * The W3C DeviceOrientation angles a rotation matrix decomposes into: R = Rz(alpha) Rx(beta) Ry(gamma), in degrees,
 * alpha [0, 360), beta [-180, 180), gamma [-90, 90).
 */
public final class DeviceOrientationAnglesTest {

    private static final double EPSILON = 1e-4;

    private static double[] angles(float... r) {
        double[] out = new double[3];
        DeviceSensorManager.deviceOrientationAngles(r, out);
        return out;
    }

    private static float cos(double degrees) {
        return (float) Math.cos(Math.toRadians(degrees));
    }

    private static float sin(double degrees) {
        return (float) Math.sin(Math.toRadians(degrees));
    }

    @Test
    public void a_device_lying_flat_facing_north_is_all_zero() {
        assertArrayEquals(new double[] {0, 0, 0}, angles(1, 0, 0, 0, 1, 0, 0, 0, 1), EPSILON);
    }

    @Test
    public void each_axis_turns_its_own_angle() {
        // About Z: alpha.
        assertArrayEquals(new double[] {90, 0, 0},
                angles(cos(90), -sin(90), 0, sin(90), cos(90), 0, 0, 0, 1), EPSILON);
        // About X: beta, the top tilting up.
        assertArrayEquals(new double[] {0, 30, 0},
                angles(1, 0, 0, 0, cos(30), -sin(30), 0, sin(30), cos(30)), EPSILON);
        // About Y: gamma.
        assertArrayEquals(new double[] {0, 0, 30},
                angles(cos(30), 0, sin(30), 0, 1, 0, -sin(30), 0, cos(30)), EPSILON);
    }

    /**
     * Face down is a half turn about X: beta reaches the end of its range and gamma stays in [-90, 90). Read through
     * Android's getOrientation, as before, roll took gamma past -90 (a device face down reported -147).
     */
    @Test
    public void face_down_is_beta_at_the_end_of_its_range_and_gamma_stays_in_its_own() {
        assertArrayEquals(new double[] {0, -180, 0}, angles(1, 0, 0, 0, -1, 0, 0, 0, -1), EPSILON);
        double[] tilted = angles(cos(20), 0, sin(20), sin(160) * sin(20), cos(160), -sin(160) * cos(20),
                -cos(160) * sin(20), sin(160), cos(160) * cos(20));
        assertArrayEquals(new double[] {0, 160, 20}, tilted, EPSILON);
    }
}
