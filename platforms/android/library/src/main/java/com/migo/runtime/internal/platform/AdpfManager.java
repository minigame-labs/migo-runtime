package com.migo.runtime.internal.platform;

import android.content.Context;
import android.os.Build;
import android.os.PowerManager;
import android.util.Log;

import com.migo.runtime.internal.NativeMethods;

/**
 * Monitors device thermal state via Android ADPF (API 29+).
 * Forwards thermal status changes to the native engine.
 *
 * Thermal status levels (from PowerManager):
 *   0 = THERMAL_STATUS_NONE
 *   1 = THERMAL_STATUS_LIGHT
 *   2 = THERMAL_STATUS_MODERATE
 *   3 = THERMAL_STATUS_SEVERE
 *   4 = THERMAL_STATUS_CRITICAL
 *   5 = THERMAL_STATUS_EMERGENCY
 *   6 = THERMAL_STATUS_SHUTDOWN
 */
public class AdpfManager {
    private static final String TAG = "AdpfManager";

    private final int sessionId;
    private PowerManager powerManager;
    private Object thermalListener;  // PowerManager.OnThermalStatusChangedListener (API 29+)
    private int lastThermalStatus = 0;

    public AdpfManager(int sessionId, Context context) {
        this.sessionId = sessionId;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {  // API 29
            this.powerManager = (PowerManager) context.getSystemService(Context.POWER_SERVICE);
        }
    }

    /**
     * Publish a listener before registration can synchronously race teardown.
     * Registration failure clears the published reference so a failed start
     * cannot leave an unowned listener behind.
     */
    static void publishThermalListenerBeforeRegister(
            Runnable publish, Runnable register, Runnable clear) {
        publish.run();
        try {
            register.run();
        } catch (RuntimeException failure) {
            clear.run();
            throw failure;
        }
    }

    /**
     * Start monitoring thermal status. Safe to call on any API level.
     */
    public void start() {
        if (thermalListener != null) return;  // already registered
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && powerManager != null) {
            try {
                PowerManager.OnThermalStatusChangedListener listener = status -> {
                    lastThermalStatus = status;
                    Log.d(TAG, "Thermal status changed: " + status);
                    NativeMethods.onThermalStatusChanged(sessionId, status);
                };
                publishThermalListenerBeforeRegister(
                        () -> thermalListener = listener,
                        () -> powerManager.addThermalStatusListener(listener),
                        () -> thermalListener = null);
                // Report initial status
                lastThermalStatus = powerManager.getCurrentThermalStatus();
                Log.d(TAG, "ADPF initialized, current thermal status: " + lastThermalStatus);
            } catch (Exception e) {
                Log.w(TAG, "Failed to register thermal listener: " + e.getMessage());
            }
        }
    }

    /**
     * Get current thermal status (0-6). Returns 0 if ADPF not available.
     */
    public int getThermalStatus() {
        return lastThermalStatus;
    }

    /**
     * Get thermal headroom forecast (API 30+). Returns -1 if unavailable.
     * Values 0.0-1.0 are normal, >1.0 means throttling imminent.
     */
    public float getThermalHeadroom() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && powerManager != null) {
            try {
                return powerManager.getThermalHeadroom(10);  // 10 second forecast
            } catch (Exception e) {
                return -1f;
            }
        }
        return -1f;
    }

    public void destroy() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && powerManager != null && thermalListener != null) {
            try {
                powerManager.removeThermalStatusListener(
                    (PowerManager.OnThermalStatusChangedListener) thermalListener);
                thermalListener = null;
            } catch (Exception e) {
                Log.w(TAG, "Failed to remove thermal listener: " + e.getMessage());
            }
        }
    }
}
