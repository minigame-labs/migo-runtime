package com.migo.runtime.internal.platform;

import android.app.ActivityManager;
import android.content.Context;
import android.os.Build;

import java.util.Locale;

/**
 * Device information utilities.
 * <p>
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class DeviceInfo {

    private DeviceInfo() {}

    // ==================== Basic Device Info ====================

    /**
     * Get device brand.
     *
     * @return Device brand (e.g., "Samsung", "Google")
     */
    public static String getBrand() {
        return safeString(Build.BRAND);
    }

    /**
     * Get device model.
     *
     * @return Device model (e.g., "Pixel 6", "SM-G990U")
     */
    public static String getModel() {
        String model = safeString(Build.MODEL);
        return model.isEmpty() ? "unknown" : model;
    }

    /**
     * Get Android version string.
     *
     * @return Version string (e.g., "13", "14")
     */
    public static String getAndroidVersion() {
        return Build.VERSION.RELEASE;
    }

    /**
     * Get Android SDK/API level.
     *
     * @return SDK version (e.g., 33, 34)
     */
    public static int getSdkVersion() {
        return Build.VERSION.SDK_INT;
    }

    /**
     * Get device manufacturer.
     *
     * @return Manufacturer name
     */
    public static String getManufacturer() {
        return safeString(Build.MANUFACTURER);
    }

    /**
     * Get device hardware name.
     *
     * @return Hardware name
     */
    public static String getHardware() {
        return safeString(Build.HARDWARE);
    }

    // ==================== CPU / ABI ====================

    /**
     * Get the primary supported ABI.
     *
     * @return Primary ABI (e.g., "arm64-v8a", "x86_64")
     */
    public static String getPrimaryAbi() {
        String[] abis = Build.SUPPORTED_ABIS;
        if (abis != null && abis.length > 0) {
            return abis[0];
        }
        return "";
    }

    /**
     * Get all supported ABIs.
     *
     * @return Array of supported ABIs
     */
    public static String[] getSupportedAbis() {
        return Build.SUPPORTED_ABIS;
    }

    /**
     * Check if device supports 64-bit.
     *
     * @return true if 64-bit supported
     */
    public static boolean is64Bit() {
        String abi = getPrimaryAbi();
        return abi.contains("64");
    }

    // ==================== Memory ====================

    /**
     * Get total device memory in megabytes.
     *
     * @param context The context
     * @return Total memory in MB
     */
    public static long getTotalMemoryMB(Context context) {
        if (context == null) return 0;
        
        try {
            ActivityManager am = (ActivityManager) context.getSystemService(Context.ACTIVITY_SERVICE);
            if (am != null) {
                ActivityManager.MemoryInfo mi = new ActivityManager.MemoryInfo();
                am.getMemoryInfo(mi);
                return mi.totalMem / (1024 * 1024);
            }
        } catch (Exception ignored) {
        }
        return 0;
    }

    /**
     * Get available device memory in megabytes.
     *
     * @param context The context
     * @return Available memory in MB
     */
    public static long getAvailableMemoryMB(Context context) {
        if (context == null) return 0;
        
        try {
            ActivityManager am = (ActivityManager) context.getSystemService(Context.ACTIVITY_SERVICE);
            if (am != null) {
                ActivityManager.MemoryInfo mi = new ActivityManager.MemoryInfo();
                am.getMemoryInfo(mi);
                return mi.availMem / (1024 * 1024);
            }
        } catch (Exception ignored) {
        }
        return 0;
    }

    /**
     * Check if device is in low memory condition.
     *
     * @param context The context
     * @return true if low memory
     */
    public static boolean isLowMemory(Context context) {
        if (context == null) return false;
        
        try {
            ActivityManager am = (ActivityManager) context.getSystemService(Context.ACTIVITY_SERVICE);
            if (am != null) {
                ActivityManager.MemoryInfo mi = new ActivityManager.MemoryInfo();
                am.getMemoryInfo(mi);
                return mi.lowMemory;
            }
        } catch (Exception ignored) {
        }
        return false;
    }

    // ==================== Locale ====================

    /**
     * Get device language code.
     *
     * @return Language code (e.g., "en", "zh")
     */
    public static String getLanguage() {
        return Locale.getDefault().getLanguage();
    }

    /**
     * Get device country code.
     *
     * @return Country code (e.g., "US", "CN")
     */
    public static String getCountry() {
        return Locale.getDefault().getCountry();
    }

    /**
     * Get device locale string.
     *
     * @return Locale string (e.g., "en_US", "zh_CN")
     */
    public static String getLocale() {
        Locale locale = Locale.getDefault();
        return locale.getLanguage() + "_" + locale.getCountry();
    }

    // ==================== CPU Type ====================

    /**
     * Get CPU type description.
     *
     * @return CPU type string, or "unknown" if not determinable
     */
    public static String getCpuType() {
        String hardware = safeString(Build.HARDWARE);
        return hardware.isEmpty() ? "unknown" : hardware;
    }

    // ==================== JSON Export ====================

    /**
     * Get device info as JSON string.
     * <p>
     * Format matches the expected native protocol.
     *
     * @param context The context (for memory info)
     * @return JSON string
     */
    public static String toJson(Context context) {
        StringBuilder sb = new StringBuilder(256);
        sb.append("{");

        String abi = escapeJson(getPrimaryAbi());
        sb.append("\"brand\":\"").append(escapeJson(getBrand())).append("\",");
        sb.append("\"model\":\"").append(escapeJson(getModel())).append("\",");
        sb.append("\"system\":\"Android ").append(getAndroidVersion()).append("\",");
        sb.append("\"platform\":\"android\",");
        sb.append("\"abi\":\"").append(abi).append("\",");
        sb.append("\"deviceAbi\":\"").append(abi).append("\",");
        sb.append("\"cpuType\":\"").append(escapeJson(getCpuType())).append("\"");
        if (context != null) {
            sb.append(",\"memorySize\":").append(getTotalMemoryMB(context));
        }
        // No benchmark level: content reads the app's own assessment of the
        // device with what it says about itself (HostAppInfo). The engine
        // measures none, and total memory is not a score.

        sb.append("}");
        return sb.toString();
    }

    // ==================== Helpers ====================

    private static String safeString(String s) {
        return s != null ? s : "";
    }

    private static String escapeJson(String s) {
        if (s == null) return "";
        return s.replace("\\", "\\\\")
                .replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t");
    }
}
