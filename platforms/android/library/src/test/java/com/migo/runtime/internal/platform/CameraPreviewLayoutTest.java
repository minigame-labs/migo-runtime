package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertEquals;

import android.util.Size;
import android.widget.FrameLayout;

import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;

/**
 * Where a camera's preview stands and how its picture fills it: the rectangle content gave in the game's logical
 * pixels, on screen; the picture covering it, upright, in proportion; and the picture asked of the camera.
 */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35)
public final class CameraPreviewLayoutTest {

    @Test
    public void the_rectangle_is_the_game_s_logical_pixels_from_the_game_view() {
        FrameLayout.LayoutParams frame = GameViewOverlay.frame(10, 20, 3f, 1.5f, 2f, 100.2f, 50f);
        assertEquals(15, frame.leftMargin);
        assertEquals(26, frame.topMargin);
        assertEquals(300, frame.width);
        assertEquals(150, frame.height);
    }

    /** Edges are rounded, not sizes: two previews content put side by side meet on screen. */
    @Test
    public void rectangles_that_meet_in_the_game_meet_on_screen() {
        FrameLayout.LayoutParams left = GameViewOverlay.frame(0, 0, 2.625f, 0f, 0f, 33.3f, 10f);
        FrameLayout.LayoutParams right = GameViewOverlay.frame(0, 0, 2.625f, 33.3f, 0f, 33.3f, 10f);
        assertEquals(right.leftMargin, left.leftMargin + left.width);
    }

    @Test
    public void a_rectangle_of_no_size_is_no_size() {
        FrameLayout.LayoutParams frame = GameViewOverlay.frame(0, 0, 3f, 5f, 5f, -4f, 0f);
        assertEquals(0, frame.width);
        assertEquals(0, frame.height);
    }

    /** A portrait picture in a portrait view of its proportions: shown as the view stretches it. */
    @Test
    public void a_picture_of_the_view_s_proportions_is_left_as_it_is() {
        float[] scale = CameraPreview.coverScale(900, 1600, 720, 1280, 0);
        assertEquals(1f, scale[0], 1e-6f);
        assertEquals(1f, scale[1], 1e-6f);
    }

    /** A tall picture in a square view covers it: as wide as the view, cropped above and below. */
    @Test
    public void a_picture_covers_the_view_and_is_cropped_where_it_does_not_fit() {
        float[] scale = CameraPreview.coverScale(600, 600, 720, 1280, 0);
        assertEquals(1f, scale[0], 1e-6f);
        assertEquals(1280f / 720f, scale[1], 1e-5f);
    }

    /**
     * A display turned a quarter: the portrait picture, once turned upright, is landscape, and covers a landscape
     * view exactly -- the scale undoes the view's stretch before the turn.
     */
    @Test
    public void a_turned_display_turns_the_picture_s_proportions() {
        float[] scale = CameraPreview.coverScale(1600, 900, 720, 1280, 90);
        assertEquals(900f, scale[0] * 1600f, 1e-3f);
        assertEquals(1600f, scale[1] * 900f, 1e-3f);
    }

    private static final Size[] OFFERED = {
            new Size(4032, 3024), new Size(1920, 1080), new Size(1440, 1080),
            new Size(1280, 720), new Size(640, 480), new Size(320, 240),
    };

    /** A full-screen portrait preview from a sensor turned a quarter: the 16:9 picture that fills it. */
    @Test
    public void the_picture_asked_for_fills_the_view_in_its_proportions() {
        assertEquals(new Size(1920, 1080), CameraPreview.chooseBufferSize(OFFERED, 1080, 1920, true));
    }

    /**
     * A landscape preview on a portrait phone whose sensor is turned a quarter: no landscape picture matches its
     * proportions, so the one that fills it and crops the least -- not the smallest, most nearly square one, which
     * would be enlarged into blocks.
     */
    @Test
    public void a_preview_no_picture_matches_gets_one_that_fills_it() {
        Size[] offered = {
                new Size(176, 144), new Size(320, 240), new Size(640, 480),
                new Size(1280, 720), new Size(1280, 960), new Size(1920, 1080),
        };
        assertEquals(new Size(1280, 960), CameraPreview.chooseBufferSize(offered, 525, 394, true));
    }

    /** A picture of the view's proportions that would be enlarged loses to one that fills the view. */
    @Test
    public void a_picture_that_fills_beats_one_of_the_right_proportions_enlarged() {
        Size[] offered = {new Size(480, 480), new Size(1280, 720)};
        assertEquals(new Size(1280, 720), CameraPreview.chooseBufferSize(offered, 600, 600, false));
    }

    /** Of the pictures that fill a small preview, the smallest: no more pixels than are shown. */
    @Test
    public void a_small_preview_asks_for_no_more_than_it_shows() {
        assertEquals(new Size(1280, 720), CameraPreview.chooseBufferSize(OFFERED, 300, 150, false));
        assertEquals(new Size(320, 240), CameraPreview.chooseBufferSize(OFFERED, 0, 0, false));
    }

    /** Nothing larger than a preview stream takes, even where a larger picture would be closer. */
    @Test
    public void no_picture_is_larger_than_a_preview_stream() {
        Size[] offered = {new Size(4032, 3024), new Size(1440, 1080), new Size(640, 480)};
        assertEquals(new Size(1440, 1080), CameraPreview.chooseBufferSize(offered, 3000, 2250, false));
    }
}
