package com.migo.runtime.internal.platform;

/**
 * Admits a sensor's readings no closer together than its rate's period, by the readings' own timestamps.
 *
 * <p>A tenth of the period is slack, so a sensor that runs at the rate asked for, with the jitter every sensor has,
 * is not halved: at 50 Hz for "game" (20 ms) a reading 19.5 ms after the last is still taken. A sensor running faster
 * is thinned to about the rate. Not thread-safe: one listener, called on one sensor thread, owns one.
 */
final class SensorPacer {
    private final long minimumGapNanos;
    private boolean admittedAny;
    private long lastAdmittedNanos;

    SensorPacer(long periodNanos) {
        this.minimumGapNanos = periodNanos - periodNanos / 10;
    }

    /** Whether the reading taken at {@code timestampNanos} goes to content. */
    boolean admit(long timestampNanos) {
        if (admittedAny && timestampNanos - lastAdmittedNanos < minimumGapNanos) return false;
        admittedAny = true;
        lastAdmittedNanos = timestampNanos;
        return true;
    }
}
