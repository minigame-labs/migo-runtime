package com.migo.runtime.internal;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;

/** Captures recorder JNI callbacks without loading the native runtime in host tests. */
@Implements(className = "com.migo.runtime.internal.NativeBridge")
public final class ShadowNativeBridge {
    private static final List<Event> EVENTS = Collections.synchronizedList(new ArrayList<>());

    static final class Event {
        final String type;
        final String payload;

        Event(String type, String payload) {
            this.type = type;
            this.payload = payload;
        }
    }

    @Implementation
    public static void onRecorderEvent(
            int sessionId, long generation, String eventType, String jsonPayload) {
        EVENTS.add(new Event(eventType, jsonPayload));
    }

    @Implementation
    public static void onRecorderFrameData(
            int sessionId, long generation, byte[] frameData, int frameLength,
            boolean isLastFrame) {
        EVENTS.add(new Event("frame", frameLength + ":" + isLastFrame));
    }

    static void reset() {
        EVENTS.clear();
    }

    static boolean hasEvent(String type) {
        synchronized (EVENTS) {
            for (Event event : EVENTS) {
                if (type.equals(event.type)) return true;
            }
            return false;
        }
    }

    static List<Event> events() {
        synchronized (EVENTS) {
            return new ArrayList<>(EVENTS);
        }
    }
}
