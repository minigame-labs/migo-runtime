package com.migo.runtime.internal;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;

import android.view.MotionEvent;

import org.junit.Test;

import java.util.ArrayList;
import java.util.List;

/**
 * A desktop-form device's mouse as the pointer stream: CSS pixels, DOM buttons,
 * presses from button actions only, captured motion accumulated, the wheel in
 * lines with the DOM's direction.
 */
public final class MouseEventHandlerTest {

    private static final int SESSION = 7;
    private static final float DENSITY = 2.0f;

    private static final class Recorded implements MouseEventHandler.PointerSink {
        final List<String> events = new ArrayList<>();

        @Override
        public boolean pointer(int sessionId, int kind, int button, float x, float y, double timeMs) {
            events.add("pointer " + kind + " " + button + " " + x + "," + y + " @" + (long) timeMs);
            return true;
        }

        @Override
        public boolean wheel(int sessionId, int deltaMode, double dx, double dy, double dz,
                             double timeMs) {
            events.add("wheel " + deltaMode + " " + dx + "," + dy);
            return true;
        }
    }

    private static MouseEventHandler.MouseEvent event(
            int action, int actionButton, int buttonState, float x, float y, float vscroll,
            float hscroll) {
        return new MouseEventHandler.MouseEvent() {
            @Override public int actionMasked() { return action; }
            @Override public int actionButton() { return actionButton; }
            @Override public int buttonState() { return buttonState; }
            @Override public long eventTime() { return 1000; }
            @Override public float x() { return x; }
            @Override public float y() { return y; }
            @Override public float axis(int axis) {
                return axis == MotionEvent.AXIS_VSCROLL ? vscroll
                        : axis == MotionEvent.AXIS_HSCROLL ? hscroll : 0f;
            }
        };
    }

    @Test
    public void hover_presses_and_drags_reach_the_pointer_stream_in_css_pixels() {
        Recorded sink = new Recorded();
        MouseEventHandler handler = new MouseEventHandler(DENSITY, sink);

        handler.dispatch(SESSION, event(MotionEvent.ACTION_HOVER_MOVE, 0, 0, 200, 100, 0, 0), false);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.BUTTON_SECONDARY, MotionEvent.BUTTON_SECONDARY, 200, 100, 0, 0), false);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_MOVE, 0,
                MotionEvent.BUTTON_SECONDARY, 220, 110, 0, 0), false);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.BUTTON_SECONDARY, 0, 220, 110, 0, 0), false);

        assertEquals(List.of(
                "pointer 1 0 100.0,50.0 @1000",
                "pointer 0 2 100.0,50.0 @1000",
                "pointer 1 2 110.0,55.0 @1000",
                "pointer 2 2 110.0,55.0 @1000"), sink.events);
    }

    @Test
    public void captured_motion_accumulates_onto_the_last_position() {
        Recorded sink = new Recorded();
        MouseEventHandler handler = new MouseEventHandler(DENSITY, sink);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_HOVER_MOVE, 0, 0, 100, 100, 0, 0), false);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_MOVE, 0, 0, 10, -4, 0, 0), true);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_MOVE, 0, 0, 10, -4, 0, 0), true);

        assertEquals("pointer 1 0 60.0,46.0 @1000", sink.events.get(2));
    }

    @Test
    public void the_wheel_scrolls_in_lines_the_way_the_dom_points() {
        Recorded sink = new Recorded();
        MouseEventHandler handler = new MouseEventHandler(DENSITY, sink);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_SCROLL, 0, 0, 0, 0, 1f, 0f), false);
        handler.dispatch(SESSION, event(MotionEvent.ACTION_SCROLL, 0, 0, 0, 0, 0f, -1f), false);

        assertEquals(List.of("wheel 1 0.0,-3.0", "wheel 1 -3.0,-0.0"), sink.events);
    }

    @Test
    public void a_button_the_dom_has_no_name_for_and_other_actions_are_not_taken() {
        Recorded sink = new Recorded();
        MouseEventHandler handler = new MouseEventHandler(DENSITY, sink);
        assertFalse(handler.dispatch(SESSION,
                event(MotionEvent.ACTION_BUTTON_PRESS, 1 << 20, 0, 0, 0, 0, 0), false));
        assertFalse(handler.dispatch(SESSION,
                event(MotionEvent.ACTION_HOVER_EXIT, 0, 0, 0, 0, 0, 0), false));
        assertEquals(0, sink.events.size());
        assertEquals(1, MouseEventHandler.domButton(MotionEvent.BUTTON_TERTIARY));
        assertEquals(3, MouseEventHandler.domButton(MotionEvent.BUTTON_BACK));
        assertEquals(4, MouseEventHandler.domButton(MotionEvent.BUTTON_FORWARD));
    }
}
