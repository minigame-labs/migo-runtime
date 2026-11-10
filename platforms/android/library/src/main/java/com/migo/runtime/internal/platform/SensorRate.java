package com.migo.runtime.internal.platform;

/**
 * A motion sensor's rate as content asks for it: the reference's {@code interval} -- "game" about every 20 ms, "ui"
 * about every 60 ms, "normal" about every 200 ms -- and the compass's five readings a second, which is "normal".
 *
 * <p>Android takes a sampling period as a hint, not a limit: a sensor already running faster for another listener --
 * a fused sensor such as orientation, fed by hardware another app or another sensor of this game keeps at its own
 * rate -- delivers at that rate. The compass answered fifty readings a second for five on a Mate 30. So the rate is
 * both what the sensor is registered at and what a {@link SensorPacer} holds delivery to.
 */
enum SensorRate {
    GAME(20),
    UI(60),
    NORMAL(200);

    /** The period to register the sensor at, in microseconds. */
    final int samplingPeriodUs;
    /** The period readings reach content at, in the nanoseconds of {@code SensorEvent.timestamp}. */
    final long periodNanos;

    SensorRate(int periodMillis) {
        this.samplingPeriodUs = periodMillis * 1000;
        this.periodNanos = periodMillis * 1_000_000L;
    }

    /** The reference's interval name; "normal", its default, for anything else. */
    static SensorRate of(String interval) {
        if ("game".equals(interval)) return GAME;
        if ("ui".equals(interval)) return UI;
        return NORMAL;
    }

    /** A pacer for one listener's readings at this rate. */
    SensorPacer pacer() {
        return new SensorPacer(periodNanos);
    }
}
