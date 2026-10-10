package com.migo.runtime.internal.platform;

import android.content.ContentResolver;
import android.content.Context;
import android.database.ContentObserver;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.MediaStore;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;

/**
 * Observes screenshot events.
 * <p>
 * From Android 14 (API 34) the system says so itself: an Activity's
 * {@code ScreenCaptureCallback} fires when the user captures it, needing only
 * the install-time {@code DETECT_SCREEN_CAPTURE} -- and without it a MediaStore
 * query sees no other app's screenshots from Android 13 on, so the older path
 * below would miss them silently. Before 14, and with no Activity to register
 * on, newly added MediaStore images are watched instead:
 * <p>
 * When the user takes a screenshot, Android writes the image to MediaStore.
 * This observer detects that by querying the actual file path/name of newly
 * added images and checking for screenshot-related keywords.
 * <p>
 * On Android 10+ (API 29+), no special permission is needed to query
 * MediaStore metadata (DISPLAY_NAME, RELATIVE_PATH). On older versions,
 * READ_EXTERNAL_STORAGE may be required but those devices are rare now.
 *
 * @hide
 */
public final class ScreenCaptureObserver implements RuntimeScoped {

    private static final String TAG = "ScreenCaptureObserver";
    private static final long DEBOUNCE_MS = 1000;

    private static final String[] PROJECTION;

    static {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            PROJECTION = new String[]{
                    MediaStore.Images.Media.DISPLAY_NAME,
                    MediaStore.Images.Media.RELATIVE_PATH,
                    MediaStore.Images.Media.DATE_ADDED,
            };
        } else {
            PROJECTION = new String[]{
                    MediaStore.Images.Media.DATA,
                    MediaStore.Images.Media.DATE_ADDED,
            };
        }
    }

    private final int sessionId;
    private final ContentResolver contentResolver;
    private final Handler handler;

    private final Context context;
    private ContentObserver observer;
    /** The Android 14 registration while it is active, else null. */
    private CaptureCallback captureCallback;
    private long lastNotifyTime = 0;


    /**
     * The runtime this observer belongs to.
     *
     * <p>Captured once, here, and stamped on every event it reports. A listener
     * registered by one isolate stays registered across a restart and keeps
     * firing; the stamp is what lets the engine tell those events from the
     * replacement runtime's own.
     */
    private final RuntimeGenerationBoundary.Token token;

    @Override
    public RuntimeGenerationBoundary.Token runtimeToken() {
        return token;
    }
    public ScreenCaptureObserver(int sessionId, Context context) {
        this.sessionId = sessionId;
        this.context = context;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.contentResolver = context.getContentResolver();
        this.handler = new Handler(Looper.getMainLooper());
    }

    /**
     * Start observing screenshot events.
     */
    public void start() {
        stop();

        if (Build.VERSION.SDK_INT >= 34 && context instanceof android.app.Activity) {
            captureCallback = new CaptureCallback((android.app.Activity) context,
                    () -> notifyCapture(System.currentTimeMillis()));
            return;
        }

        observer = new ContentObserver(handler) {
            @Override
            public void onChange(boolean selfChange, Uri uri) {
                if (uri == null) return;
                handleChange(uri);
            }
        };

        try {
            contentResolver.registerContentObserver(
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                    true,
                    observer
            );
        } catch (Exception e) {
            android.util.Log.w(TAG, "Failed to register observer", e);
        }
    }

    /**
     * Stop observing screenshot events.
     */
    public void stop() {
        if (captureCallback != null) {
            captureCallback.unregister();
            captureCallback = null;
        }
        if (observer != null) {
            try {
                contentResolver.unregisterContentObserver(observer);
            } catch (Exception ignored) {
            }
            observer = null;
        }
    }

    /**
     * Clean up resources.
     */
    public void destroy() {
        stop();
    }

    private void handleChange(Uri uri) {
        // Debounce: screenshots often trigger multiple onChange calls
        long now = System.currentTimeMillis();
        if (now - lastNotifyTime < DEBOUNCE_MS) {
            return;
        }

        // Quick check: if the URI string itself contains screenshot keywords, skip the query
        String uriStr = uri.toString().toLowerCase();
        if (matchesScreenshot(uriStr)) {
            notifyCapture(now);
            return;
        }

        // Query MediaStore for the actual file info
        if (isScreenshotUri(uri)) {
            notifyCapture(now);
        }
    }

    private void notifyCapture(long now) {
        lastNotifyTime = now;
        NativeMethods.onUserCaptureScreen(sessionId, token.generation());
    }

    private boolean isScreenshotUri(Uri uri) {
        Cursor cursor = null;
        try {
            // Query the most recent image. On API 26+ use Bundle-based query
            // arguments for proper LIMIT support; on older APIs use the LIMIT
            // trick appended to the sort order string.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Bundle queryArgs = new Bundle();
                queryArgs.putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER,
                        MediaStore.Images.Media.DATE_ADDED + " DESC");
                queryArgs.putInt(ContentResolver.QUERY_ARG_LIMIT, 1);
                cursor = contentResolver.query(
                        MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                        PROJECTION, queryArgs, null);
            } else {
                String sortOrder = MediaStore.Images.Media.DATE_ADDED + " DESC LIMIT 1";
                cursor = contentResolver.query(
                        MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                        PROJECTION, null, null, sortOrder);
            }
            if (cursor != null && cursor.moveToFirst()) {
                // Verify the image was added recently (within the last few seconds)
                int dateIdx = cursor.getColumnIndexOrThrow(MediaStore.Images.Media.DATE_ADDED);
                long dateAdded = cursor.getLong(dateIdx);
                long nowSeconds = System.currentTimeMillis() / 1000;
                if (Math.abs(nowSeconds - dateAdded) > 5) {
                    return false;
                }

                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    String displayName = cursor.getString(
                            cursor.getColumnIndexOrThrow(MediaStore.Images.Media.DISPLAY_NAME));
                    String relativePath = cursor.getString(
                            cursor.getColumnIndexOrThrow(MediaStore.Images.Media.RELATIVE_PATH));

                    // Check file name and path for screenshot keywords
                    if (displayName != null && matchesScreenshot(displayName.toLowerCase())) {
                        return true;
                    }
                    if (relativePath != null && matchesScreenshot(relativePath.toLowerCase())) {
                        return true;
                    }
                } else {
                    // API < 29: use DATA column (absolute file path)
                    @SuppressWarnings("deprecation")
                    String data = cursor.getString(
                            cursor.getColumnIndexOrThrow(MediaStore.Images.Media.DATA));
                    if (data != null && matchesScreenshot(data.toLowerCase())) {
                        return true;
                    }
                }
            }
        } catch (Exception e) {
            // SecurityException, IllegalArgumentException, etc.
            // Silently ignore - we can't determine if it's a screenshot
        } finally {
            if (cursor != null) {
                cursor.close();
            }
        }
        return false;
    }

    private static boolean matchesScreenshot(String s) {
        return s.contains("screenshot")
                || s.contains("screen_shot")
                || s.contains("screen-shot")
                || s.contains("screenshots")
                || s.contains("截屏")
                || s.contains("截图");
    }

    /** The Android 14 registration, apart so no older device resolves its types. */
    private static final class CaptureCallback {
        private final android.app.Activity activity;
        private final android.app.Activity.ScreenCaptureCallback callback;

        CaptureCallback(android.app.Activity activity, Runnable onCapture) {
            this.activity = activity;
            this.callback = onCapture::run;
            activity.registerScreenCaptureCallback(activity.getMainExecutor(), callback);
        }

        void unregister() {
            activity.unregisterScreenCaptureCallback(callback);
        }
    }
}
