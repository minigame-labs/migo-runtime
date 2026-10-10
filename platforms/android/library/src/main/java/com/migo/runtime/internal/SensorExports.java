package com.migo.runtime.internal;

import android.app.Activity;
import android.content.Context;

import com.migo.runtime.internal.platform.DeviceSensorManager;
import com.migo.runtime.internal.platform.ScreenCaptureObserver;
import com.migo.runtime.internal.platform.ScreenRecordingMonitor;

import java.util.concurrent.ConcurrentHashMap;

/**
 * Domain class for per-session sensor and screen capture manager delegation.
 *
 * @hide
 */
public final class SensorExports {

    private SensorExports() {}

    private static final Object sSensorLock = new Object();
    private static final Object sCaptureLock = new Object();

    private static final ConcurrentHashMap<Integer, DeviceSensorManager> sSensorManagers =
            new ConcurrentHashMap<>();

    private static final ConcurrentHashMap<Integer, ScreenCaptureObserver> sCaptureObservers =
            new ConcurrentHashMap<>();

    private static void syncSensorLifecycle(int sessionId, DeviceSensorManager manager) {
        LifecycleStateSynchronizer.synchronize(
                manager,
                () -> NativeExports.isSessionResourceSuspended(sessionId),
                manager::setLifecycleSuspended);
    }

    /**
     * The sensor manager belonging to the runtime that is running now.
     *
     * <p>Not {@code sSensorManagers.get}: the cache is keyed by session, so a
     * restart would otherwise hand the replacement isolate the manager the
     * retired one built — still registered with {@link android.hardware.SensorManager},
     * still firing, and stamping a generation the engine drops at dispatch. The
     * game would call {@code startAccelerometer} and never receive a reading.
     */
    private static DeviceSensorManager liveSensorManager(int sessionId) {
        return RuntimeGenerationBoundary.liveEntry(
                sSensorManagers, sessionId, DeviceSensorManager::destroy);
    }

    private static ScreenCaptureObserver liveCaptureObserver(int sessionId) {
        return RuntimeGenerationBoundary.liveEntry(
                sCaptureObservers, sessionId, ScreenCaptureObserver::destroy);
    }

    private static DeviceSensorManager getOrCreateSensorManager(int sessionId) {
        DeviceSensorManager existing = liveSensorManager(sessionId);
        if (existing != null) {
            syncSensorLifecycle(sessionId, existing);
            return existing;
        }
        synchronized (sSensorLock) {
            existing = liveSensorManager(sessionId);
            if (existing != null) {
                syncSensorLifecycle(sessionId, existing);
                return existing;
            }
            if (NativeExports.isSessionTerminated(sessionId)) return null;
            RuntimeContext ctx = RuntimeRegistry.get(sessionId);
            if (ctx == null) return null;
            Activity activity = ctx.getActivity();
            if (activity == null) return null;
            boolean suspended = NativeExports.isSessionResourceSuspended(sessionId);
            DeviceSensorManager mgr = new DeviceSensorManager(sessionId, activity, suspended);
            sSensorManagers.put(sessionId, mgr);
            syncSensorLifecycle(sessionId, mgr);
            return mgr;
        }
    }

    /** Whether the sensor started (or will, once the session is in front); false when there is none. */
    public static boolean startDeviceMotionListening(int sessionId, String interval) {
        DeviceSensorManager mgr = getOrCreateSensorManager(sessionId);
        return mgr != null && mgr.startDeviceMotionListening(interval);
    }

    public static void stopDeviceMotionListening(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.stopDeviceMotionListening();
        }
    }

    /** Whether the sensor started (or will, once the session is in front); false when there is none. */
    public static boolean startGyroscope(int sessionId, String interval) {
        DeviceSensorManager mgr = getOrCreateSensorManager(sessionId);
        return mgr != null && mgr.startGyroscope(interval);
    }

    public static void stopGyroscope(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.stopGyroscope();
        }
    }

    /** Whether the sensor started (or will, once the session is in front); false when there is none. */
    public static boolean startCompass(int sessionId) {
        DeviceSensorManager mgr = getOrCreateSensorManager(sessionId);
        return mgr != null && mgr.startCompass();
    }

    public static void stopCompass(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.stopCompass();
        }
    }

    /** Whether the sensor started (or will, once the session is in front); false when there is none. */
    public static boolean startAccelerometer(int sessionId, String interval) {
        DeviceSensorManager mgr = getOrCreateSensorManager(sessionId);
        return mgr != null && mgr.startAccelerometer(interval);
    }

    public static void stopAccelerometer(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.stopAccelerometer();
        }
    }

    public static void destroySensorManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sSensorManagers,
                id -> id == sessionId,
                DeviceSensorManager::destroy);
    }

    public static void suspendPowerSensitiveManagers(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.suspendForLifecycle();
        }
    }

    public static void resumePowerSensitiveManagers(int sessionId) {
        DeviceSensorManager mgr = liveSensorManager(sessionId);
        if (mgr != null) {
            mgr.resumeForLifecycle();
        }
    }

    public static void startCaptureScreen(int sessionId) {
        ScreenCaptureObserver observer = liveCaptureObserver(sessionId);
        if (observer == null) {
            synchronized (sCaptureLock) {
                observer = liveCaptureObserver(sessionId);
                if (observer == null) {
                    RuntimeContext ctx = RuntimeRegistry.get(sessionId);
                    if (ctx == null) return;
                    Activity activity = ctx.getActivity();
                    Context appCtx = activity != null ? activity : AppContext.getOrNull();
                    if (appCtx == null) return;
                    observer = new ScreenCaptureObserver(sessionId, appCtx);
                    sCaptureObservers.put(sessionId, observer);
                }
            }
        }
        observer.start();
    }

    public static void stopCaptureScreen(int sessionId) {
        ScreenCaptureObserver observer = liveCaptureObserver(sessionId);
        if (observer != null) {
            observer.stop();
        }
    }

    public static void destroyCaptureObserver(int sessionId) {
        ResourceCleanup.destroyMatching(
                sCaptureObservers,
                id -> id == sessionId,
                ScreenCaptureObserver::destroy);
    }

    // ==================== Screen recording ====================

    private static Activity activityOf(int sessionId) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        return ctx != null ? ctx.getActivity() : null;
    }

    /** {@link ScreenRecordingMonitor#currentState}, or -2 when the session has no activity. */
    public static int getScreenRecordingState(int sessionId) {
        Activity activity = activityOf(sessionId);
        return activity != null ? ScreenRecordingMonitor.currentState(activity) : -2;
    }

    public static void startScreenRecordingObserver(int sessionId) {
        Activity activity = activityOf(sessionId);
        if (activity != null) {
            ScreenRecordingMonitor.start(sessionId, activity);
        }
    }

    public static void stopScreenRecordingObserver(int sessionId) {
        ScreenRecordingMonitor.stop(sessionId);
    }

    /** 0 once applied, -1 when the session has no activity. */
    public static int setVisualEffectOnCapture(int sessionId, boolean hidden) {
        Activity activity = activityOf(sessionId);
        if (activity == null) return -1;
        ScreenRecordingMonitor.setHiddenFromCapture(activity, hidden);
        return 0;
    }

    public static void destroyAll(int sessionId) {
        ResourceCleanup.runAll(
                () -> destroySensorManager(sessionId),
                () -> destroyCaptureObserver(sessionId),
                () -> stopScreenRecordingObserver(sessionId));
    }
}
