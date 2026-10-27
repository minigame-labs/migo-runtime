package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.app.NotificationManager;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;

/**
 * Permission utilities.
 * <p>
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class Permissions {

    private Permissions() {}

    // Common permission strings
    public static final String CAMERA = "android.permission.CAMERA";
    public static final String RECORD_AUDIO = "android.permission.RECORD_AUDIO";
    public static final String FINE_LOCATION = "android.permission.ACCESS_FINE_LOCATION";
    public static final String COARSE_LOCATION = "android.permission.ACCESS_COARSE_LOCATION";
    public static final String WRITE_STORAGE = "android.permission.WRITE_EXTERNAL_STORAGE";
    public static final String READ_STORAGE = "android.permission.READ_EXTERNAL_STORAGE";
    public static final String BLUETOOTH = "android.permission.BLUETOOTH";
    public static final String BLUETOOTH_ADMIN = "android.permission.BLUETOOTH_ADMIN";
    public static final String BLUETOOTH_CONNECT = "android.permission.BLUETOOTH_CONNECT";
    public static final String BLUETOOTH_SCAN = "android.permission.BLUETOOTH_SCAN";
    public static final String BLUETOOTH_ADVERTISE = "android.permission.BLUETOOTH_ADVERTISE";
    public static final String READ_CALENDAR = "android.permission.READ_CALENDAR";
    public static final String READ_MEDIA_IMAGES = "android.permission.READ_MEDIA_IMAGES";
    public static final String POST_NOTIFICATIONS = "android.permission.POST_NOTIFICATIONS";

    /**
     * Permission state.
     */
    public enum State {
        /** Permission granted */
        AUTHORIZED("authorized"),
        /** Permission denied */
        DENIED("denied"),
        /** Permission not determined (not requested yet) */
        NOT_DETERMINED("not determined");

        private final String value;

        State(String value) {
            this.value = value;
        }

        public String getValue() {
            return value;
        }
    }

    /**
     * Check if a permission is granted.
     *
     * @param context    The context
     * @param permission The permission to check
     * @return true if granted
     */
    public static boolean isGranted(Context context, String permission) {
        if (context == null || permission == null) return false;

        return context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED;
    }

    /**
     * Get permission state.
     *
     * @param activity   The activity
     * @param permission The permission to check
     * @return Permission state
     */
    public static State getState(Activity activity, String permission) {
        if (activity == null || permission == null) {
            return State.NOT_DETERMINED;
        }
        int result = activity.checkSelfPermission(permission);
        if (result == PackageManager.PERMISSION_GRANTED) {
            return State.AUTHORIZED;
        } else {
            // If shouldShowRequestPermissionRationale is true, it was denied.
            if (activity.shouldShowRequestPermissionRationale(permission)) {
                return State.DENIED;
            }
            return State.NOT_DETERMINED;
        }
    }

    /**
     * Check multiple permissions at once.
     *
     * @param context     The context
     * @param permissions The permissions to check
     * @return true if all granted
     */
    public static boolean areAllGranted(Context context, String... permissions) {
        if (context == null || permissions == null) return false;
        
        for (String permission : permissions) {
            if (!isGranted(context, permission)) {
                return false;
            }
        }
        return true;
    }

    /**
     * Get app authorization settings as JSON string.
     * @param context The context (Activity or application Context)
     * @return JSON string with permission states
     */
    public static String toJson(Context context) {
        StringBuilder sb = new StringBuilder(512);
        sb.append("{");

        sb.append("\"albumAuthorized\":\"").append(getAlbumAuth(context)).append("\",");
        sb.append("\"bluetoothAuthorized\":\"").append(getBluetoothAuth(context)).append("\",");
        sb.append("\"cameraAuthorized\":\"").append(getAuth(context, CAMERA)).append("\",");
        sb.append("\"locationAuthorized\":\"").append(getAuth(context, FINE_LOCATION)).append("\",");
        sb.append("\"locationReducedAccuracy\":").append(getLocationReducedAccuracy(context)).append(",");
        sb.append("\"microphoneAuthorized\":\"").append(getAuth(context, RECORD_AUDIO)).append("\",");

        String notifAuth = getNotificationAuth(context);
        sb.append("\"notificationAuthorized\":\"").append(notifAuth).append("\",");
        sb.append("\"notificationAlertAuthorized\":\"").append(notifAuth).append("\",");
        sb.append("\"notificationBadgeAuthorized\":\"").append(notifAuth).append("\",");
        sb.append("\"notificationSoundAuthorized\":\"").append(notifAuth).append("\",");
        sb.append("\"phoneCalendarAuthorized\":\"").append(getAuth(context, READ_CALENDAR)).append("\"");

        sb.append("}");
        return sb.toString();
    }

    // ==================== Authorization Helpers ====================

    private static String getAuth(Context context, String permission) {
        if (context == null) return "not determined";
        return context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED
                ? "authorized" : "denied";
    }

    private static String getBluetoothAuth(Context context) {
        if (context == null) return "not determined";
        if (Build.VERSION.SDK_INT >= 31) {
            return getAuth(context, BLUETOOTH_CONNECT);
        }
        // Pre-S: BLUETOOTH is a normal permission, always granted if declared
        return "authorized";
    }

    private static String getNotificationAuth(Context context) {
        if (context == null) return "not determined";
        if (Build.VERSION.SDK_INT >= 33) {
            return getAuth(context, POST_NOTIFICATIONS);
        }
        try {
            NotificationManager nm = (NotificationManager)
                    context.getSystemService(Context.NOTIFICATION_SERVICE);
            if (nm != null) {
                return nm.areNotificationsEnabled() ? "authorized" : "denied";
            }
        } catch (Exception ignored) {
        }
        return "authorized";
    }

    private static String getAlbumAuth(Context context) {
        if (context == null) return "not determined";
        if (Build.VERSION.SDK_INT >= 33) {
            return getAuth(context, READ_MEDIA_IMAGES);
        }
        return getAuth(context, READ_STORAGE);
    }

    private static boolean getLocationReducedAccuracy(Context context) {
        if (context == null) return false;
        boolean coarse = context.checkSelfPermission(COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED;
        boolean fine = context.checkSelfPermission(FINE_LOCATION) == PackageManager.PERMISSION_GRANTED;
        return coarse && !fine;
    }
}
