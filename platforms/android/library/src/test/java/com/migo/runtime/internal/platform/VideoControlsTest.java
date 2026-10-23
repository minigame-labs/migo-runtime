package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import android.os.SystemClock;
import android.view.MotionEvent;
import android.view.View;

import org.json.JSONObject;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import android.app.Activity;

import org.robolectric.Robolectric;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;
import org.robolectric.shadows.ShadowLooper;

import java.util.ArrayList;
import java.util.List;

/**
 * A video's own controls as content's options ask them: what they show, and
 * what a player's tap, double tap and drag do to the player.
 */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class VideoControlsTest {

    /** A player whose state the test sets, and which notes what it is asked. */
    private static final class FakePlayer implements VideoControls.Player {
        boolean playing;
        double position;
        double duration = 100;
        boolean fullScreen;
        final List<String> asked = new ArrayList<>();

        @Override public boolean isPlaying() { return playing; }
        @Override public double position() { return position; }
        @Override public double duration() { return duration; }
        @Override public void togglePlay() { asked.add("toggle"); playing = !playing; }
        @Override public void seekTo(double seconds) { asked.add("seek " + Math.round(seconds)); }
        @Override public boolean isFullScreen() { return fullScreen; }
        @Override public void toggleFullScreen() { asked.add("full screen"); fullScreen = !fullScreen; }
    }

    private FakePlayer player;
    private VideoControls controls;
    private long now;

    @Before
    public void setUp() {
        player = new FakePlayer();
        // Attached, as over a playing video: its delayed hide runs on the window.
        Activity activity = Robolectric.buildActivity(Activity.class).setup().get();
        controls = new VideoControls(activity, player);
        activity.setContentView(controls);
        controls.measure(View.MeasureSpec.makeMeasureSpec(400, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(200, View.MeasureSpec.EXACTLY));
        controls.layout(0, 0, 400, 200);
        now = SystemClock.uptimeMillis();
    }

    private void configure(String json) throws Exception {
        controls.configure(new JSONObject(json));
    }

    private boolean touch(int action, float x, float y, long downTime) {
        MotionEvent event = MotionEvent.obtain(downTime, now, action, x, y, 0);
        try {
            return controls.dispatchTouchEvent(event);
        } finally {
            event.recycle();
        }
    }

    private void tap(float x, float y) {
        long down = now;
        touch(MotionEvent.ACTION_DOWN, x, y, down);
        now += 50;
        touch(MotionEvent.ACTION_UP, x, y, down);
        now += 50;
    }

    private View center() {
        for (int i = 0; i < controls.getChildCount(); i++) {
            View child = controls.getChildAt(i);
            if ("play".contentEquals(String.valueOf(child.getContentDescription()))) return child;
        }
        throw new AssertionError("no centre play button");
    }

    @Test
    public void the_centre_button_shows_while_not_playing_and_plays() throws Exception {
        configure("{}");
        assertEquals(View.VISIBLE, center().getVisibility());
        center().performClick();
        assertEquals(List.of("toggle"), player.asked);
        controls.refresh();
        assertEquals(View.GONE, center().getVisibility());
    }

    @Test
    public void the_centre_button_is_content_s_to_turn_off() throws Exception {
        configure("{\"showCenterPlayBtn\": false}");
        assertEquals(View.GONE, center().getVisibility());
    }

    /** The bar, as the view that shows full screen sits in it. */
    private View bar() {
        return find(controls, "full screen").getParent() instanceof View
                ? (View) find(controls, "full screen").getParent() : null;
    }

    private static View find(View view, String description) {
        if (description.contentEquals(String.valueOf(view.getContentDescription()))) return view;
        if (view instanceof android.view.ViewGroup) {
            android.view.ViewGroup group = (android.view.ViewGroup) view;
            for (int i = 0; i < group.getChildCount(); i++) {
                View found = find(group.getChildAt(i), description);
                if (found != null) return found;
            }
        }
        return null;
    }

    @Test
    public void a_tap_shows_the_bar_and_another_hides_it() throws Exception {
        configure("{}");
        assertFalse(controls.isBarShown());
        assertEquals("hidden until a tap", View.GONE, bar().getVisibility());
        tap(60, 60);
        assertTrue(controls.isBarShown());
        assertEquals(View.VISIBLE, bar().getVisibility());
        tap(60, 60);
        assertFalse(controls.isBarShown());
        assertEquals(View.GONE, bar().getVisibility());
    }

    @Test
    public void while_playing_the_bar_goes_once_the_player_is_left_alone() throws Exception {
        configure("{}");
        player.playing = true;
        tap(60, 60);
        assertTrue(controls.isBarShown());
        ShadowLooper.idleMainLooper(VideoControls.BAR_SHOWN_MS + 10, java.util.concurrent.TimeUnit.MILLISECONDS);
        assertFalse(controls.isBarShown());
    }

    @Test
    public void without_controls_a_tap_shows_no_bar() throws Exception {
        configure("{\"controls\": false}");
        tap(60, 60);
        assertFalse(controls.isBarShown());
    }

    @Test
    public void a_video_that_asks_for_nothing_leaves_its_touches_to_the_game() throws Exception {
        configure("{\"controls\": false, \"enableProgressGesture\": false, \"enablePlayGesture\": false,"
                + " \"showCenterPlayBtn\": false}");
        assertFalse(controls.takesTouches());
        assertFalse(touch(MotionEvent.ACTION_DOWN, 200, 100, now));
    }

    @Test
    public void a_double_tap_plays_where_content_enabled_it() throws Exception {
        configure("{\"enablePlayGesture\": true}");
        tap(60, 60);
        tap(60, 60);
        ShadowLooper.idleMainLooper();
        assertEquals(List.of("toggle"), player.asked);
    }

    @Test
    public void a_sideways_drag_seeks_across_the_whole_video() throws Exception {
        configure("{}");
        player.position = 10;
        long down = now;
        touch(MotionEvent.ACTION_DOWN, 100, 100, down);
        for (int x = 120; x <= 300; x += 20) {
            now += 16;
            touch(MotionEvent.ACTION_MOVE, x, 100, down);
        }
        now += 16;
        touch(MotionEvent.ACTION_UP, 300, 100, down);
        // Half the width is half of 100 s, from 10 s.
        assertEquals(List.of("seek 60"), player.asked);
    }

    @Test
    public void the_progress_gesture_is_content_s_to_turn_off() throws Exception {
        configure("{\"enableProgressGesture\": false}");
        long down = now;
        touch(MotionEvent.ACTION_DOWN, 100, 100, down);
        for (int x = 120; x <= 300; x += 20) {
            now += 16;
            touch(MotionEvent.ACTION_MOVE, x, 100, down);
        }
        touch(MotionEvent.ACTION_UP, 300, 100, down);
        assertTrue(player.asked.isEmpty());
    }

    @Test
    public void the_time_reads_as_a_clock() {
        assertEquals("0:00", VideoControls.clock(0));
        assertEquals("1:05", VideoControls.clock(65.9));
        assertEquals("1:02:03", VideoControls.clock(3723));
    }
}
