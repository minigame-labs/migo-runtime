package com.migo.runtime.internal.platform;

import android.bluetooth.BluetoothAdapter;
import android.content.Context;
import android.location.LocationManager;
import android.net.ConnectivityManager;
import android.net.NetworkInfo;
import android.os.Build;
import android.provider.Settings;

/**
 * System settings and state utilities.
 * <p>
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class SystemSettings {

    private SystemSettings() {}

    // ==================== Bluetooth ====================

    /**
     * Check if Bluetooth is enabled.
     *
     * @return true if Bluetooth is enabled
     */
    public static boolean isBluetoothEnabled() {
        try {
            BluetoothAdapter adapter = BluetoothAdapter.getDefaultAdapter();
            return adapter != null && adapter.isEnabled();
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Check if Bluetooth is supported.
     *
     * @return true if Bluetooth is supported
     */
    public static boolean isBluetoothSupported() {
        return BluetoothAdapter.getDefaultAdapter() != null;
    }

    // ==================== Location ====================

    /**
     * Check if location services are enabled.
     *
     * @param context The context
     * @return true if location is enabled
     */
    public static boolean isLocationEnabled(Context context) {
        if (context == null) return false;
        
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                // API 28+: Use LocationManager.isLocationEnabled()
                LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
                return lm != null && lm.isLocationEnabled();
            } else {
                // API 26-27: Check individual providers
                LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
                if (lm == null) return false;
                
                boolean gps = false;
                boolean network = false;
                
                try {
                    gps = lm.isProviderEnabled(LocationManager.GPS_PROVIDER);
                } catch (Exception ignored) {
                }
                
                try {
                    network = lm.isProviderEnabled(LocationManager.NETWORK_PROVIDER);
                } catch (Exception ignored) {
                }
                
                return gps || network;
            }
        } catch (Exception e) {
            return false;
        }
    }

    // ==================== WiFi ====================

    /**
     * Check if WiFi is enabled.
     *
     * @param context The context
     * @return true if WiFi is enabled
     */
    public static boolean isWifiEnabled(Context context) {
        if (context == null) return false;

        try {
            return Settings.Global.getInt(
                    context.getContentResolver(),
                    Settings.Global.WIFI_ON, 0
            ) != 0;
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Check if WiFi is connected.
     *
     * @param context The context
     * @return true if connected to WiFi
     */
    public static boolean isWifiConnected(Context context) {
        if (context == null) return false;
        
        try {
            ConnectivityManager cm = (ConnectivityManager) context
                    .getSystemService(Context.CONNECTIVITY_SERVICE);
            if (cm == null) return false;
            
            NetworkInfo ni = cm.getActiveNetworkInfo();
            return ni != null && ni.isConnected() && ni.getType() == ConnectivityManager.TYPE_WIFI;
        } catch (Exception e) {
            return false;
        }
    }

    // ==================== Network ====================

    /**
     * Check if network is available.
     *
     * @param context The context
     * @return true if network is available
     */
    public static boolean isNetworkAvailable(Context context) {
        if (context == null) return false;
        
        try {
            ConnectivityManager cm = (ConnectivityManager) context
                    .getSystemService(Context.CONNECTIVITY_SERVICE);
            if (cm == null) return false;
            
            NetworkInfo ni = cm.getActiveNetworkInfo();
            return ni != null && ni.isConnected();
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Check if connected via mobile data.
     *
     * @param context The context
     * @return true if connected via mobile
     */
    public static boolean isMobileConnected(Context context) {
        if (context == null) return false;
        
        try {
            ConnectivityManager cm = (ConnectivityManager) context
                    .getSystemService(Context.CONNECTIVITY_SERVICE);
            if (cm == null) return false;
            
            NetworkInfo ni = cm.getActiveNetworkInfo();
            return ni != null && ni.isConnected() && ni.getType() == ConnectivityManager.TYPE_MOBILE;
        } catch (Exception e) {
            return false;
        }
    }

    // ==================== Airplane Mode ====================

    /**
     * Check if airplane mode is enabled.
     *
     * @param context The context
     * @return true if airplane mode is on
     */
    public static boolean isAirplaneModeOn(Context context) {
        if (context == null) return false;
        
        try {
            return Settings.Global.getInt(
                    context.getContentResolver(),
                    Settings.Global.AIRPLANE_MODE_ON, 0
            ) != 0;
        } catch (Exception e) {
            return false;
        }
    }

    // ==================== Packed Byte Array ====================

    /**
     * Get system settings as a packed byte array.
     * <p>
     * Layout (4 bytes):
     * - [0]: bluetoothEnabled (0/1)
     * - [1]: locationEnabled (0/1)
     * - [2]: wifiEnabled (0/1)
     * - [3]: orientation (0=unknown, 1=portrait, 2=landscape)
     *
     * @param context The context
     * @param isLandscape Whether device is in landscape
     * @return Packed byte array
     */
    public static byte[] toBytes(Context context, boolean isLandscape) {
        byte[] result = new byte[4];
        
        result[0] = (byte) (isBluetoothEnabled() ? 1 : 0);
        result[1] = (byte) (isLocationEnabled(context) ? 1 : 0);
        result[2] = (byte) (isWifiEnabled(context) ? 1 : 0);
        result[3] = (byte) (isLandscape ? 2 : 1);
        
        return result;
    }
}
