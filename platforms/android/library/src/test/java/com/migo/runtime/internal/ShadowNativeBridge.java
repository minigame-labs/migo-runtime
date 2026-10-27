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

    /** Event names by number, as AudioRecorderManager.EVENT_* numbers them. */
    private static final String[] EVENT_NAMES = {
        "start", "pause", "resume", "stop", "error", "interruptionBegin", "interruptionEnd",
    };

    @Implementation
    public static void onRecorderEvent(int sessionId, int event, String payloadJson) {
        EVENTS.add(new Event(EVENT_NAMES[event], payloadJson));
    }

    @Implementation
    public static void onRecorderFrameData(
            int sessionId, int recordingId, byte[] frameData, int frameLength,
            boolean isLastFrame) {
        EVENTS.add(new Event("frame", recordingId + ":" + frameLength + ":" + isLastFrame));
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
