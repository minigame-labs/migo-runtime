package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;

/**
 * A native view shown over the game where content puts it: a rectangle in the
 * game's own pixels -- the logical pixels its touches arrive in -- from the
 * game view's top left. A camera's preview is one.
 *
 * <p>The view goes in the activity's content, which draws above the game's
 * surface, and follows the game view when its layout changes (a rotation, a
 * resized window). Main thread only.
 *
 * @hide
 */
final class GameViewOverlay implements View.OnLayoutChangeListener {

    private final Activity activity;
    private final View gameView;
    private final View child;
    private final float density;
    private ViewGroup parent;
    private float x;
    private float y;
    private float width;
    private float height;

    GameViewOverlay(Activity activity, View gameView, View child) {
        this.activity = activity;
        this.gameView = gameView;
        this.child = child;
        this.density = DisplayCompat.getDensity(activity);
    }

    /** Show the view at {@code x}, {@code y}, {@code width} x {@code height}, or move it there. */
    void place(float x, float y, float width, float height) {
        this.x = x;
        this.y = y;
        this.width = width;
        this.height = height;
        if (parent == null) {
            ViewGroup content = activity.findViewById(android.R.id.content);
            if (content == null) return;
            parent = content;
            parent.addView(child, layout());
            gameView.addOnLayoutChangeListener(this);
        } else {
            child.setLayoutParams(layout());
        }
    }

    /** Take the view away. */
    void remove() {
        gameView.removeOnLayoutChangeListener(this);
        if (parent != null) parent.removeView(child);
        parent = null;
    }

    @Override
    public void onLayoutChange(View view, int left, int top, int right, int bottom,
                               int oldLeft, int oldTop, int oldRight, int oldBottom) {
        if (parent != null) child.setLayoutParams(layout());
    }

    private FrameLayout.LayoutParams layout() {
        int[] game = new int[2];
        int[] origin = new int[2];
        gameView.getLocationInWindow(game);
        parent.getLocationInWindow(origin);
        return frame(game[0] - origin[0], game[1] - origin[1], density, x, y, width, height);
    }

    /**
     * Where a rectangle content gave falls in a frame whose game view starts at
     * {@code offsetX}, {@code offsetY}: its edges are rounded, not its size, so
     * rectangles that meet in the game meet on screen.
     */
    static FrameLayout.LayoutParams frame(int offsetX, int offsetY, float density,
                                          float x, float y, float width, float height) {
        int left = offsetX + Math.round(x * density);
        int top = offsetY + Math.round(y * density);
        int right = offsetX + Math.round((x + Math.max(0f, width)) * density);
        int bottom = offsetY + Math.round((y + Math.max(0f, height)) * density);
        FrameLayout.LayoutParams params =
                new FrameLayout.LayoutParams(right - left, bottom - top, Gravity.TOP | Gravity.LEFT);
        params.leftMargin = left;
        params.topMargin = top;
        return params;
    }
}
