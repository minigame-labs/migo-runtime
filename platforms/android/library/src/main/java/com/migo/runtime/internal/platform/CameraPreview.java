package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.Context;
import android.graphics.Matrix;
import android.graphics.SurfaceTexture;
import android.hardware.display.DisplayManager;
import android.os.Handler;
import android.os.Looper;
import android.util.Size;
import android.view.TextureView;
import android.view.View;

/**
 * A camera's preview on screen: what the camera sees, over the game, in the
 * rectangle content gave.
 *
 * <p>The camera draws into a SurfaceTexture its manager owns, made detached so
 * the camera runs whether or not the preview is drawn -- a camera content opened
 * for its frames alone, at no size or off screen, runs all the same -- and the
 * TextureView shows it while it is on screen. The picture keeps its proportions:
 * it covers the rectangle, cropped where it does not fit, turned upright for the
 * way the display is turned.
 *
 * <p>Main thread only.
 *
 * @hide
 */
final class CameraPreview implements TextureView.SurfaceTextureListener,
        View.OnLayoutChangeListener, DisplayManager.DisplayListener {

    /** The largest picture a preview stream takes: the PREVIEW size of the
     *  camera's guaranteed stream combinations. */
    private static final int MAX_LONG_SIDE = 1920;
    private static final int MAX_SHORT_SIDE = 1080;

    private final Activity activity;
    private final TextureView view;
    private final GameViewOverlay overlay;
    private final DisplayManager displays;
    private final Matrix transform = new Matrix();
    private int bufferWidth;
    private int bufferHeight;
    private int sensorOrientation;
    private boolean shown;

    CameraPreview(Activity activity, View gameView, SurfaceTexture texture) {
        this.activity = activity;
        this.view = new TextureView(activity);
        this.view.setSurfaceTexture(texture);
        this.view.setSurfaceTextureListener(this);
        this.view.addOnLayoutChangeListener(this);
        this.overlay = new GameViewOverlay(activity, gameView, view);
        this.displays = (DisplayManager) activity.getSystemService(Context.DISPLAY_SERVICE);
    }

    /** Show the preview at {@code x}, {@code y}, {@code width} x {@code height}, or move it there. */
    void place(float x, float y, float width, float height) {
        overlay.place(x, y, width, height);
        if (!shown && displays != null) {
            displays.registerDisplayListener(this, new Handler(Looper.getMainLooper()));
        }
        shown = true;
    }

    /** The picture the camera now draws: {@code width} x {@code height} as its sensor sees it. */
    void setBuffer(int width, int height, int sensorOrientation) {
        this.bufferWidth = width;
        this.bufferHeight = height;
        this.sensorOrientation = sensorOrientation;
        updateTransform();
    }

    /** Take the preview away. The texture stays its manager's. */
    void remove() {
        if (shown && displays != null) displays.unregisterDisplayListener(this);
        shown = false;
        overlay.remove();
    }

    private void updateTransform() {
        int viewWidth = view.getWidth();
        int viewHeight = view.getHeight();
        if (viewWidth == 0 || viewHeight == 0 || bufferWidth == 0 || bufferHeight == 0) return;
        boolean sideways = sensorOrientation % 180 != 0;
        int degrees = 90 * DisplayCompat.getRotation(activity);
        float[] scale = coverScale(viewWidth, viewHeight,
                sideways ? bufferHeight : bufferWidth,
                sideways ? bufferWidth : bufferHeight,
                degrees);
        float centreX = viewWidth / 2f;
        float centreY = viewHeight / 2f;
        transform.setScale(scale[0], scale[1], centreX, centreY);
        transform.postRotate(-degrees, centreX, centreY);
        view.setTransform(transform);
    }

    /**
     * The scale, about the view's centre, that makes a picture of
     * {@code naturalWidth} x {@code naturalHeight} -- upright for the device held
     * its natural way, which is how the camera hands it to the view -- cover the
     * view once turned upright on a display turned {@code displayDegrees}. The
     * view stretches the picture to its own size first; this undoes that, and
     * scales it until it covers.
     *
     * @return {scale x, scale y}
     */
    static float[] coverScale(int viewWidth, int viewHeight,
                              int naturalWidth, int naturalHeight, int displayDegrees) {
        boolean turned = displayDegrees % 180 != 0;
        float shownWidth = turned ? naturalHeight : naturalWidth;
        float shownHeight = turned ? naturalWidth : naturalHeight;
        float cover = Math.max(viewWidth / shownWidth, viewHeight / shownHeight);
        return new float[] {
                cover * naturalWidth / viewWidth,
                cover * naturalHeight / viewHeight,
        };
    }

    /**
     * The picture to ask the camera for, of the {@code sizes} it offers, for a
     * view of {@code viewWidth} x {@code viewHeight} pixels that shows it turned
     * a quarter when {@code sideways}: no larger than a preview stream takes;
     * one that fills the view without being enlarged, if any does -- of those,
     * the proportions closest to the view's, which crop the least of what the
     * camera sees, and then the fewest pixels; otherwise the one enlarged least.
     */
    static Size chooseBufferSize(Size[] sizes, int viewWidth, int viewHeight, boolean sideways) {
        if (sizes == null || sizes.length == 0) return new Size(1280, 720);
        int wantWidth = Math.max(1, sideways ? viewHeight : viewWidth);
        int wantHeight = Math.max(1, sideways ? viewWidth : viewHeight);
        double want = viewWidth > 0 && viewHeight > 0 ? (double) wantWidth / wantHeight : 4.0 / 3.0;
        Size best = null;
        for (Size size : sizes) {
            if (fits(size) && (best == null || better(size, best, wantWidth, wantHeight, want))) {
                best = size;
            }
        }
        return best != null ? best : sizes[0];
    }

    /** Whether {@code size} is a better picture than {@code best} for the view. */
    private static boolean better(Size size, Size best, int wantWidth, int wantHeight, double want) {
        double scale = Math.min((double) size.getWidth() / wantWidth, (double) size.getHeight() / wantHeight);
        double bestScale = Math.min((double) best.getWidth() / wantWidth, (double) best.getHeight() / wantHeight);
        boolean fills = scale >= 1;
        boolean bestFills = bestScale >= 1;
        if (fills != bestFills) return fills;
        if (!fills) return scale > bestScale;
        double error = aspectError(size, want);
        double bestError = aspectError(best, want);
        if (Math.abs(error - bestError) > 1e-3) return error < bestError;
        return (long) size.getWidth() * size.getHeight() < (long) best.getWidth() * best.getHeight();
    }

    private static boolean fits(Size size) {
        int longSide = Math.max(size.getWidth(), size.getHeight());
        int shortSide = Math.min(size.getWidth(), size.getHeight());
        return longSide <= MAX_LONG_SIDE && shortSide <= MAX_SHORT_SIDE;
    }

    private static double aspectError(Size size, double want) {
        return Math.abs(Math.log((double) size.getWidth() / size.getHeight() / want));
    }

    // The view follows the game; the picture follows the view and the display.

    @Override
    public void onLayoutChange(View v, int left, int top, int right, int bottom,
                               int oldLeft, int oldTop, int oldRight, int oldBottom) {
        updateTransform();
    }

    @Override
    public void onDisplayChanged(int displayId) {
        updateTransform();
    }

    @Override
    public void onDisplayAdded(int displayId) {}

    @Override
    public void onDisplayRemoved(int displayId) {}

    @Override
    public void onSurfaceTextureAvailable(SurfaceTexture surface, int width, int height) {}

    @Override
    public void onSurfaceTextureSizeChanged(SurfaceTexture surface, int width, int height) {
        updateTransform();
    }

    /** The texture is the camera's: the view lets go of it, and its manager releases it. */
    @Override
    public boolean onSurfaceTextureDestroyed(SurfaceTexture surface) {
        return false;
    }

    @Override
    public void onSurfaceTextureUpdated(SurfaceTexture surface) {}
}
