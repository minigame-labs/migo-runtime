package com.migo.runtime.internal.platform;

import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.BatteryManager;
import android.os.PowerManager;

/**
 * Battery information utilities.
 * <p>
 * Reads battery level, charging status, and low power mode from
 * Android system services. Compatible with API 26+.
 *
 * @hide
 */
public final class BatteryInfo {

    private BatteryInfo() {}

    /**
     * Get battery level as a percentage (1-100).
     *
     * @param context The context
     * @return Battery level percentage, or "0" if unavailable
     */
    public static String getLevel(Context context) {
        if (context == null) return "0";
        try {
            IntentFilter filter = new IntentFilter(Intent.ACTION_BATTERY_CHANGED);
            Intent batteryStatus = context.registerReceiver(null, filter);
            if (batteryStatus == null) return "0";

            int level = batteryStatus.getIntExtra(BatteryManager.EXTRA_LEVEL, -1);
            int scale = batteryStatus.getIntExtra(BatteryManager.EXTRA_SCALE, -1);
            if (level < 0 || scale <= 0) return "0";

            int pct = (int) (level * 100f / scale);
            return String.valueOf(Math.max(1, Math.min(pct, 100)));
        } catch (Exception e) {
            return "0";
        }
    }

    /**
     * Check if the device is currently charging.
     *
     * @param context The context
     * @return true if charging or full
     */
    public static boolean isCharging(Context context) {
        if (context == null) return false;
        try {
            IntentFilter filter = new IntentFilter(Intent.ACTION_BATTERY_CHANGED);
            Intent batteryStatus = context.registerReceiver(null, filter);
            if (batteryStatus == null) return false;

            int status = batteryStatus.getIntExtra(BatteryManager.EXTRA_STATUS, -1);
            return status == BatteryManager.BATTERY_STATUS_CHARGING
                    || status == BatteryManager.BATTERY_STATUS_FULL;
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Check if low power mode (battery saver) is enabled.
     *
     * @param context The context
     * @return true if low power mode is enabled
     */
    public static boolean isLowPowerModeEnabled(Context context) {
        if (context == null) return false;
        try {
            PowerManager pm = (PowerManager) context.getSystemService(Context.POWER_SERVICE);
            return pm != null && pm.isPowerSaveMode();
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Get battery info as JSON string.
     * <p>
     * Format: {@code {"level":"85","isCharging":true,"isLowPowerModeEnabled":false}}
     *
     * @param context The context
     * @return JSON string with battery info
     */
    public static String toJson(Context context) {
        String level = getLevel(context);
        boolean charging = isCharging(context);
        boolean lowPower = isLowPowerModeEnabled(context);

        return "{\"level\":\"" + level
                + "\",\"isCharging\":" + charging
                + ",\"isLowPowerModeEnabled\":" + lowPower + "}";
    }
}
