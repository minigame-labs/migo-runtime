package com.migo.runtime.internal.platform;

import android.content.Context;
import android.view.SurfaceView;

/**
 * The surface the built-in integrations show the game on. A view learns that it
 * gained or lost pointer capture only by overriding
 * {@link android.view.View#onPointerCaptureChange}, so the game's view is this
 * one, which passes the change on.
 *
 * @hide
 */
public final class GameSurfaceView extends SurfaceView {

    /** Told each time this view gains or loses pointer capture. */
    public interface PointerCaptureListener {
        void onPointerCaptureChanged(boolean hasCapture);
    }

    private PointerCaptureListener pointerCaptureListener;

    public GameSurfaceView(Context context) {
        super(context);
    }

    public void setPointerCaptureListener(PointerCaptureListener listener) {
        pointerCaptureListener = listener;
    }

    @Override
    public void onPointerCaptureChange(boolean hasCapture) {
        super.onPointerCaptureChange(hasCapture);
        PointerCaptureListener listener = pointerCaptureListener;
        if (listener != null) listener.onPointerCaptureChanged(hasCapture);
    }
}
