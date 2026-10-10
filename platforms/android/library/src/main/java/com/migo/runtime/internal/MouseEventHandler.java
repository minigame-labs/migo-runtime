package com.migo.runtime.internal;

import android.view.MotionEvent;

/**
 * The mouse of a desktop-form device -- a docked phone, a Chromebook, desktop
 * windowing -- reported as the pointer stream a C ABI host sends
 * ({@code MigoPointerEvent}, {@code MigoWheelEvent}): CSS pixels, DOM button
 * order, beside the touches Android makes of the same clicks.
 *
 * <p>Android delivers a mouse in three places, and this reads all of them:
 * hovering, button presses and the wheel arrive as generic motion; a drag with a
 * button held arrives as a touch move; and while the pointer is captured every
 * motion is relative. A press is {@code ACTION_BUTTON_PRESS} and nothing else, so
 * the {@code ACTION_DOWN} that precedes a primary press is not a second one.
 * Captured motion accumulates onto the last position, so content derives
 * {@code movementX}/{@code movementY} as it does from any host.
 *
 * <p>Main-thread confined, like {@link TouchEventHandler}.
 *
 * @hide
 */
public final class MouseEventHandler {

    /** Where the stream goes: {@code NativeMethods.onPointerEvent} / {@code onWheelEvent}. */
    interface PointerSink {
        boolean pointer(int sessionId, int kind, int button, float x, float y, double timeMs);

        boolean wheel(int sessionId, int deltaMode, double dx, double dy, double dz, double timeMs);
    }

    /** The parts of a {@link MotionEvent} this reads, so it is testable off a device. */
    interface MouseEvent {
        int actionMasked();
        int actionButton();
        int buttonState();
        long eventTime();
        float x();
        float y();
        float axis(int axis);
    }

    private static final class MotionMouseEvent implements MouseEvent {
        private MotionEvent event;

        void bind(MotionEvent event) {
            this.event = event;
        }

        void clear() {
            event = null;
        }

        @Override public int actionMasked() { return event.getActionMasked(); }
        @Override public int actionButton() { return event.getActionButton(); }
        @Override public int buttonState() { return event.getButtonState(); }
        @Override public long eventTime() { return event.getEventTime(); }
        @Override public float x() { return event.getX(); }
        @Override public float y() { return event.getY(); }
        @Override public float axis(int axis) { return event.getAxisValue(axis); }
    }

    static final int POINTER_DOWN = 0;
    static final int POINTER_MOVE = 1;
    static final int POINTER_UP = 2;
    /** DOM {@code WheelEvent.DOM_DELTA_LINE}. */
    static final int WHEEL_DELTA_LINE = 1;
    /** Lines a wheel notch scrolls, as browsers report a notch. */
    static final double LINES_PER_NOTCH = 3.0;

    private float inverseDensity = 1.0f;
    private float lastX;
    private float lastY;
    private final PointerSink sink;
    private final MotionMouseEvent motionMouseEvent = new MotionMouseEvent();

    public MouseEventHandler(float density) {
        this(density, new PointerSink() {
            @Override
            public boolean pointer(int sessionId, int kind, int button, float x, float y, double timeMs) {
                return NativeMethods.onPointerEvent(sessionId, kind, button, x, y, timeMs);
            }

            @Override
            public boolean wheel(int sessionId, int deltaMode, double dx, double dy, double dz,
                                 double timeMs) {
                return NativeMethods.onWheelEvent(sessionId, deltaMode, dx, dy, dz, timeMs);
            }
        });
    }

    MouseEventHandler(float density, PointerSink sink) {
        this.sink = sink;
        updateDensity(density);
    }

    /** The physical-to-CSS conversion, as {@link TouchEventHandler#updateDensity}. */
    public void updateDensity(float density) {
        final float validated = density > 0.0f && !Float.isNaN(density) && !Float.isInfinite(density)
                ? density
                : 1.0f;
        inverseDensity = 1.0f / validated;
    }

    /** A mouse event from wherever Android delivered it; {@code relative} while captured. */
    public boolean dispatch(int sessionId, MotionEvent event, boolean relative) {
        motionMouseEvent.bind(event);
        try {
            return dispatch(sessionId, motionMouseEvent, relative);
        } finally {
            motionMouseEvent.clear();
        }
    }

    boolean dispatch(int sessionId, MouseEvent event, boolean relative) {
        final double time = event.eventTime();
        switch (event.actionMasked()) {
            case MotionEvent.ACTION_HOVER_ENTER:
            case MotionEvent.ACTION_HOVER_MOVE:
            case MotionEvent.ACTION_MOVE:
                place(event, relative);
                return sink.pointer(sessionId, POINTER_MOVE, heldButton(event.buttonState()),
                        lastX, lastY, time);
            case MotionEvent.ACTION_BUTTON_PRESS:
            case MotionEvent.ACTION_BUTTON_RELEASE: {
                int button = domButton(event.actionButton());
                if (button < 0) return false;
                place(event, relative);
                int kind = event.actionMasked() == MotionEvent.ACTION_BUTTON_PRESS
                        ? POINTER_DOWN
                        : POINTER_UP;
                return sink.pointer(sessionId, kind, button, lastX, lastY, time);
            }
            case MotionEvent.ACTION_SCROLL: {
                // Android's vertical axis is positive away from the user, the DOM's
                // towards the content below.
                double dx = event.axis(MotionEvent.AXIS_HSCROLL) * LINES_PER_NOTCH;
                double dy = -event.axis(MotionEvent.AXIS_VSCROLL) * LINES_PER_NOTCH;
                if (dx == 0.0 && dy == 0.0) return false;
                return sink.wheel(sessionId, WHEEL_DELTA_LINE, dx, dy, 0.0, time);
            }
            default:
                return false;
        }
    }

    private void place(MouseEvent event, boolean relative) {
        if (relative) {
            lastX += event.x() * inverseDensity;
            lastY += event.y() * inverseDensity;
        } else {
            lastX = event.x() * inverseDensity;
            lastY = event.y() * inverseDensity;
        }
    }

    /** DOM {@code MouseEvent.button} for one Android button, or -1. */
    static int domButton(int androidButton) {
        switch (androidButton) {
            case MotionEvent.BUTTON_PRIMARY:
            case MotionEvent.BUTTON_STYLUS_PRIMARY:
                return 0;
            case MotionEvent.BUTTON_TERTIARY:
                return 1;
            case MotionEvent.BUTTON_SECONDARY:
            case MotionEvent.BUTTON_STYLUS_SECONDARY:
                return 2;
            case MotionEvent.BUTTON_BACK:
                return 3;
            case MotionEvent.BUTTON_FORWARD:
                return 4;
            default:
                return -1;
        }
    }

    /** The button a motion holds, primary first; 0 when none is held. */
    static int heldButton(int buttonState) {
        if ((buttonState & (MotionEvent.BUTTON_PRIMARY | MotionEvent.BUTTON_STYLUS_PRIMARY)) != 0) {
            return 0;
        }
        if ((buttonState & MotionEvent.BUTTON_TERTIARY) != 0) return 1;
        if ((buttonState & (MotionEvent.BUTTON_SECONDARY | MotionEvent.BUTTON_STYLUS_SECONDARY)) != 0) {
            return 2;
        }
        if ((buttonState & MotionEvent.BUTTON_BACK) != 0) return 3;
        if ((buttonState & MotionEvent.BUTTON_FORWARD) != 0) return 4;
        return 0;
    }
}
