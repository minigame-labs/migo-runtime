package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/**
 * A sensor delivers at the rate content asked for, whatever rate the hardware runs at. On a Mate 30 the compass,
 * registered for five readings a second, answered fifty: the orientation sensor ran at the rate another listener kept
 * the hardware at (migo-conformance motion-at-rest-spec).
 */
public final class SensorPacerTest {
    private static int admittedOver(SensorRate rate, long sourcePeriodNanos, long durationNanos, long jitterNanos) {
        SensorPacer pacer = rate.pacer();
        int admitted = 0;
        long t = 1_000_000_000L;
        for (int i = 0; t < 1_000_000_000L + durationNanos; i++) {
            if (pacer.admit(t)) admitted++;
            t += sourcePeriodNanos + ((i & 1) == 0 ? jitterNanos : -jitterNanos);
        }
        return admitted;
    }

    @Test
    public void aSensorRunningFasterIsHeldToTheRateAsked() {
        // Fifty a second for 1.5 s: "normal" and the compass are about five a second, "ui" about sixteen.
        int normal = admittedOver(SensorRate.NORMAL, 20_000_000L, 1_500_000_000L, 0);
        int ui = admittedOver(SensorRate.UI, 20_000_000L, 1_500_000_000L, 0);
        assertTrue("normal admitted " + normal, normal >= 7 && normal <= 9);
        assertTrue("ui admitted " + ui, ui >= 20 && ui <= 26);
    }

    /** A sensor at the rate asked, with the jitter every sensor has, keeps every reading. */
    @Test
    public void aSensorAtTheRateAskedKeepsEveryReading() {
        assertEquals(75, admittedOver(SensorRate.GAME, 20_000_000L, 1_500_000_000L, 500_000L));
    }

    @Test
    public void theFirstReadingIsAlwaysTaken() {
        SensorPacer pacer = SensorRate.NORMAL.pacer();
        assertTrue(pacer.admit(5L));
    }

    @Test
    public void theIntervalNamesAreTheReferences() {
        assertEquals(SensorRate.GAME, SensorRate.of("game"));
        assertEquals(SensorRate.UI, SensorRate.of("ui"));
        assertEquals(SensorRate.NORMAL, SensorRate.of("normal"));
        assertEquals(SensorRate.NORMAL, SensorRate.of(null));
        assertEquals(20_000, SensorRate.GAME.samplingPeriodUs);
        assertEquals(60_000, SensorRate.UI.samplingPeriodUs);
        assertEquals(200_000, SensorRate.NORMAL.samplingPeriodUs);
    }
}
