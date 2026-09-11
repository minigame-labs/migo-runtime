package com.migo.runtime.internal;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.view.Surface;

import com.migo.runtime.RuntimeConfig;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * High-level wrapper around native JNI methods.
 * <p>
 * Provides a safer API with proper parameter validation and error handling.
 * This is the recommended way to interact with the native engine.
 *
 * @hide
 */
public final class NativeMethods {
    interface PermissionUpdater {
        boolean update(int sessionId, String scope, boolean granted);
    }

    interface SessionShutdown {
        boolean shutdown(int sessionId);
    }

    private static final PermissionUpdater NATIVE_PERMISSION_UPDATER =
            NativeBridge::updatePermission;
    private static volatile PermissionUpdater sPermissionUpdater = NATIVE_PERMISSION_UPDATER;
    private static final SessionShutdown NATIVE_SESSION_SHUTDOWN = NativeBridge::shutdown;
    private static volatile SessionShutdown sSessionShutdown = NATIVE_SESSION_SHUTDOWN;

    /**
     * Size of a single TouchPoint structure in bytes.
     * Layout: id(4) + x(4) + y(4) + force(4) + flags(4) = 20 bytes
     */
    private static final int TOUCH_POINT_SIZE = 20;

    /**
     * Maximum number of simultaneous touch points supported.
     */
    private static final int MAX_TOUCH_POINTS = 10;

    /** Maximum accepted camera frame width or height, in physical pixels. */
    private static final int MAX_CAMERA_FRAME_DIMENSION = 8192;

    /** Maximum accepted camera frame pixel count. */
    private static final long MAX_CAMERA_FRAME_PIXELS =
            (long) MAX_CAMERA_FRAME_DIMENSION * MAX_CAMERA_FRAME_DIMENSION;

    /** Maximum synchronous Y/U/V payload copied from a camera callback. */
    private static final long MAX_CAMERA_FRAME_BYTES = 64L * 1024L * 1024L;

    private static final int DEFAULT_LAUNCH_SCENE = 1001;

    private static final String EXTRA_LAUNCH_OPTIONS_JSON = "migo_launch_options_json";
    private static final String EXTRA_SCENE = "migo_scene";
    private static final String EXTRA_QUERY = "migo_query";
    private static final String EXTRA_QUERY_JSON = "migo_query_json";
    private static final String EXTRA_SHARE_TICKET = "migo_share_ticket";
    private static final String EXTRA_REFERRER_APP_ID = "migo_referrer_app_id";
    private static final String EXTRA_REFERRER_CHAT_TYPE = "migo_referrer_chat_type";
    private static final String EXTRA_REFERRER_EXTRA_DATA_JSON = "migo_referrer_extra_data";

    /**
     * Thread-local ByteBuffer for touch events to avoid allocation per event.
     */
    private static final ThreadLocal<ByteBuffer> sTouchBuffer = new ThreadLocal<ByteBuffer>() {
        @Override
        protected ByteBuffer initialValue() {
            ByteBuffer buffer = ByteBuffer.allocateDirect(TOUCH_POINT_SIZE * MAX_TOUCH_POINTS);
            buffer.order(ByteOrder.nativeOrder());
            return buffer;
        }
    };

    private NativeMethods() {}

    // ==================== Core Lifecycle ====================

    /**
     * Get the native engine version.
     *
     * @return Version string, or "unknown" if native library failed to load
     */
    public static String getVersion() {
        try {
            String version = NativeBridge.version();
            return version != null ? version : "unknown";
        } catch (UnsatisfiedLinkError e) {
            return "unknown";
        }
    }

    /**
     * Return the loaded native version without converting a linkage failure to
     * a value. Runtime initialization uses this to preserve its existing
     * native-load failure state machine while staying behind this facade.
     */
    public static String getLoadedVersion() {
        return NativeBridge.version();
    }

    /** Return the native ABI's minimum supported Android API level. */
    public static int getMinApiLevel() {
        return NativeBridge.getMinApiLevel();
    }

    /**
     * Load externally supplied ICU data. Empty paths are rejected before
     * crossing JNI; the public runtime API retains its descriptive exception.
     */
    public static boolean initIcuData(String icuDataPath) {
        if (icuDataPath == null || icuDataPath.trim().isEmpty()) {
            return false;
        }
        return NativeBridge.initIcuData(icuDataPath);
    }

    /** Get a session's debug statistics without exposing the raw JNI method. */
    public static byte[] getDebugStats(int sessionId) {
        return sessionId >= 0 ? NativeBridge.getDebugStats(sessionId) : null;
    }

    /** Get console records after a non-negative cursor through the checked facade. */
    public static String getConsoleLogs(int sessionId, long sinceCursor) {
        return sessionId >= 0 && sinceCursor >= 0
                ? NativeBridge.getConsoleLogs(sessionId, sinceCursor)
                : null;
    }

    /**
     * Initialize a new session.
     *
     * @param surface Android Surface for rendering
     * @param config  Runtime configuration
     * @return Session ID (>= 0) on success, negative error code on failure
     * @throws IllegalArgumentException if surface or config is null
     */
    public static int init(Surface surface, RuntimeConfig config) {
        if (surface == null) {
            throw new IllegalArgumentException("Surface cannot be null");
        }
        if (config == null) {
            throw new IllegalArgumentException("RuntimeConfig cannot be null");
        }
        return NativeBridge.init(surface, config);
    }

    /**
     * Initialize a session whose rendering Surface has not arrived yet.
     *
     * @param config Runtime configuration
     * @return Session ID (>= 0) on success, negative error code on failure
     * @throws IllegalArgumentException if config is null
     */
    public static int initWarm(RuntimeConfig config) {
        if (config == null) {
            throw new IllegalArgumentException("RuntimeConfig cannot be null");
        }
        // Separate entry point because in init() a null Surface is a caller
        // bug, while here it is what asks the engine for a warm start.
        return NativeBridge.init(null, config);
    }

    /**
     * Shut down a session.
     *
     * @param sessionId The session ID to shut down
     */
    public static boolean shutdown(int sessionId) {
        return sessionId >= 0 && sSessionShutdown.shutdown(sessionId);
    }

    // ==================== Surface Management ====================

    /**
     * Update the rendering surface.
     *
     * @param sessionId The session ID
     * @param surface   New Surface object
     * @param width     Surface buffer width in physical pixels
     * @param height    Surface buffer height in physical pixels
     * @param density   Current device-pixel ratio for this display
     */
    public static void updateSurface(
            int sessionId, Surface surface, int width, int height, float density) {
        if (sessionId >= 0 && surface != null) {
            NativeBridge.updateSurface(sessionId, surface, width, height, density);
        }
    }

    /**
     * Notify native code that the rendering surface was destroyed.
     *
     * @param sessionId The session ID
     */
    public static void onSurfaceDestroyed(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onSurfaceDestroyed(sessionId);
        }
    }

    // ==================== Lifecycle Events ====================

    /**
     * Notify session that it should resume.
     *
     * @param sessionId The session ID
     */
    public static void onShow(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onShow(sessionId, buildLaunchOptionsJson(sessionId));
        }
    }

    private static String buildLaunchOptionsJson(int sessionId) {
        try {
            RuntimeContext runtimeContext = RuntimeRegistry.get(sessionId);
            Activity activity = runtimeContext != null ? runtimeContext.getActivity() : null;
            Intent intent = activity != null ? activity.getIntent() : null;

            JSONObject launchOptions = createDefaultLaunchOptions();
            if (intent == null) {
                return launchOptions.toString();
            }

            String launchOptionsJson = intent.getStringExtra(EXTRA_LAUNCH_OPTIONS_JSON);
            if (launchOptionsJson != null && !launchOptionsJson.trim().isEmpty()) {
                try {
                    JSONObject parsed = new JSONObject(launchOptionsJson);
                    return parsed.toString();
                } catch (JSONException ignored) {
                }
            }

            launchOptions.put("scene", readScene(intent));
            launchOptions.put("query", readQuery(intent));
            launchOptions.put("shareTicket", readStringExtra(intent, EXTRA_SHARE_TICKET));
            launchOptions.put("referrerInfo", readReferrerInfo(intent));
            return launchOptions.toString();
        } catch (Exception ignored) {
            return createDefaultLaunchOptions().toString();
        }
    }

    private static JSONObject createDefaultLaunchOptions() {
        JSONObject options = new JSONObject();
        try {
            options.put("scene", DEFAULT_LAUNCH_SCENE);
            options.put("query", new JSONObject());
            options.put("shareTicket", "");
            JSONObject referrerInfo = new JSONObject();
            referrerInfo.put("appId", "");
            referrerInfo.put("extraData", new JSONObject());
            referrerInfo.put("chatType", 0);
            options.put("referrerInfo", referrerInfo);
        } catch (JSONException ignored) {
        }
        return options;
    }

    private static int readScene(Intent intent) {
        int numeric = intent.getIntExtra(EXTRA_SCENE, Integer.MIN_VALUE);
        if (numeric != Integer.MIN_VALUE) {
            return numeric;
        }

        String text = intent.getStringExtra(EXTRA_SCENE);
        if (text != null) {
            try {
                return Integer.parseInt(text.trim());
            } catch (NumberFormatException ignored) {
            }
        }

        return DEFAULT_LAUNCH_SCENE;
    }

    private static JSONObject readQuery(Intent intent) {
        JSONObject query = new JSONObject();

        String queryJson = intent.getStringExtra(EXTRA_QUERY_JSON);
        if (queryJson != null && !queryJson.trim().isEmpty()) {
            try {
                JSONObject parsed = new JSONObject(queryJson);
                mergeJsonObject(query, parsed);
            } catch (JSONException ignored) {
            }
        }

        appendQueryString(query, intent.getStringExtra(EXTRA_QUERY));

        Uri data = intent.getData();
        if (data != null) {
            appendQueryString(query, data.getQuery());
        }

        return query;
    }

    private static JSONObject readReferrerInfo(Intent intent) {
        JSONObject referrerInfo = new JSONObject();
        try {
            referrerInfo.put("appId", readStringExtra(intent, EXTRA_REFERRER_APP_ID));

            JSONObject extraData = new JSONObject();
            String extraDataJson = intent.getStringExtra(EXTRA_REFERRER_EXTRA_DATA_JSON);
            if (extraDataJson != null && !extraDataJson.trim().isEmpty()) {
                try {
                    JSONObject parsed = new JSONObject(extraDataJson);
                    mergeJsonObject(extraData, parsed);
                } catch (JSONException ignored) {
                }
            }
            referrerInfo.put("extraData", extraData);

            int chatType = intent.getIntExtra(EXTRA_REFERRER_CHAT_TYPE, Integer.MIN_VALUE);
            if (chatType == Integer.MIN_VALUE) {
                String chatTypeText = intent.getStringExtra(EXTRA_REFERRER_CHAT_TYPE);
                if (chatTypeText != null) {
                    try {
                        chatType = Integer.parseInt(chatTypeText.trim());
                    } catch (NumberFormatException ignored) {
                        chatType = 0;
                    }
                } else {
                    chatType = 0;
                }
            }
            referrerInfo.put("chatType", chatType);
        } catch (JSONException ignored) {
        }

        return referrerInfo;
    }

    private static String readStringExtra(Intent intent, String key) {
        String value = intent.getStringExtra(key);
        return value != null ? value : "";
    }

    private static void appendQueryString(JSONObject target, String rawQuery) {
        if (rawQuery == null || rawQuery.isEmpty()) {
            return;
        }

        String query = rawQuery.startsWith("?") ? rawQuery.substring(1) : rawQuery;
        if (query.isEmpty()) {
            return;
        }

        String[] pairs = query.split("&");
        for (String pair : pairs) {
            if (pair == null || pair.isEmpty()) {
                continue;
            }

            int index = pair.indexOf('=');
            String keyRaw = index >= 0 ? pair.substring(0, index) : pair;
            String valueRaw = index >= 0 ? pair.substring(index + 1) : "";

            String key = Uri.decode(keyRaw);
            if (key == null || key.isEmpty()) {
                continue;
            }

            String value = Uri.decode(valueRaw);
            try {
                target.put(key, value != null ? value : "");
            } catch (JSONException ignored) {
            }
        }
    }

    private static void mergeJsonObject(JSONObject target, JSONObject source) {
        if (target == null || source == null) {
            return;
        }

        java.util.Iterator<String> keys = source.keys();
        while (keys.hasNext()) {
            String key = keys.next();
            try {
                target.put(key, source.opt(key));
            } catch (JSONException ignored) {
            }
        }
    }

    /**
     * Notify session that it should pause.
     *
     * @param sessionId The session ID
     */
    public static void onHide(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onHide(sessionId);
        }
    }

    /**
     * Restart the game session.
     *
     * @param sessionId The session ID
     */
    public static void onRestart(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onRestart(sessionId);
        }
    }

    /**
     * Notify session that audio has been interrupted by the system
     * (e.g., incoming phone call).
     *
     * @param sessionId The session ID
     */
    public static void onAudioInterruptionBegin(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onAudioInterruptionBegin(sessionId);
        }
    }

    /**
     * Notify session that audio interruption has ended
     * and playback can resume.
     *
     * @param sessionId The session ID
     */
    public static void onAudioInterruptionEnd(int sessionId) {
        if (sessionId >= 0) {
            NativeBridge.onAudioInterruptionEnd(sessionId);
        }
    }

    // ==================== Input Events ====================

    /**
     * Send touch events to the session using array parameters.
     *
     * @param sessionId The session ID
     * @param action    Touch action code
     * @param time      Event timestamp in milliseconds
     * @param ids       Array of pointer IDs
     * @param xs        Array of X coordinates
     * @param ys        Array of Y coordinates
     * @param forces    Array of pressure values (0.0-1.0)
     * @param flags     Array of flags (1 = changed pointer)
     */
    public static boolean onTouch(int sessionId, int action, long time,
                                  int[] ids, float[] xs, float[] ys, float[] forces, int[] flags) {
        if (sessionId < 0 || ids == null || ids.length == 0) {
            return false;
        }

        int count = Math.min(ids.length, MAX_TOUCH_POINTS);
        ByteBuffer buffer = sTouchBuffer.get();
        buffer.clear();

        for (int i = 0; i < count; i++) {
            buffer.putInt(ids[i]);
            buffer.putFloat(xs != null && i < xs.length ? xs[i] : 0);
            buffer.putFloat(ys != null && i < ys.length ? ys[i] : 0);
            buffer.putFloat(forces != null && i < forces.length ? forces[i] : 1.0f);
            buffer.putInt(flags != null && i < flags.length ? flags[i] : 0);
        }

        buffer.flip();
        return NativeBridge.onTouchEvent(sessionId, action, time, count, buffer);
    }

    /**
     * Send raw touch buffer to the session.
     *
     * @param sessionId The session ID
     * @param action    Touch action code
     * @param time      Event timestamp in milliseconds
     * @param count     Number of touch points
     * @param buffer    Pre-packed DirectByteBuffer
     */
    public static boolean onTouchRaw(int sessionId, int action, long time, int count,
                                     ByteBuffer buffer) {
        if (sessionId >= 0 && count > 0 && buffer != null) {
            return NativeBridge.onTouchEvent(sessionId, action, time, count, buffer);
        }
        return false;
    }

    // ==================== Game Loading ====================

    /**
     * Load and run a game module.
     * <p>
     * Game paths are derived from gameId and base directories (filesDir, cacheDir)
     * configured in RuntimeConfig.
     *
     * @param sessionId The session ID
     * @param gameId    Unique game identifier (1-64 lower-case alphanumeric, underscore, hyphen)
     * @param entry     Entry point file (e.g., "game.js")
     * @return 0 on success, negative error code on failure
     */
    public static int modMain(int sessionId, String gameId, String entry) {
        if (sessionId < 0 || gameId == null || entry == null) {
            return -1;
        }
        return NativeBridge.modMain(sessionId, gameId, entry);
    }

    // ==================== System Callbacks ====================

    /**
     * Callback for Bluetooth setting result.
     *
     * @param sessionId The session ID
     * @param requestId The id the request carried, or
     *                  {@link CallbackCorrelation#ABSENT}
     * @param enabled   Whether Bluetooth is now enabled
     */
    public static void onBluetoothSettingResult(int sessionId, int requestId, boolean enabled) {
        if (sessionId >= 0) {
            NativeBridge.onOpenSystemBluetoothSetting(sessionId, requestId, enabled ? 1 : 0);
        }
    }

    /**
     * Callback for app authorization setting result.
     *
     * @param sessionId The session ID
     * @param requestId The id the request carried, or
     *                  {@link CallbackCorrelation#ABSENT}
     * @param code      0 on success, negative on failure
     */
    public static void onAppAuthorizeSettingResult(int sessionId, int requestId, int code) {
        if (sessionId >= 0) {
            NativeBridge.onOpenAppAuthorizeSetting(sessionId, requestId, code);
        }
    }

    /**
     * Callback for modal dialog result.
     *
     * @param sessionId The session ID
     * @param requestId The id the request carried, or
     *                  {@link CallbackCorrelation#ABSENT}
     * @param confirm   1 if user tapped confirm, 0 otherwise
     * @param cancel    1 if user tapped cancel, 0 otherwise
     */
    public static void onModalResult(int sessionId, int requestId, int confirm, int cancel) {
        if (sessionId >= 0) {
            NativeBridge.onModalResult(sessionId, requestId, confirm, cancel);
        }
    }

    /**
     * Callback for action sheet result.
     *
     * @param sessionId The session ID
     * @param requestId The id the request carried, or
     *                  {@link CallbackCorrelation#ABSENT}
     * @param tapIndex  Index of selected item (0-based), or -1 if cancelled
     */
    public static void onActionSheetResult(int sessionId, int requestId, int tapIndex) {
        if (sessionId >= 0) {
            NativeBridge.onActionSheetResult(sessionId, requestId, tapIndex);
        }
    }

    // ==================== Device Sensor Callbacks ====================

    /**
     * Callback for device motion sensor data.
     * Called from {@link com.migo.runtime.internal.platform.DeviceSensorManager}.
     *
     * @param sessionId The session ID
     * @param alpha     Rotation around Z axis in degrees (0-360)
     * @param beta      Rotation around X axis in degrees (-180 to 180)
     * @param gamma     Rotation around Y axis in degrees (-90 to 90)
     */
    public static void onDeviceMotionChange(
            int sessionId, long generation, double alpha, double beta, double gamma) {
        if (sessionId >= 0) {
            NativeBridge.onDeviceMotionChange(sessionId, generation, alpha, beta, gamma);
        }
    }

    /**
     * Callback for gyroscope sensor data.
     * Called from {@link com.migo.runtime.internal.platform.DeviceSensorManager}.
     *
     * @param sessionId The session ID
     * @param x         Angular velocity around X axis in rad/s
     * @param y         Angular velocity around Y axis in rad/s
     * @param z         Angular velocity around Z axis in rad/s
     */
    public static void onGyroscopeChange(
            int sessionId, long generation, double x, double y, double z) {
        if (sessionId >= 0) {
            NativeBridge.onGyroscopeChange(sessionId, generation, x, y, z);
        }
    }

    /**
     * Callback for device orientation change.
     *
     * @param sessionId The session ID
     * @param value     One of: "portrait", "landscape", "landscapeReverse"
     */
    public static void onDeviceOrientationChange(int sessionId, String value) {
        if (sessionId >= 0 && value != null) {
            NativeBridge.onDeviceOrientationChange(sessionId, value);
        }
    }

    /**
     * Callback for compass data.
     *
     * @param sessionId The session ID
     * @param direction Direction in degrees (0-360, 0 = north)
     * @param accuracy  Accuracy string: "high", "medium", "low", "no-contact", "unreliable", or "unknow X"
     */
    public static void onCompassChange(
            int sessionId, long generation, double direction, String accuracy) {
        if (sessionId >= 0) {
            NativeBridge.onCompassChange(sessionId, generation, direction, accuracy);
        }
    }

    /**
     * Callback for accelerometer data.
     * Called from {@link com.migo.runtime.internal.platform.DeviceSensorManager}.
     *
     * @param sessionId The session ID
     * @param x         Acceleration along X axis in m/s^2
     * @param y         Acceleration along Y axis in m/s^2
     * @param z         Acceleration along Z axis in m/s^2
     */
    public static void onAccelerometerChange(
            int sessionId, long generation, double x, double y, double z) {
        if (sessionId >= 0) {
            NativeBridge.onAccelerometerChange(sessionId, generation, x, y, z);
        }
    }

    // ==================== Network Callbacks ====================

    /**
     * Callback for network status change.
     * Called from {@link com.migo.runtime.internal.platform.NetworkMonitor}.
     *
     * @param sessionId   The session ID
     * @param isConnected Whether network is connected
     * @param networkType Network type: "wifi", "2g", "3g", "4g", "5g", "unknown", "none"
     */
    public static void onNetworkStatusChange(int sessionId, boolean isConnected, String networkType) {
        if (sessionId >= 0) {
            NativeBridge.onNetworkStatusChange(sessionId, isConnected, networkType);
        }
    }

    // ==================== Camera Callbacks ====================

    /**
     * Callback for camera events.
     * Called from {@link com.migo.runtime.internal.platform.CameraManager}.
     *
     * @param sessionId   The session ID
     * @param cameraId    The camera instance ID
     * @param eventType   Event type: "stop", "error", "authCancel", "timeoutCallback"
     * @param jsonPayload JSON-encoded event data
     */
    public static void onCameraEvent(
            int sessionId, long generation, int cameraId, String eventType, String jsonPayload) {
        if (sessionId >= 0 && eventType != null) {
            NativeBridge.onCameraEvent(sessionId, generation, cameraId, eventType,
                    jsonPayload != null ? jsonPayload : "{}");
        }
    }

    /**
     * Callback for camera frame data.
     * Called from {@link com.migo.runtime.internal.platform.CameraManager}.
     *
     * @param sessionId The session ID
     * @param cameraId  The camera instance ID
     * @param yBuffer   Direct Y-plane ByteBuffer
     * @param yOffset   Y-plane window start (buffer position)
     * @param yLength   Y-plane window length (buffer remaining)
     * @param uBuffer   Direct U-plane ByteBuffer
     * @param uOffset   U-plane window start
     * @param uLength   U-plane window length
     * @param vBuffer   Direct V-plane ByteBuffer
     * @param vOffset   V-plane window start
     * @param vLength   V-plane window length
     * @param width     Frame width in pixels
     * @param height    Frame height in pixels
     */
    public static void onCameraFrameData(int sessionId, long generation, int cameraId,
            ByteBuffer yBuffer, int yOffset, int yLength,
            ByteBuffer uBuffer, int uOffset, int uLength,
            ByteBuffer vBuffer, int vOffset, int vLength,
            int width, int height) {
        // Native code reads each plane via its direct address. Reject malformed
        // or oversized metadata before crossing JNI; the Rust entry point and
        // shared packing boundary repeat their own checks.
        if (sessionId >= 0
                && isCameraFrameMetadataWithinLimits(
                        width, height, yLength, uLength, vLength)
                && yBuffer != null && yBuffer.isDirect()
                && uBuffer != null && uBuffer.isDirect()
                && vBuffer != null && vBuffer.isDirect()) {
            NativeBridge.onCameraFrameData(sessionId, generation, cameraId,
                    yBuffer, yOffset, yLength,
                    uBuffer, uOffset, uLength,
                    vBuffer, vOffset, vLength,
                    width, height);
        }
    }

    private static boolean isCameraFrameMetadataWithinLimits(
            int width, int height, int yLength, int uLength, int vLength) {
        if (width <= 0 || height <= 0
                || width > MAX_CAMERA_FRAME_DIMENSION
                || height > MAX_CAMERA_FRAME_DIMENSION
                || yLength < 0 || uLength < 0 || vLength < 0) {
            return false;
        }

        final long pixels;
        final long totalBytes;
        try {
            pixels = Math.multiplyExact((long) width, (long) height);
            totalBytes = Math.addExact(
                    Math.addExact((long) yLength, (long) uLength), (long) vLength);
        } catch (ArithmeticException ignored) {
            return false;
        }
        return pixels <= MAX_CAMERA_FRAME_PIXELS && totalBytes <= MAX_CAMERA_FRAME_BYTES;
    }

    // ==================== Bluetooth Callbacks ====================

    /**
     * Callback for Bluetooth adapter state change.
     * Called from {@link com.migo.runtime.internal.platform.BluetoothManager}.
     *
     * @param sessionId   The session ID
     * @param available   Whether the adapter is available
     * @param discovering Whether the adapter is discovering devices
     */
    public static void onBluetoothAdapterStateChange(int sessionId, boolean available, boolean discovering) {
        if (sessionId >= 0) {
            NativeBridge.onBluetoothAdapterStateChange(sessionId, available, discovering);
        }
    }

    /**
     * Callback for Bluetooth device found.
     * Called from {@link com.migo.runtime.internal.platform.BluetoothManager}.
     *
     * @param sessionId   The session ID
     * @param devicesJson JSON-encoded array of discovered devices
     */
    public static void onBluetoothDeviceFound(int sessionId, String devicesJson) {
        if (sessionId >= 0 && devicesJson != null) {
            NativeBridge.onBluetoothDeviceFound(sessionId, devicesJson);
        }
    }

    /**
     * Callback for Beacon update.
     * Called from {@link com.migo.runtime.internal.platform.BluetoothManager}.
     *
     * @param sessionId   The session ID
     * @param beaconsJson JSON-encoded array of beacon devices
     */
    public static void onBeaconUpdate(int sessionId, String beaconsJson) {
        if (sessionId >= 0 && beaconsJson != null) {
            NativeBridge.onBeaconUpdate(sessionId, beaconsJson);
        }
    }

    /**
     * Callback for Beacon service state change.
     * Called from {@link com.migo.runtime.internal.platform.BluetoothManager}.
     *
     * @param sessionId   The session ID
     * @param available   Whether the beacon service is available
     * @param discovering Whether the beacon service is discovering
     */
    public static void onBeaconServiceChange(int sessionId, boolean available, boolean discovering) {
        if (sessionId >= 0) {
            NativeBridge.onBeaconServiceChange(sessionId, available, discovering);
        }
    }

    // ==================== BLE GATT Events ====================

    public static void onBLEConnectionStateChange(int sessionId, String deviceId, boolean connected) {
        if (sessionId >= 0 && deviceId != null) {
            NativeBridge.onBLEConnectionStateChange(sessionId, deviceId, connected);
        }
    }

    /**
     * The value of a characteristic that reported none, shared rather than
     * built: this is a notification path, and Section 6.1 requires it not to
     * allocate.
     */
    private static final byte[] NO_BLE_VALUE = new byte[0];

    public static void onBLECharacteristicValueChange(int sessionId, String deviceId,
                                                        String serviceId, String characteristicId, byte[] value) {
        if (sessionId >= 0 && deviceId != null && serviceId != null && characteristicId != null) {
            NativeBridge.onBLECharacteristicValueChange(sessionId, deviceId, serviceId, characteristicId, value != null ? value : NO_BLE_VALUE);
        }
    }

    public static void onBLEMTUChange(int sessionId, String deviceId, int mtu) {
        if (sessionId >= 0 && deviceId != null) {
            NativeBridge.onBLEMTUChange(sessionId, deviceId, mtu);
        }
    }

    // ==================== Screen Capture ====================

    /**
     * Notify that the user took a screenshot.
     * Triggers migo.onUserCaptureScreen listener in JS.
     *
     * @param sessionId The session ID
     */
    public static void onUserCaptureScreen(int sessionId, long generation) {
        if (sessionId >= 0) {
            NativeBridge.onUserCaptureScreen(sessionId, generation);
        }
    }

    // ==================== Memory Warning ====================

    /**
     * Notify that the system has sent a memory warning.
     * Triggers migo.onMemoryWarning listener in JS.
     *
     * @param sessionId The session ID
     * @param level     Memory warning level (Android TRIM_MEMORY_* constant):
     *                  5 = RUNNING_MODERATE, 10 = RUNNING_LOW, 15 = RUNNING_CRITICAL
     */
    public static void onMemoryWarning(int sessionId, int level) {
        if (sessionId >= 0) {
            NativeBridge.onMemoryWarning(sessionId, level);
        }
    }

    // ==================== ADPF Thermal ====================

    /**
     * Notify that the device thermal status has changed (ADPF, API 29+).
     *
     * @param sessionId The session ID
     * @param status    Thermal status level: 0=none, 1=light, 2=moderate,
     *                  3=severe, 4=critical, 5=emergency, 6=shutdown
     */
    public static void onThermalStatusChanged(int sessionId, int status) {
        if (sessionId >= 0) {
            NativeBridge.onThermalStatusChanged(sessionId, status);
        }
    }

    // ==================== Keyboard Callbacks ====================

    /**
     * Callback for keyboard input text change.
     *
     * @param sessionId  The session ID
     * @param generation The generation the keyboard manager captured
     * @param value      Current text value
     */
    public static void onKeyboardInput(int sessionId, long generation, String value) {
        if (sessionId >= 0 && value != null) {
            NativeBridge.onKeyboardInput(sessionId, generation, value);
        }
    }

    /**
     * Callback for keyboard confirm action.
     *
     * @param sessionId  The session ID
     * @param generation The generation the keyboard manager captured
     * @param value      Current text value
     */
    public static void onKeyboardConfirm(int sessionId, long generation, String value) {
        if (sessionId >= 0 && value != null) {
            NativeBridge.onKeyboardConfirm(sessionId, generation, value);
        }
    }

    /**
     * Callback for keyboard complete (dismissed).
     *
     * @param sessionId  The session ID
     * @param generation The generation the keyboard manager captured
     * @param value      Current text value
     */
    public static void onKeyboardComplete(int sessionId, long generation, String value) {
        if (sessionId >= 0 && value != null) {
            NativeBridge.onKeyboardComplete(sessionId, generation, value);
        }
    }

    /**
     * Callback for keyboard height change.
     *
     * @param sessionId  The session ID
     * @param generation The generation the keyboard manager captured
     * @param height     Keyboard height in CSS pixels (0 when hidden)
     */
    public static void onKeyboardHeightChange(int sessionId, long generation, double height) {
        if (sessionId >= 0) {
            NativeBridge.onKeyboardHeightChange(sessionId, generation, height);
        }
    }

    // ==================== Image API Callbacks ====================

    /**
     * Callback for chooseImage result.
     * Called from {@link com.migo.runtime.internal.platform.ImageApiManager}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result with tempFilePaths/tempFiles or error
     */
    public static void onCompressImageResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onCompressImageResult(sessionId, resultJson);
        }
    }

    public static void onChooseImageResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onChooseImageResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for chooseMessageFile result.
     * Called from {@link com.migo.runtime.internal.platform.ImageApiManager}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result with tempFiles or error
     */
    public static void onChooseMessageFileResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onChooseMessageFileResult(sessionId, resultJson);
        }
    }

    // ==================== Location Callbacks ====================

    /**
     * Callback for getLocation result.
     * Called from {@link com.migo.runtime.internal.platform.LocationProvider}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result with location data or error
     */
    public static void onLocationResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onLocationResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for getFuzzyLocation result.
     * Called from {@link com.migo.runtime.internal.platform.LocationProvider}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result with location data or error
     */
    public static void onFuzzyLocationResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onFuzzyLocationResult(sessionId, resultJson);
        }
    }

    // ==================== Scan Code Callbacks ====================

    /**
     * Callback for scanCode result.
     * Called from {@link com.migo.runtime.internal.platform.ScanCodeManager}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result with scan data or error
     */
    public static void onScanCodeResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onScanCodeResult(sessionId, resultJson);
        }
    }

    // ==================== Permission Callbacks ====================

    /**
     * Settle one pending {@code migo.authorize()}.
     *
     * @param sessionId The session ID
     * @param resultJson JSON: {"requestId":N,"granted":true} or {"requestId":N,"error":"..."}
     */
    public static void onAuthorizeResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onAuthorizeResult(sessionId, resultJson);
        }
    }

    /**
     * Record the host's standing decision for one scope.
     * <p>
     * Goes into the native cache that capability checks read. Not routed to
     * JavaScript: state content could reach is state content could rewrite.
     *
     * @param sessionId The session ID
     * @param scope     platform scope name, e.g. "scope.camera"
     * @param granted   whether the game may use it
     */
    public static boolean updatePermission(int sessionId, String scope, boolean granted) {
        if (sessionId >= 0 && scope != null && !scope.isEmpty()) {
            return sPermissionUpdater.update(sessionId, scope, granted);
        }
        return false;
    }

    static void setPermissionUpdaterForTests(PermissionUpdater updater) {
        sPermissionUpdater = updater;
    }

    static void resetPermissionUpdaterForTests() {
        sPermissionUpdater = NATIVE_PERMISSION_UPDATER;
    }

    static void setSessionShutdownForTests(SessionShutdown shutdown) {
        sSessionShutdown = shutdown;
    }

    static void resetSessionShutdownForTests() {
        sSessionShutdown = NATIVE_SESSION_SHUTDOWN;
    }

    // ==================== Ad Callbacks ====================

    /**
     * Deliver one ad event to content.
     * <p>
     * Called from {@link com.migo.runtime.internal.NativeExports}' ad event
     * sink. Every ad event -- load, error, close, resize, hide -- uses this one
     * channel; the payload's {@code adId} routes it to the right ad object.
     * <p>
     * For a rewarded-video {@code close}, the payload's {@code isEnded} is the
     * reward verdict and originates in the host's ad SDK.
     *
     * @param sessionId The session ID
     * @param eventJson JSON: {"adId":N,"event":"close","isEnded":true}
     */
    public static void onAdEvent(int sessionId, String eventJson) {
        if (sessionId >= 0 && eventJson != null) {
            NativeBridge.onAdEvent(sessionId, eventJson);
        }
    }

    // ==================== Auth Callbacks ====================

    /**
     * Callback for login result.
     * Called from {@link com.migo.runtime.internal.NativeExports#authLogin(int, String)}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N,"code":"..."} or {"requestId":N,"error":"reason"}
     */
    public static void onLoginResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onLoginResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for checkSession result.
     * Called from {@link com.migo.runtime.internal.NativeExports#authCheckSession(int, String)}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N} or {"requestId":N,"error":"reason"}
     */
    public static void onCheckSessionResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onCheckSessionResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for getUserInfo result.
     * Called from {@link com.migo.runtime.internal.NativeExports#authGetUserInfo(int, String)}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N,"userInfo":{...}} or {"requestId":N,"error":"reason"}
     */
    public static void onGetUserInfoResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onGetUserInfoResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for getPhoneNumber result.
     * Called from {@link com.migo.runtime.internal.NativeExports#authGetPhoneNumber(int, String)}.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N,"code":"..."} or {"requestId":N,"error":"reason"}
     */
    public static void onGetPhoneNumberResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onGetPhoneNumberResult(sessionId, resultJson);
        }
    }

    // ==================== Subpackage Callbacks ====================

    /**
     * Callback for subpackage download progress.
     * Called from the host app's download implementation.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N,"progress":50,"totalBytesWritten":1024,"totalBytesExpectedToWrite":2048}
     */
    public static void onSubpackageProgress(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onSubpackageProgress(sessionId, resultJson);
        }
    }

    /**
     * Callback when subpackage download completes.
     * Called from the host app's download implementation.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON: {"requestId":N} on success, {"requestId":N,"error":"reason"} on failure
     */
    public static void onSubpackageResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onSubpackageResult(sessionId, resultJson);
        }
    }

    // ==================== Recorder Callbacks ====================

    /**
     * Callback for recorder events.
     * Called from {@link com.migo.runtime.internal.platform.AudioRecorderManager}.
     *
     * @param sessionId   The session ID
     * @param eventType   Event type: "start", "pause", "resume", "stop", "error",
     *                    "interruptionBegin", "interruptionEnd"
     * @param jsonPayload JSON-encoded event data
     */
    public static void onRecorderEvent(
            int sessionId, long generation, String eventType, String jsonPayload) {
        if (sessionId >= 0 && eventType != null) {
            NativeBridge.onRecorderEvent(
                    sessionId, generation, eventType, jsonPayload != null ? jsonPayload : "{}");
        }
    }

    /**
     * Callback for recorder frame data.
     * Called from {@link com.migo.runtime.internal.platform.AudioRecorderManager}.
     *
     * @param sessionId   The session ID
     * @param frameData   Raw audio frame bytes
     * @param frameLength Number of valid bytes at the start of {@code frameData}
     * @param isLastFrame Whether this is the last frame before stop
     */
    public static void onRecorderFrameData(
            int sessionId, long generation, byte[] frameData, int frameLength,
            boolean isLastFrame) {
        if (sessionId >= 0 && frameData != null
                && frameLength >= 0 && frameLength <= frameData.length) {
            NativeBridge.onRecorderFrameData(
                    sessionId, generation, frameData, frameLength, isLastFrame);
        }
    }

    // ==================== Setting Callback ====================

    /**
     * Callback for openSetting result.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON with authSetting
     */
    public static void onOpenSettingResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onOpenSettingResult(sessionId, resultJson);
        }
    }

    // ==================== Share Callback ====================

    /**
     * Callback for shareAppMessage result.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result
     */
    public static void onShareAppMessageResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onShareAppMessageResult(sessionId, resultJson);
        }
    }

    // ==================== Navigate Callback ====================

    /**
     * Callback for navigateToMiniProgram result.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result
     */
    public static void onNavigateToMiniProgramResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onNavigateToMiniProgramResult(sessionId, resultJson);
        }
    }

    // ==================== Video Callbacks ====================

    /**
     * Callback for video player events.
     * Called from {@link com.migo.runtime.internal.platform.VideoManager}.
     *
     * @param sessionId The session ID
     * @param videoId   The video player instance ID
     * @param eventType Event type: "play", "pause", "ended", "error", "timeupdate",
     *                  "progress", "fullscreenchange"
     * @param dataJson  JSON-encoded event data
     */
    public static void onVideoEvent(
            int sessionId, long generation, int videoId, String eventType, String dataJson) {
        try {
            NativeBridge.onVideoEvent(sessionId, generation, videoId,
                eventType != null ? eventType : "",
                dataJson != null ? dataJson : "{}");
        } catch (Exception e) {
            // Ignore callback errors during shutdown
        }
    }

    // ==================== Payment Callbacks ====================

    /**
     * Callback for requestMidasPayment result.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result (should include requestId)
     */
    public static void onMidasPaymentResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onMidasPaymentResult(sessionId, resultJson);
        }
    }

    /**
     * Callback for requestMidasPaymentGameItem result.
     *
     * @param sessionId  The session ID
     * @param resultJson JSON result (should include requestId)
     */
    public static void onMidasPaymentGameItemResult(int sessionId, String resultJson) {
        if (sessionId >= 0 && resultJson != null) {
            NativeBridge.onMidasPaymentGameItemResult(sessionId, resultJson);
        }
    }

    // ==================== Script Execution ====================

    /**
     * Execute a JavaScript snippet in the game's V8 runtime.
     * <p>
     * The script is evaluated asynchronously on the host thread.
     *
     * @param sessionId The session ID
     * @param script    JavaScript source code to evaluate
     */
    public static void executeScript(int sessionId, String script) {
        if (sessionId >= 0 && script != null && !script.isEmpty()) {
            NativeBridge.executeScript(sessionId, script);
        }
    }
}
