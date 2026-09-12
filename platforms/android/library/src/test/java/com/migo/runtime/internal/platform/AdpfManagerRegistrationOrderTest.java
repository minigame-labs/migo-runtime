package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.util.ArrayList;
import java.util.List;

import org.junit.Test;

public final class AdpfManagerRegistrationOrderTest {
    @Test
    public void listenerIsPublishedBeforeRegistrationCanRaceDestroy() {
        List<String> events = new ArrayList<>();
        final boolean[] published = {false};

        AdpfManager.publishThermalListenerBeforeRegister(
                () -> {
                    published[0] = true;
                    events.add("publish");
                },
                () -> {
                    assertTrue("destroy must be able to see the listener", published[0]);
                    events.add("register");
                },
                () -> events.add("clear"));

        assertEquals("publish", events.get(0));
        assertEquals("register", events.get(1));
    }

    @Test
    public void failedRegistrationClearsThePublishedListener() {
        final boolean[] cleared = {false};
        try {
            AdpfManager.publishThermalListenerBeforeRegister(
                    () -> {},
                    () -> { throw new IllegalStateException("registration failed"); },
                    () -> cleared[0] = true);
        } catch (IllegalStateException expected) {
            // Expected registration failure.
        }
        assertTrue(cleared[0]);
    }
}
