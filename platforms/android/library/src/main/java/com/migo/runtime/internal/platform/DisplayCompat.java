package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.Context;
import android.content.res.Configuration;
import android.content.pm.ActivityInfo;
import android.graphics.Point;
import android.os.Build;
import android.os.Looper;
import android.util.DisplayMetrics;
import android.view.Display;
import android.view.Surface;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowManager;
import android.view.WindowMetrics;

/**
 * Compatibility wrapper for display-related APIs.
 * <p>
 * Provides consistent APIs across Android API 26-36, handling deprecated
 * methods appropriately for each API level.
 *
 * @hide
 */
public final class DisplayCompat {

    private DisplayCompat() {}

    // ==================== Screen Dimensions ====================

    /**
     * Get real screen width in pixels.
     * <p>
     * Uses WindowMetrics on API 30+, getRealMetrics on older versions.
     *
     * @param activity The activity
     * @return Screen width in pixels
     */
    public static int getScreenWidth(Activity activity) {
        if (activity == null) return 0;
        
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Use WindowMetrics
            WindowMetrics metrics = activity.getWindowManager().getCurrentWindowMetrics();
            return metrics.getBounds().width();
        } else {
            // API 26-29: Use deprecated getRealMetrics
            DisplayMetrics dm = new DisplayMetrics();
            activity.getWindowManager().getDefaultDisplay().getRealMetrics(dm);
            return dm.widthPixels;
        }
    }

    /**
     * Get real screen height in pixels.
     * <p>
     * Uses WindowMetrics on API 30+, getRealMetrics on older versions.
     *
     * @param activity The activity
     * @return Screen height in pixels
     */
    public static int getScreenHeight(Activity activity) {
        if (activity == null) return 0;
        
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Use WindowMetrics
            WindowMetrics metrics = activity.getWindowManager().getCurrentWindowMetrics();
            return metrics.getBounds().height();
        } else {
            // API 26-29: Use deprecated getRealMetrics
            DisplayMetrics dm = new DisplayMetrics();
            activity.getWindowManager().getDefaultDisplay().getRealMetrics(dm);
            return dm.heightPixels;
        }
    }

    /**
     * Get display density (pixels per dp).
     *
     * @param context The context
     * @return Display density
     */
    public static float getDensity(Context context) {
        if (context == null) return 1.0f;
        return context.getResources().getDisplayMetrics().density;
    }

    /**
     * Get display DPI.
     *
     * @param context The context
     * @return Display DPI
     */
    public static int getDpi(Context context) {
        if (context == null) return 160;
        return context.getResources().getDisplayMetrics().densityDpi;
    }

    // ==================== Display Rotation ====================

    /**
     * Get current display rotation.
     * <p>
     * Uses Display.getRotation() which works on all API levels.
     *
     * @param activity The activity
     * @return Rotation constant (Surface.ROTATION_0, ROTATION_90, etc.)
     */
    public static int getRotation(Activity activity) {
        if (activity == null) return 0;
        
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Use context.getDisplay()
            Display display = activity.getDisplay();
            return display != null ? display.getRotation() : 0;
        } else {
            // API 26-29: Use deprecated getDefaultDisplay()
            return activity.getWindowManager().getDefaultDisplay().getRotation();
        }
    }

    /**
     * Check whether the activity is currently displayed in landscape.
     *
     * <p>Uses {@link Configuration#orientation} as the source of truth. That
     * field reflects the <em>activity's effective orientation</em> — i.e. it
     * picks up {@code setRequestedOrientation()} regardless of the device's
     * natural orientation. Using {@link Display#getRotation()} alone is wrong
     * on devices whose natural orientation is landscape (tablets, ChromeOS,
     * many emulators): there {@code rotation == ROTATION_0} even when the
     * activity is forced to landscape, and a phone-natural assumption
     * misclassifies the activity as portrait.
     *
     * <p>Display rotation is only consulted as a fallback when Configuration
     * reports {@code ORIENTATION_UNDEFINED}.
     *
     * @param activity The activity
     * @return true if the activity is in landscape (incl. landscapeReverse)
     */
    public static boolean isLandscape(Activity activity) {
        if (activity == null) return false;

        int orientation = activity.getResources().getConfiguration().orientation;
        if (orientation == Configuration.ORIENTATION_LANDSCAPE) return true;
        if (orientation == Configuration.ORIENTATION_PORTRAIT) return false;

        // ORIENTATION_UNDEFINED: best-effort fallback to the display rotation.
        int rotation = getRotation(activity);
        return rotation == Surface.ROTATION_90 || rotation == Surface.ROTATION_270;
    }

    /**
     * Map current activity/configuration state to device orientation event value.
     *
     * @param activity      The activity
     * @param configuration Current configuration (nullable)
     * @return "portrait", "landscape", or "landscapeReverse"
     */
    public static String mapDeviceOrientationValue(Activity activity, Configuration configuration) {
        if (activity == null) {
            return "portrait";
        }

        int orientation = configuration != null
                ? configuration.orientation
                : Configuration.ORIENTATION_UNDEFINED;

        if (orientation == Configuration.ORIENTATION_LANDSCAPE) {
            int rotation = getRotation(activity);
            if (rotation == Surface.ROTATION_270) {
                return "landscapeReverse";
            }
            return "landscape";
        }

        if (orientation == Configuration.ORIENTATION_PORTRAIT) {
            return "portrait";
        }

        int rotation = getRotation(activity);
        if (rotation == Surface.ROTATION_90) {
            return "landscape";
        }
        if (rotation == Surface.ROTATION_270) {
            return "landscapeReverse";
        }
        return "portrait";
    }

    // ==================== Status Bar ====================

    /**
     * Get status bar height.
     *
     * @param context The context
     * @return Status bar height in pixels
     */
    public static int getStatusBarHeight(Context context) {
        if (context == null) return 0;
        
        int resourceId = context.getResources().getIdentifier(
                "status_bar_height", "dimen", "android");
        if (resourceId > 0) {
            return context.getResources().getDimensionPixelSize(resourceId);
        }
        return 0;
    }

    /**
     * Get navigation bar height.
     *
     * @param context The context
     * @return Navigation bar height in pixels
     */
    public static int getNavigationBarHeight(Context context) {
        if (context == null) return 0;
        
        int resourceId = context.getResources().getIdentifier(
                "navigation_bar_height", "dimen", "android");
        if (resourceId > 0) {
            return context.getResources().getDimensionPixelSize(resourceId);
        }
        return 0;
    }

    // ==================== Safe Area / Insets ====================

    /**
     * Safe area insets container.
     */
    public static final class SafeAreaInsets {
        public final int left;
        public final int top;
        public final int right;
        public final int bottom;

        public SafeAreaInsets(int left, int top, int right, int bottom) {
            this.left = left;
            this.top = top;
            this.right = right;
            this.bottom = bottom;
        }

        public static SafeAreaInsets empty() {
            return new SafeAreaInsets(0, 0, 0, 0);
        }
    }

    /**
     * Get safe area insets (system bars + display cutout).
     * <p>
     * On API 26-27: Uses WindowInsets system window insets.
     * On API 28-29: Adds DisplayCutout safe insets.
     * On API 30+: Uses WindowInsets.Type for comprehensive insets.
     * Falls back to status-bar height only when root insets are unavailable.
     *
     * @param activity The activity
     * @return Safe area insets
     */
    public static SafeAreaInsets getSafeAreaInsets(Activity activity) {
        if (activity == null) {
            return SafeAreaInsets.empty();
        }

        View decorView = activity.getWindow().getDecorView();

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Modern WindowInsets API
            WindowInsets insets = decorView.getRootWindowInsets();
            if (insets != null) {
                android.graphics.Insets systemBars = insets.getInsets(
                        WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout()
                );
                return new SafeAreaInsets(
                        systemBars.left, systemBars.top,
                        systemBars.right, systemBars.bottom
                );
            }
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            // API 28-29: WindowInsets + DisplayCutout
            WindowInsets insets = decorView.getRootWindowInsets();
            if (insets != null) {
                int left = insets.getSystemWindowInsetLeft();
                int top = insets.getSystemWindowInsetTop();
                int right = insets.getSystemWindowInsetRight();
                int bottom = insets.getSystemWindowInsetBottom();

                android.view.DisplayCutout cutout = insets.getDisplayCutout();
                if (cutout != null) {
                    left = Math.max(left, cutout.getSafeInsetLeft());
                    top = Math.max(top, cutout.getSafeInsetTop());
                    right = Math.max(right, cutout.getSafeInsetRight());
                    bottom = Math.max(bottom, cutout.getSafeInsetBottom());
                }
                return new SafeAreaInsets(left, top, right, bottom);
            }
        } else {
            // API 26-27: Basic WindowInsets
            WindowInsets insets = decorView.getRootWindowInsets();
            if (insets != null) {
                return new SafeAreaInsets(
                        insets.getSystemWindowInsetLeft(),
                        insets.getSystemWindowInsetTop(),
                        insets.getSystemWindowInsetRight(),
                        insets.getSystemWindowInsetBottom()
                );
            }
        }

        // Fallback when root insets are temporarily unavailable.
        int statusBarHeight = getStatusBarHeight(activity);
        return new SafeAreaInsets(0, statusBarHeight, 0, 0);
    }

    // ==================== Full Screen / Immersive Mode ====================

    /**
     * Enter immersive full-screen mode.
     * <p>
     * Uses WindowInsetsController on API 30+, deprecated setSystemUiVisibility on older versions.
     *
     * @param activity The activity
     */
    public static void enterImmersiveMode(Activity activity) {
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) {
            return;
        }

        Window window = activity.getWindow();
        View decorView = window.getDecorView();

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Use WindowInsetsController
            window.setDecorFitsSystemWindows(false);
            android.view.WindowInsetsController controller = window.getInsetsController();
            if (controller != null) {
                controller.hide(WindowInsets.Type.statusBars() | WindowInsets.Type.navigationBars());
                controller.setSystemBarsBehavior(
                        android.view.WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                );
            }
        } else {
            // API 26-29: Use deprecated setSystemUiVisibility
            window.addFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN);
            
            int flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;

            decorView.setSystemUiVisibility(flags);

            // Re-apply flags when system UI visibility changes
            int finalFlags = flags;
            decorView.setOnSystemUiVisibilityChangeListener(visibility -> {
                if ((visibility & View.SYSTEM_UI_FLAG_FULLSCREEN) == 0) {
                    decorView.setSystemUiVisibility(finalFlags);
                }
            });
        }

        // Handle display cutout on Android P+
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            WindowManager.LayoutParams lp = window.getAttributes();
            lp.layoutInDisplayCutoutMode = 
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(lp);
        }
    }

    // ==================== Orientation ====================

    /**
     * Set device orientation (landscape or portrait).
     * <p>
     * Changes the activity's requested orientation. The change triggers
     * an orientation change event via onDeviceOrientationChange callback.
     *
     * @param activity The activity
     * @param value    "landscape" or "portrait"
     * @return 0 on success, -1 on failure, -2 if value is invalid
     */
    public static int setDeviceOrientation(final Activity activity, String value) {
        if (activity == null) return -1;
        if (value == null) return -2;

        final int orientation;
        switch (value) {
            case "landscape":
                orientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE;
                break;
            case "portrait":
                orientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT;
                break;
            default:
                return -2; // Invalid value
        }

        try {
            Runnable applyOrientation = new Runnable() {
                @Override
                public void run() {
                    try {
                        activity.setRequestedOrientation(orientation);
                    } catch (Exception ignored) {
                    }
                }
            };

            if (Looper.myLooper() == Looper.getMainLooper()) {
                applyOrientation.run();
            } else {
                activity.runOnUiThread(applyOrientation);
            }
            return 0;
        } catch (Exception e) {
            return -1;
        }
    }

    // ==================== Full Screen / Immersive Mode ====================

    /**
     * Exit immersive mode and show system bars.
     *
     * @param activity The activity
     */
    public static void exitImmersiveMode(Activity activity) {
        if (activity == null || activity.isFinishing() || activity.isDestroyed()) {
            return;
        }

        Window window = activity.getWindow();

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            // API 30+: Use WindowInsetsController
            window.setDecorFitsSystemWindows(true);
            android.view.WindowInsetsController controller = window.getInsetsController();
            if (controller != null) {
                controller.show(WindowInsets.Type.statusBars() | WindowInsets.Type.navigationBars());
            }
        } else {
            // API 26-29: Clear flags
            window.clearFlags(WindowManager.LayoutParams.FLAG_FULLSCREEN);
            View decorView = window.getDecorView();
            decorView.setSystemUiVisibility(View.SYSTEM_UI_FLAG_VISIBLE);
            decorView.setOnSystemUiVisibilityChangeListener(null);
        }
    }
}
