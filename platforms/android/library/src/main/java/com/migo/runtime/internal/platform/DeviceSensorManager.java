package com.migo.runtime.internal.platform;

import android.content.Context;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.view.Display;
import android.view.Surface;
import android.view.WindowManager;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;

/**
 * Manages device sensor listeners for motion, gyroscope, and orientation.
 * <p>
 * Uses Android SensorManager to listen for sensor events and dispatches
 * data to the native layer via {@link NativeMethods}.
 * <p>
 * Interval mapping from Mini Game spec:
 * <ul>
 *   <li>"game"   = ~20ms  (SENSOR_DELAY_GAME)</li>
 *   <li>"ui"     = ~60ms  (SENSOR_DELAY_UI)</li>
 *   <li>"normal" = ~200ms (SENSOR_DELAY_NORMAL)</li>
 * </ul>
 * <p>
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class DeviceSensorManager implements RuntimeScoped {

    private final int sessionId;
    private final SensorManager sensorManager;
    private final WindowManager windowManager;

    // Pre-allocated array for remapForDisplay() to avoid per-event allocation
    private final float[] remappedMatrix = new float[9];

    // Cached display rotation, updated periodically
    private int cachedRotation = Surface.ROTATION_0;
    private long lastRotationCheck = 0;
    private static final long ROTATION_CHECK_INTERVAL_MS = 500;

    private SensorEventListener motionListener;
    private SensorEventListener gyroscopeListener;
    private SensorEventListener compassListener;
    private SensorEventListener accelerometerListener;

    private final LifecycleRequestState<String> motionRequest;
    private final LifecycleRequestState<String> gyroscopeRequest;
    private final LifecycleRequestState<Boolean> compassRequest;
    private final LifecycleRequestState<String> accelerometerRequest;

    public DeviceSensorManager(int sessionId, Context context) {
        this(sessionId, context, false);
    }


    /**
     * The runtime this sensor listener belongs to.
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
    public DeviceSensorManager(int sessionId, Context context, boolean lifecycleSuspended) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.sensorManager = (SensorManager) context.getSystemService(Context.SENSOR_SERVICE);
        this.windowManager = (WindowManager) context.getSystemService(Context.WINDOW_SERVICE);
        this.motionRequest = new LifecycleRequestState<>(lifecycleSuspended);
        this.gyroscopeRequest = new LifecycleRequestState<>(lifecycleSuspended);
        this.compassRequest = new LifecycleRequestState<>(lifecycleSuspended);
        this.accelerometerRequest = new LifecycleRequestState<>(lifecycleSuspended);
    }

    // ==================== Device Motion ====================

    /**
     * Start listening for device motion (rotation vector) events.
     * <p>
     * Uses TYPE_ROTATION_VECTOR sensor and converts quaternion output to
     * Euler angles (alpha, beta, gamma) following the W3C DeviceOrientation spec.
     *
     * @param interval "game", "ui", or "normal"
     */
    public synchronized boolean startDeviceMotionListening(String interval) {
        LifecycleRequestState.Action action = motionRequest.requestStart(interval);
        if (action == LifecycleRequestState.Action.RESTART) {
            stopDeviceMotionListeningInternal();
        }
        if ((action == LifecycleRequestState.Action.START
                || action == LifecycleRequestState.Action.RESTART)
                && !startDeviceMotionListeningInternal(interval)) {
            motionRequest.startFailed(true);
            return false;
        }
        return true;
    }

    private boolean startDeviceMotionListeningInternal(String interval) {
        if (sensorManager == null) return false;

        Sensor sensor = sensorManager.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR);
        if (sensor == null) {
            // Fallback to game rotation vector (no magnetic field, less accurate but more available)
            sensor = sensorManager.getDefaultSensor(Sensor.TYPE_GAME_ROTATION_VECTOR);
        }
        if (sensor == null) return false;

        final int delay = parseInterval(interval);

        motionListener = new SensorEventListener() {
            private final float[] rotationMatrix = new float[9];
            private final float[] orientation = new float[3];

            @Override
            public void onSensorChanged(SensorEvent event) {
                synchronized (DeviceSensorManager.this) {
                    if (motionListener != this || !motionRequest.isActive()) return;
                    SensorManager.getRotationMatrixFromVector(rotationMatrix, event.values);

                    // Remap coordinate system based on display rotation for correct
                    // alpha/beta/gamma regardless of screen orientation
                    float[] remapped = remapForDisplay(rotationMatrix);

                    SensorManager.getOrientation(remapped, orientation);

                    // Convert from radians to degrees
                    // orientation[0] = azimuth (rotation around Z), -PI..PI -> alpha 0..360
                    // orientation[1] = pitch (rotation around X), -PI..PI -> beta -180..180
                    // orientation[2] = roll (rotation around Y), -PI/2..PI/2 -> gamma -90..90
                    double alpha = Math.toDegrees(orientation[0]);
                    if (alpha < 0) alpha += 360.0;
                    double beta = Math.toDegrees(orientation[1]);
                    double gamma = Math.toDegrees(orientation[2]);

                    NativeMethods.onDeviceMotionChange(
                            sessionId, token.generation(), alpha, beta, gamma);
                }
            }

            @Override
            public void onAccuracyChanged(Sensor sensor, int accuracy) {
            }
        };

        if (!sensorManager.registerListener(motionListener, sensor, delay)) {
            motionListener = null;
            return false;
        }
        return true;
    }

    /**
     * Stop listening for device motion events.
     */
    public synchronized void stopDeviceMotionListening() {
        if (motionRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopDeviceMotionListeningInternal();
        }
    }

    private void stopDeviceMotionListeningInternal() {
        SensorEventListener listener = motionListener;
        motionListener = null;
        if (sensorManager != null && listener != null) {
            sensorManager.unregisterListener(listener);
        }
    }

    // ==================== Gyroscope ====================

    /**
     * Start listening for gyroscope events.
     * <p>
     * Reports angular velocity in rad/s around x, y, z axes.
     *
     * @param interval "game", "ui", or "normal"
     */
    public synchronized boolean startGyroscope(String interval) {
        LifecycleRequestState.Action action = gyroscopeRequest.requestStart(interval);
        if (action == LifecycleRequestState.Action.RESTART) {
            stopGyroscopeInternal();
        }
        if ((action == LifecycleRequestState.Action.START
                || action == LifecycleRequestState.Action.RESTART)
                && !startGyroscopeInternal(interval)) {
            gyroscopeRequest.startFailed(true);
            return false;
        }
        return true;
    }

    private boolean startGyroscopeInternal(String interval) {
        if (sensorManager == null) return false;

        Sensor sensor = sensorManager.getDefaultSensor(Sensor.TYPE_GYROSCOPE);
        if (sensor == null) return false;

        final int delay = parseInterval(interval);

        gyroscopeListener = new SensorEventListener() {
            @Override
            public void onSensorChanged(SensorEvent event) {
                synchronized (DeviceSensorManager.this) {
                    if (gyroscopeListener != this || !gyroscopeRequest.isActive()) return;
                    NativeMethods.onGyroscopeChange(
                            sessionId,
                            token.generation(),
                            event.values[0],
                            event.values[1],
                            event.values[2]
                    );
                }
            }

            @Override
            public void onAccuracyChanged(Sensor sensor, int accuracy) {
            }
        };

        if (!sensorManager.registerListener(gyroscopeListener, sensor, delay)) {
            gyroscopeListener = null;
            return false;
        }
        return true;
    }

    /**
     * Stop listening for gyroscope events.
     */
    public synchronized void stopGyroscope() {
        if (gyroscopeRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopGyroscopeInternal();
        }
    }

    private void stopGyroscopeInternal() {
        SensorEventListener listener = gyroscopeListener;
        gyroscopeListener = null;
        if (sensorManager != null && listener != null) {
            sensorManager.unregisterListener(listener);
        }
    }

    // ==================== Compass ====================

    /**
     * Start listening for compass (magnetic field + accelerometer) events.
     * <p>
     * Reports direction in degrees (0-360) and accuracy level.
     * Uses TYPE_ORIENTATION sensor for simplicity (deprecated but widely supported),
     * or falls back to TYPE_MAGNETIC_FIELD + TYPE_ACCELEROMETER.
     * <p>
     * Frequency: ~5 times/second (200ms interval, SENSOR_DELAY_NORMAL).
     */
    public synchronized boolean startCompass() {
        LifecycleRequestState.Action action = compassRequest.requestStart(Boolean.TRUE);
        if (action == LifecycleRequestState.Action.RESTART) {
            stopCompassInternal();
        }
        if ((action == LifecycleRequestState.Action.START
                || action == LifecycleRequestState.Action.RESTART)
                && !startCompassInternal()) {
            compassRequest.startFailed(true);
            return false;
        }
        return true;
    }

    private boolean startCompassInternal() {
        if (sensorManager == null) return false;

        // Use TYPE_ORIENTATION for simplicity (deprecated but still works)
        Sensor orientationSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ORIENTATION);
        if (orientationSensor != null) {
            compassListener = new SensorEventListener() {
                @Override
                public void onSensorChanged(SensorEvent event) {
                    synchronized (DeviceSensorManager.this) {
                        if (compassListener != this || !compassRequest.isActive()) return;
                        // event.values[0] = azimuth (direction, 0-360 degrees)
                        double direction = event.values[0];
                        String accuracy = mapAccuracy(event.accuracy);
                        NativeMethods.onCompassChange(
                                sessionId, token.generation(), direction, accuracy);
                    }
                }

                @Override
                public void onAccuracyChanged(Sensor sensor, int accuracy) {
                }
            };
            // ~5 times/second = 200ms = SENSOR_DELAY_NORMAL
            if (!sensorManager.registerListener(
                    compassListener, orientationSensor, SensorManager.SENSOR_DELAY_NORMAL)) {
                compassListener = null;
                return false;
            }
            return true;
        }

        // Fallback: Use magnetic field + accelerometer
        final Sensor magneticSensor = sensorManager.getDefaultSensor(Sensor.TYPE_MAGNETIC_FIELD);
        final Sensor accelerometerSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
        if (magneticSensor == null || accelerometerSensor == null) return false;

        compassListener = new SensorEventListener() {
            private final float[] gravity = new float[3];
            private final float[] geomagnetic = new float[3];
            private final float[] rotationMatrix = new float[9];
            private final float[] orientation = new float[3];
            private int currentAccuracy = SensorManager.SENSOR_STATUS_ACCURACY_MEDIUM;

            @Override
            public void onSensorChanged(SensorEvent event) {
                synchronized (DeviceSensorManager.this) {
                    if (compassListener != this || !compassRequest.isActive()) return;
                    if (event.sensor.getType() == Sensor.TYPE_ACCELEROMETER) {
                        System.arraycopy(event.values, 0, gravity, 0, 3);
                    } else if (event.sensor.getType() == Sensor.TYPE_MAGNETIC_FIELD) {
                        System.arraycopy(event.values, 0, geomagnetic, 0, 3);
                        currentAccuracy = event.accuracy;
                    }

                    if (SensorManager.getRotationMatrix(rotationMatrix, null, gravity, geomagnetic)) {
                        SensorManager.getOrientation(rotationMatrix, orientation);
                        double direction = Math.toDegrees(orientation[0]);
                        if (direction < 0) direction += 360.0;
                        String accuracy = mapAccuracy(currentAccuracy);
                        NativeMethods.onCompassChange(
                                sessionId, token.generation(), direction, accuracy);
                    }
                }
            }

            @Override
            public void onAccuracyChanged(Sensor sensor, int accuracy) {
                synchronized (DeviceSensorManager.this) {
                    if (compassListener != this || !compassRequest.isActive()) return;
                    if (sensor.getType() == Sensor.TYPE_MAGNETIC_FIELD) {
                        currentAccuracy = accuracy;
                    }
                }
            }
        };

        boolean magneticRegistered = sensorManager.registerListener(
                compassListener, magneticSensor, SensorManager.SENSOR_DELAY_NORMAL);
        boolean accelerometerRegistered = sensorManager.registerListener(
                compassListener, accelerometerSensor, SensorManager.SENSOR_DELAY_NORMAL);
        if (!magneticRegistered || !accelerometerRegistered) {
            sensorManager.unregisterListener(compassListener);
            compassListener = null;
            return false;
        }
        return true;
    }

    /**
     * Stop listening for compass events.
     */
    public synchronized void stopCompass() {
        if (compassRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopCompassInternal();
        }
    }

    private void stopCompassInternal() {
        SensorEventListener listener = compassListener;
        compassListener = null;
        if (sensorManager != null && listener != null) {
            sensorManager.unregisterListener(listener);
        }
    }

    /**
     * Map Android sensor accuracy to spec string.
     */
    private static String mapAccuracy(int accuracy) {
        switch (accuracy) {
            case SensorManager.SENSOR_STATUS_ACCURACY_HIGH:
                return "high";
            case SensorManager.SENSOR_STATUS_ACCURACY_MEDIUM:
                return "medium";
            case SensorManager.SENSOR_STATUS_ACCURACY_LOW:
                return "low";
            case SensorManager.SENSOR_STATUS_NO_CONTACT:
                return "no-contact";
            case SensorManager.SENSOR_STATUS_UNRELIABLE:
                return "unreliable";
            default:
                return "unknown " + accuracy;
        }
    }

    // ==================== Accelerometer ====================

    /**
     * Start listening for accelerometer events.
     * <p>
     * Reports acceleration in m/s^2 along x, y, z axes (including gravity).
     *
     * @param interval "game", "ui", or "normal"
     */
    public synchronized boolean startAccelerometer(String interval) {
        LifecycleRequestState.Action action = accelerometerRequest.requestStart(interval);
        if (action == LifecycleRequestState.Action.RESTART) {
            stopAccelerometerInternal();
        }
        if ((action == LifecycleRequestState.Action.START
                || action == LifecycleRequestState.Action.RESTART)
                && !startAccelerometerInternal(interval)) {
            accelerometerRequest.startFailed(true);
            return false;
        }
        return true;
    }

    private boolean startAccelerometerInternal(String interval) {
        if (sensorManager == null) return false;

        Sensor sensor = sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
        if (sensor == null) return false;

        final int delay = parseInterval(interval);

        accelerometerListener = new SensorEventListener() {
            @Override
            public void onSensorChanged(SensorEvent event) {
                synchronized (DeviceSensorManager.this) {
                    if (accelerometerListener != this || !accelerometerRequest.isActive()) return;
                    // In g, as every host reports it: Android's m/s^2 divided by
                    // standard gravity. The axes are already the convention.
                    NativeMethods.onAccelerometerChange(
                            sessionId,
                            token.generation(),
                            event.values[0] / SensorManager.STANDARD_GRAVITY,
                            event.values[1] / SensorManager.STANDARD_GRAVITY,
                            event.values[2] / SensorManager.STANDARD_GRAVITY
                    );
                }
            }

            @Override
            public void onAccuracyChanged(Sensor sensor, int accuracy) {
            }
        };

        if (!sensorManager.registerListener(accelerometerListener, sensor, delay)) {
            accelerometerListener = null;
            return false;
        }
        return true;
    }

    /**
     * Stop listening for accelerometer events.
     */
    public synchronized void stopAccelerometer() {
        if (accelerometerRequest.requestStop() == LifecycleRequestState.Action.STOP) {
            stopAccelerometerInternal();
        }
    }

    private void stopAccelerometerInternal() {
        SensorEventListener listener = accelerometerListener;
        accelerometerListener = null;
        if (sensorManager != null && listener != null) {
            sensorManager.unregisterListener(listener);
        }
    }

    // ==================== Cleanup ====================

    public synchronized void setLifecycleSuspended(boolean suspended) {
        if (suspended) {
            suspendForLifecycle();
        } else {
            resumeForLifecycle();
        }
    }

    public synchronized void suspendForLifecycle() {
        if (motionRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopDeviceMotionListeningInternal();
        }
        if (gyroscopeRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopGyroscopeInternal();
        }
        if (compassRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopCompassInternal();
        }
        if (accelerometerRequest.suspend() == LifecycleRequestState.Action.STOP) {
            stopAccelerometerInternal();
        }
    }

    public synchronized void resumeForLifecycle() {
        if (motionRequest.resume() == LifecycleRequestState.Action.START
                && !startDeviceMotionListeningInternal(motionRequest.getRequest())) {
            motionRequest.startFailed(true);
        }
        if (gyroscopeRequest.resume() == LifecycleRequestState.Action.START
                && !startGyroscopeInternal(gyroscopeRequest.getRequest())) {
            gyroscopeRequest.startFailed(true);
        }
        if (compassRequest.resume() == LifecycleRequestState.Action.START
                && !startCompassInternal()) {
            compassRequest.startFailed(true);
        }
        if (accelerometerRequest.resume() == LifecycleRequestState.Action.START
                && !startAccelerometerInternal(accelerometerRequest.getRequest())) {
            accelerometerRequest.startFailed(true);
        }
    }

    /**
     * Stop all sensor listeners. Call when session is destroyed.
     */
    public synchronized void destroy() {
        motionRequest.destroy();
        gyroscopeRequest.destroy();
        compassRequest.destroy();
        accelerometerRequest.destroy();
        stopDeviceMotionListeningInternal();
        stopGyroscopeInternal();
        stopCompassInternal();
        stopAccelerometerInternal();
    }

    // ==================== Internal ====================

    /**
     * Remap rotation matrix based on current display rotation so that
     * orientation angles are correct regardless of screen orientation.
     */
    private float[] remapForDisplay(float[] rotationMatrix) {
        int axisX = SensorManager.AXIS_X;
        int axisY = SensorManager.AXIS_Y;

        if (windowManager != null) {
            // Cache rotation to avoid querying display on every sensor event
            long now = System.currentTimeMillis();
            if (now - lastRotationCheck > ROTATION_CHECK_INTERVAL_MS) {
                cachedRotation = windowManager.getDefaultDisplay().getRotation();
                lastRotationCheck = now;
            }
            switch (cachedRotation) {
                case Surface.ROTATION_90:
                    axisX = SensorManager.AXIS_Y;
                    axisY = SensorManager.AXIS_MINUS_X;
                    break;
                case Surface.ROTATION_180:
                    axisX = SensorManager.AXIS_MINUS_X;
                    axisY = SensorManager.AXIS_MINUS_Y;
                    break;
                case Surface.ROTATION_270:
                    axisX = SensorManager.AXIS_MINUS_Y;
                    axisY = SensorManager.AXIS_X;
                    break;
                default: // ROTATION_0
                    break;
            }
        }

        SensorManager.remapCoordinateSystem(rotationMatrix, axisX, axisY, remappedMatrix);
        return remappedMatrix;
    }

    /**
     * @param interval "game", "ui", or "normal" (default)
     * @return SensorManager delay constant
     */
    private static int parseInterval(String interval) {
        if (interval == null) return SensorManager.SENSOR_DELAY_NORMAL;
        switch (interval) {
            case "game":
                return SensorManager.SENSOR_DELAY_GAME;
            case "ui":
                return SensorManager.SENSOR_DELAY_UI;
            default:
                return SensorManager.SENSOR_DELAY_NORMAL;
        }
    }
}
