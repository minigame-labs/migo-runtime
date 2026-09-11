package com.migo.runtime.internal.platform;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.graphics.SurfaceTexture;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.TotalCaptureResult;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.media.MediaRecorder;
import android.os.Build;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Looper;
import android.util.Log;
import android.util.Size;

import com.migo.runtime.internal.ExclusiveDeviceArbiter;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.RuntimeGenerationBoundary;
import com.migo.runtime.internal.RuntimeScoped;
import com.migo.runtime.internal.ResourceCleanup;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Platform-level camera manager using Camera2 API.
 * <p>
 * Supports photo capture, video recording, zoom control, and real-time frame streaming.
 * Events are sent via {@link NativeMethods#onCameraEvent} and
 * frame data via {@link NativeMethods#onCameraFrameData}.
 *
 * @hide
 */
public final class CameraManager implements RuntimeScoped {

    private static final String TAG = "CameraManager";

    /** Camera states. */
    private static final int STATE_CLOSED = 0;
    private static final int STATE_OPENED = 1;
    private static final int STATE_RECORDING = 2;

    private final int sessionId;
    private final int cameraId;
    private final Activity activity;
    private final Handler mainHandler;
    private final Object initializationLock = new Object();

    // Camera2 objects
    private android.hardware.camera2.CameraManager cameraManager;
    private CameraDevice cameraDevice;
    private CameraCaptureSession captureSession;
    private CaptureRequest.Builder previewRequestBuilder;
    private String hardwareCameraId;

    // Background thread for camera operations
    private HandlerThread backgroundThread;
    private Handler backgroundHandler;

    // Frame streaming
    private ImageReader frameReader;
    private final AtomicBoolean frameListening = new AtomicBoolean(false);

    // Preview surface resources (must be released to avoid native leaks)
    private SurfaceTexture previewTexture;
    private android.view.Surface previewSurface;

    // Photo capture
    private ImageReader photoReader;

    // Video recording
    private MediaRecorder mediaRecorder;
    private String videoFilePath;
    private long recordStartTime;

    // Semaphore to prevent concurrent open/close
    private final Semaphore cameraOpenCloseLock = new Semaphore(1);

    // Semaphore to wait for camera opening to complete
    private final Semaphore cameraOpenCompleteLock = new Semaphore(0);

    private final AtomicInteger state = new AtomicInteger(STATE_CLOSED);
    private final LifecycleRequestState<Boolean> cameraActivityRequest;
    private volatile boolean recordingPausedForLifecycle = false;
    private volatile boolean sessionNeedsReconfigure = false;

    // Configuration
    private String position = "back";   // "back" or "front"
    private String flash = "auto";      // "auto", "on", "off"
    private String sizePreset = "small"; // "small", "medium", "large"
    private float currentZoom = 1.0f;
    private float maxZoom = 1.0f;

    // Resolved sizes
    private Size previewSize;
    private Size photoSize;
    private Size videoSize;

    /**
     * The runtime this camera belongs to.
     *
     * <p>Captured once, here, and stamped on every event and frame it reports.
     * The teardown that releases the camera reports a
     * {@code stop} as it goes, and that report belongs to the isolate that
     * opened it, not to the one replacing it.
     */
    private final RuntimeGenerationBoundary.Token token;

    @Override
    public RuntimeGenerationBoundary.Token runtimeToken() {
        return token;
    }

    public CameraManager(int sessionId, int cameraId, Activity activity) {
        this(sessionId, cameraId, activity, false);
    }

    public CameraManager(
            int sessionId,
            int cameraId,
            Activity activity,
            boolean lifecycleSuspended) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.cameraId = cameraId;
        this.activity = activity;
        this.mainHandler = new Handler(Looper.getMainLooper());
        this.cameraActivityRequest = new LifecycleRequestState<>(lifecycleSuspended);
    }

    /**
     * Create and open the camera with the given options.
     *
     * @param optionsJson JSON with keys: x, y, width, height, devicePosition, flash, size
     * @return JSON result string: {"cameraId": <id>}
     */
    public String create(String optionsJson) {
        Log.d(TAG, "create() called with optionsJson: " + optionsJson);
        cameraOpenCompleteLock.drainPermits(); // Clear any stale permits
        parseOptions(optionsJson);
        Log.d(TAG, "create() parsed options - position: " + position + ", flash: " + flash + ", size: " + sizePreset);

        if (activity.checkSelfPermission(Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            Log.w(TAG, "create() camera permission denied");
            fireEvent("authCancel", "{}");
            return errorJson("createCamera:fail auth deny");
        }
        // Keyed by position, not by "camera": a device has several physical cameras
        // and two games using different ones is legitimate.
        if (!ExclusiveDeviceArbiter.tryAcquire(
                ExclusiveDeviceArbiter.camera(position), sessionId)) {
            return errorJson("createCamera:fail in use by another game");
        }
        Log.d(TAG, "create() permission check passed");

        cameraManager = (android.hardware.camera2.CameraManager)
                activity.getSystemService(Context.CAMERA_SERVICE);
        if (cameraManager == null) {
            Log.e(TAG, "create() failed to get camera service");
            return errorJson("createCamera:fail camera service unavailable");
        }
        Log.d(TAG, "create() got camera manager");

        try {
            hardwareCameraId = findCameraId();
            Log.d(TAG, "create() found hardwareCameraId: " + hardwareCameraId);
            if (hardwareCameraId == null) {
                Log.e(TAG, "create() no camera found for position: " + position);
                return errorJson("createCamera:fail no camera found for position: " + position);
            }

            resolveSizes();
            Log.d(TAG, "create() resolved sizes - preview: " + previewSize + ", photo: " + photoSize + ", video: " + videoSize);
            synchronized (initializationLock) {
                if (cameraActivityRequest.isDestroyed()) {
                    return errorJson("createCamera:fail session destroyed");
                }
                startBackgroundThread();
                Log.d(TAG, "create() started background thread");

                LifecycleRequestState.Action action =
                        cameraActivityRequest.requestStart(Boolean.TRUE);
                if (action == LifecycleRequestState.Action.NONE) {
                    if (cameraActivityRequest.isDestroyed()) {
                        return errorJson("createCamera:fail session destroyed");
                    }
                    Log.d(TAG, "create() deferred camera open while lifecycle-suspended");
                    return createResultJson();
                }

                if (cameraActivityRequest.isDestroyed()) {
                    return errorJson("createCamera:fail session destroyed");
                }
                openCamera();
                Log.d(TAG, "create() called openCamera()");
            }

            // Wait for camera to be fully opened and preview session configured
            Log.d(TAG, "create() waiting for camera open to complete...");
            boolean opened = cameraOpenCompleteLock.tryAcquire(5000, TimeUnit.MILLISECONDS);
            if (!opened) {
                Log.w(TAG, "create() timeout waiting for camera open");
                cameraActivityRequest.startFailed(false);
                return errorJson("createCamera:fail timeout waiting for camera open");
            }
            Log.d(TAG, "create() camera open completed");

            Log.d(TAG, "create() returning successfully with cameraId: " + cameraId);
            return createResultJson();
        } catch (Exception e) {
            cameraActivityRequest.startFailed(false);
            Log.e(TAG, "create() exception: " + e.getMessage(), e);
            return errorJson("createCamera:fail " + e.getMessage());
        }
    }

    /**
     * Destroy the camera and release all resources.
     */
    public void destroy() {
        Log.d(TAG, "destroy() called");
        cameraActivityRequest.destroy();
        synchronized (initializationLock) {
            boolean wasRecording = state.getAndSet(STATE_CLOSED) == STATE_RECORDING;
            ResourceCleanup.runAll(
                    () -> closeCamera(wasRecording),
                    this::stopBackgroundThread,
                    () -> ExclusiveDeviceArbiter.release(
                            ExclusiveDeviceArbiter.camera(position), sessionId));
        }
        fireEvent("stop", "{}");
        Log.d(TAG, "destroy() completed");
    }

    public synchronized void setLifecycleSuspended(boolean suspended) {
        if (suspended) {
            suspendForLifecycle();
        } else {
            resumeForLifecycle();
        }
    }

    public synchronized void suspendForLifecycle() {
        if (cameraActivityRequest.suspend() == LifecycleRequestState.Action.STOP) {
            postCameraAction(this::suspendCameraInternal);
        }
    }

    public synchronized void resumeForLifecycle() {
        if (cameraActivityRequest.resume() == LifecycleRequestState.Action.START) {
            postCameraAction(this::resumeCameraInternal);
        }
    }

    private void postCameraAction(Runnable action) {
        Handler handler = backgroundHandler;
        if (handler != null) {
            handler.post(action);
        }
    }

    private void suspendCameraInternal() {
        if (cameraActivityRequest.isDestroyed()) return;

        if (state.get() == STATE_RECORDING
                && mediaRecorder != null
                && !recordingPausedForLifecycle) {
            try {
                mediaRecorder.pause();
                recordingPausedForLifecycle = true;
            } catch (RuntimeException e) {
                Log.w(TAG, "Failed to pause camera recording for lifecycle: " + e.getMessage());
            }
        }

        if (captureSession != null) {
            try {
                captureSession.stopRepeating();
            } catch (Exception e) {
                Log.w(TAG, "Failed to stop camera repeating request: " + e.getMessage());
            }
        }
    }

    private void resumeCameraInternal() {
        if (!cameraActivityRequest.isActive() || cameraActivityRequest.isDestroyed()) return;

        if (cameraDevice == null) {
            cameraOpenCompleteLock.drainPermits();
            try {
                openCamera();
            } catch (Exception e) {
                cameraActivityRequest.startFailed(true);
                fireEvent("error", "{\"errMsg\":\""
                        + escapeJson("camera resume:fail " + e.getMessage()) + "\"}");
            }
            return;
        }

        if (state.get() != STATE_RECORDING && sessionNeedsReconfigure) {
            try {
                closeSession();
                if (frameListening.get()) {
                    createFrameReader();
                } else {
                    closeFrameReader();
                }
                startPreviewSession();
            } catch (Exception e) {
                fireEvent("error", "{\"errMsg\":\""
                        + escapeJson("camera resume:fail " + e.getMessage()) + "\"}");
            }
            return;
        }

        if (captureSession == null) {
            if (state.get() == STATE_OPENED) {
                try {
                    startPreviewSession();
                } catch (Exception e) {
                    fireEvent("error", "{\"errMsg\":\""
                            + escapeJson("camera resume:fail " + e.getMessage()) + "\"}");
                }
            }
            return;
        }

        try {
            captureSession.setRepeatingRequest(
                    previewRequestBuilder.build(), null, backgroundHandler);
            if (state.get() == STATE_RECORDING
                    && mediaRecorder != null
                    && recordingPausedForLifecycle) {
                mediaRecorder.resume();
                recordingPausedForLifecycle = false;
            }
        } catch (Exception e) {
            fireEvent("error", "{\"errMsg\":\""
                    + escapeJson("camera resume:fail " + e.getMessage()) + "\"}");
        }
    }

    /**
     * Submit a photo capture and return immediately.
     *
     * <p>The old implementation waited on a latch here, which put the Java
     * callback's disk write on the V8 thread.  The request id is carried in
     * the eventual {@code takePhotoResult} event so a restarted or destroyed
     * camera cannot settle a different request.
     */
    public void takePhotoAsync(final int requestId, String optionsJson) {
        Log.d(TAG, "takePhotoAsync() called with optionsJson: " + optionsJson);
        if (!cameraActivityRequest.isActive()) {
            firePhotoResult(requestId, errorJson("camera.takePhoto:fail camera lifecycle suspended"));
            return;
        }
        if (state.get() == STATE_CLOSED || captureSession == null || cameraDevice == null) {
            firePhotoResult(requestId, errorJson("camera.takePhoto:fail camera not ready"));
            return;
        }

        String quality = "normal";
        try {
            JSONObject opts = new JSONObject(optionsJson);
            quality = opts.optString("quality", "normal");
        } catch (JSONException ignored) {
            // Keep the public default for malformed optional quality.
        }

        final int jpegQuality;
        switch (quality) {
            case "high": jpegQuality = 95; break;
            case "low": jpegQuality = 60; break;
            default: jpegQuality = 80; break;
        }

        try {
            if (photoReader == null) {
                firePhotoResult(requestId,
                        errorJson("camera.takePhoto:fail photoReader not initialized"));
                return;
            }

            final String tempPath = createTempFilePath("photo", ".jpg");
            final int finalJpegQuality = jpegQuality;
            photoReader.setOnImageAvailableListener(reader -> {
                Image image = null;
                try {
                    image = reader.acquireLatestImage();
                    if (image == null) {
                        firePhotoResult(requestId,
                                errorJson("camera.takePhoto:fail no image"));
                        return;
                    }
                    ByteBuffer buffer = image.getPlanes()[0].getBuffer();
                    byte[] bytes = new byte[buffer.remaining()];
                    buffer.get(bytes);
                    try (FileOutputStream fos = new FileOutputStream(tempPath)) {
                        fos.write(bytes);
                    }

                    JSONObject result = new JSONObject();
                    result.put("requestId", requestId);
                    result.put("tempImagePath", tempPath);
                    result.put("width", photoSize.getWidth());
                    result.put("height", photoSize.getHeight());
                    fireEvent("takePhotoResult", result.toString());
                } catch (Exception e) {
                    new File(tempPath).delete();
                    firePhotoResult(requestId,
                            errorJson("camera.takePhoto:fail " + e.getMessage()));
                } finally {
                    if (image != null) image.close();
                }
            }, backgroundHandler);

            CaptureRequest.Builder captureBuilder =
                    cameraDevice.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE);
            captureBuilder.addTarget(photoReader.getSurface());
            captureBuilder.set(CaptureRequest.JPEG_QUALITY, (byte) finalJpegQuality);
            applyFlashMode(captureBuilder);
            applyZoom(captureBuilder);
            captureSession.capture(captureBuilder.build(),
                    new CameraCaptureSession.CaptureCallback() {
                        @Override
                        public void onCaptureCompleted(CameraCaptureSession session,
                                                       CaptureRequest request,
                                                       TotalCaptureResult result) {
                            Log.d(TAG, "takePhotoAsync() capture completed");
                        }
                    }, backgroundHandler);
        } catch (Exception e) {
            firePhotoResult(requestId,
                    errorJson("camera.takePhoto:fail " + e.getMessage()));
        }
    }

    private void firePhotoResult(int requestId, String payload) {
        try {
            JSONObject result = new JSONObject(payload);
            result.put("requestId", requestId);
            fireEvent("takePhotoResult", result.toString());
        } catch (JSONException e) {
            fireEvent("takePhotoResult",
                    "{\"requestId\":" + requestId + ",\"_error\":{\"errMsg\":\"camera.takePhoto:fail result\"}}");
        }
    }

    /**
     * Start video recording.
     *
     * @param optionsJson JSON with keys: cameraId
     * @return JSON result: {} on success
     */
    public String startRecord(String optionsJson) {
        Log.d(TAG, "startRecord() called");
        if (!cameraActivityRequest.isActive()) {
            return errorJson("camera.startRecord:fail camera lifecycle suspended");
        }
        if (state.get() != STATE_OPENED || cameraDevice == null) {
            Log.e(TAG, "startRecord() camera not ready - state: " + state.get() + ", cameraDevice: " + (cameraDevice != null));
            return errorJson("camera.startRecord:fail camera not ready");
        }
        Log.d(TAG, "startRecord() camera ready, starting recording");

        try {
            closeSession();

            videoFilePath = createTempFilePath("video", ".mp4");
            setupMediaRecorder();
            recordingPausedForLifecycle = false;

            // Create recording session with preview + mediaRecorder surfaces
            List<android.view.Surface> surfaces = new ArrayList<>();
            android.view.Surface recorderSurface = mediaRecorder.getSurface();
            surfaces.add(recorderSurface);

            if (frameListening.get() && frameReader != null) {
                surfaces.add(frameReader.getSurface());
            }

            previewRequestBuilder = cameraDevice.createCaptureRequest(CameraDevice.TEMPLATE_RECORD);
            previewRequestBuilder.addTarget(recorderSurface);

            if (frameListening.get() && frameReader != null) {
                previewRequestBuilder.addTarget(frameReader.getSurface());
            }

            applyFlashMode(previewRequestBuilder);
            applyZoom(previewRequestBuilder);

            cameraDevice.createCaptureSession(surfaces,
                    new CameraCaptureSession.StateCallback() {
                        @Override
                        public void onConfigured(CameraCaptureSession session) {
                            captureSession = session;
                            if (cameraActivityRequest.isDestroyed()) {
                                session.close();
                                return;
                            }
                            try {
                                boolean suspendAfterStart = !cameraActivityRequest.isActive();
                                captureSession.setRepeatingRequest(
                                        previewRequestBuilder.build(), null, backgroundHandler);
                                mediaRecorder.start();
                                state.set(STATE_RECORDING);
                                recordStartTime = System.currentTimeMillis();
                                if (suspendAfterStart) {
                                    mediaRecorder.pause();
                                    recordingPausedForLifecycle = true;
                                    captureSession.stopRepeating();
                                }
                            } catch (Exception e) {
                                fireEvent("error",
                                        "{\"errMsg\":\"" + escapeJson("camera.startRecord:fail " + e.getMessage()) + "\"}");
                            }
                        }

                        @Override
                        public void onConfigureFailed(CameraCaptureSession session) {
                            fireEvent("error",
                                    "{\"errMsg\":\"camera.startRecord:fail session config failed\"}");
                        }
                    }, backgroundHandler);

            return "{}";
        } catch (Exception e) {
            return errorJson("camera.startRecord:fail " + e.getMessage());
        }
    }

    /**
     * Stop video recording.
     *
     * @param optionsJson JSON with keys: cameraId, compressed (boolean)
     * @return JSON result: {"tempThumbPath": "", "tempVideoPath": "<path>"}
     */
    public String stopRecord(String optionsJson) {
        Log.d(TAG, "stopRecord() called");
        if (state.get() != STATE_RECORDING) {
            Log.e(TAG, "stopRecord() not recording, state: " + state.get());
            return errorJson("camera.stopRecord:fail not recording");
        }

        return stopRecordInternal();
    }

    /**
     * Set zoom level.
     *
     * @param optionsJson JSON with keys: zoom (number)
     * @return JSON result: {"zoom": <actual_zoom>}
     */
    public synchronized String setZoom(String optionsJson) {
        Log.d(TAG, "setZoom() called with optionsJson: " + optionsJson);
        if (state.get() == STATE_CLOSED && !cameraActivityRequest.isRequested()) {
            Log.e(TAG, "setZoom() camera not ready");
            return errorJson("camera.setZoom:fail camera not ready");
        }

        float zoom = 1.0f;
        try {
            JSONObject opts = new JSONObject(optionsJson);
            zoom = (float) opts.optDouble("zoom", 1.0);
        } catch (JSONException ignored) {}

        currentZoom = Math.max(1.0f, Math.min(zoom, maxZoom));
        Log.d(TAG, "setZoom() requested: " + zoom + ", actual: " + currentZoom + ", maxZoom: " + maxZoom);

        if (cameraActivityRequest.isActive()
                && previewRequestBuilder != null
                && captureSession != null) {
            try {
                applyZoom(previewRequestBuilder);
                captureSession.setRepeatingRequest(
                        previewRequestBuilder.build(), null, backgroundHandler);
                Log.d(TAG, "setZoom() applied to capture session");
            } catch (Exception e) {
                Log.e(TAG, "setZoom() exception: " + e.getMessage(), e);
                return errorJson("camera.setZoom:fail " + e.getMessage());
            }
        } else {
            Log.w(TAG, "setZoom() previewRequestBuilder or captureSession not ready");
        }

        try {
            JSONObject result = new JSONObject();
            result.put("zoom", currentZoom);
            return result.toString();
        } catch (JSONException e) {
            return "{\"zoom\":" + currentZoom + "}";
        }
    }

    /**
     * Start listening for camera frame changes.
     */
    public void listenFrameChange() {
        Log.d(TAG, "listenFrameChange() called");
        if (frameListening.getAndSet(true)) {
            Log.d(TAG, "listenFrameChange() already listening");
            return; // already listening
        }

        if (!cameraActivityRequest.isActive()
                || state.get() == STATE_CLOSED
                || cameraDevice == null) {
            Log.w(TAG, "listenFrameChange() camera not ready, state: " + state.get());
            sessionNeedsReconfigure = true;
            return;
        }

        try {
            createFrameReader();
            restartPreviewSession();
            Log.d(TAG, "listenFrameChange() started successfully");
        } catch (Exception e) {
            Log.e(TAG, "listenFrameChange() exception: " + e.getMessage(), e);
            fireEvent("error",
                    "{\"errMsg\":\"" + escapeJson("camera.listenFrameChange:fail " + e.getMessage()) + "\"}");
        }
    }

    /**
     * Stop listening for camera frame changes.
     */
    public void closeFrameChange() {
        Log.d(TAG, "closeFrameChange() called");
        if (!frameListening.getAndSet(false)) {
            Log.d(TAG, "closeFrameChange() was not listening");
            return;
        }

        if (cameraActivityRequest.isActive()) {
            try {
                restartPreviewSession();
            } catch (Exception e) {
                Log.w(TAG, "closeFrameChange() restart preview failed: " + e.getMessage());
                // Best effort
            }
        } else {
            sessionNeedsReconfigure = true;
        }

        closeFrameReader();
        Log.d(TAG, "closeFrameChange() completed");
    }

    // ========================================================================
    // Camera2 internals
    // ========================================================================

    // Slim intentionally omits CAMERA from its manifest; the check remains
    // necessary for revocation after create() validated the original grant.
    @SuppressLint("MissingPermission")
    private void openCamera() throws CameraAccessException, InterruptedException {
        Log.d(TAG, "openCamera() called");
        // The grant may have been revoked since create() checked it.
        if (activity.checkSelfPermission(Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            fireEvent("authCancel", "{}");
            throw new CameraAccessException(
                    CameraAccessException.CAMERA_ERROR, "Camera permission denied");
        }
        if (!cameraOpenCloseLock.tryAcquire(2500, TimeUnit.MILLISECONDS)) {
            Log.e(TAG, "openCamera() timeout waiting for camera lock");
            throw new RuntimeException("Timeout waiting to acquire camera lock");
        }
        Log.d(TAG, "openCamera() acquired camera lock");

        try {
            Log.d(TAG, "openCamera() calling cameraManager.openCamera() with hardwareCameraId: " + hardwareCameraId);
            cameraManager.openCamera(hardwareCameraId, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    Log.d(TAG, "openCamera() onOpened callback called");
                    cameraOpenCloseLock.release();
                    cameraDevice = camera;
                    state.set(STATE_OPENED);
                    Log.d(TAG, "openCamera() camera opened successfully, state set to OPENED");

                    try {
                        if (cameraActivityRequest.isDestroyed()
                                || !cameraActivityRequest.isRequested()) {
                            camera.close();
                            cameraDevice = null;
                            state.set(STATE_CLOSED);
                            cameraOpenCompleteLock.release();
                            return;
                        }
                        if (!cameraActivityRequest.isActive()) {
                            cameraOpenCompleteLock.release();
                            return;
                        }
                        if (frameListening.get()) {
                            Log.d(TAG, "openCamera() frame listening is enabled, creating frame reader");
                            createFrameReader();
                        } else {
                            Log.d(TAG, "openCamera() frame listening is disabled");
                        }
                        Log.d(TAG, "openCamera() starting preview session");
                        startPreviewSession();
                        Log.d(TAG, "openCamera() preview session started successfully");
                    } catch (Exception e) {
                        Log.e(TAG, "openCamera() onOpened exception: " + e.getMessage(), e);
                        cameraOpenCompleteLock.release();
                        fireEvent("error",
                                "{\"errMsg\":\"" + escapeJson("camera open:fail " + e.getMessage()) + "\"}");
                    }
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    Log.w(TAG, "openCamera() onDisconnected callback called");
                    cameraOpenCloseLock.release();
                    cameraOpenCompleteLock.release();
                    camera.close();
                    cameraDevice = null;
                    state.set(STATE_CLOSED);
                    cameraActivityRequest.startFailed(true);
                    fireEvent("stop", "{}");
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    Log.e(TAG, "openCamera() onError callback called with error code: " + error);
                    cameraOpenCloseLock.release();
                    cameraOpenCompleteLock.release();
                    camera.close();
                    cameraDevice = null;
                    state.set(STATE_CLOSED);
                    cameraActivityRequest.startFailed(true);
                    fireEvent("error", "{\"errMsg\":\"camera device error: " + error + "\"}");
                }
            }, backgroundHandler);
            Log.d(TAG, "openCamera() openCamera() call completed");
        } catch (SecurityException e) {
            Log.e(TAG, "openCamera() SecurityException: " + e.getMessage(), e);
            cameraOpenCloseLock.release();
            cameraOpenCompleteLock.release();
            fireEvent("authCancel", "{}");
            throw new CameraAccessException(CameraAccessException.CAMERA_ERROR,
                    "Camera permission denied");
        }
    }

    private void closeCamera(boolean stopRecorder) {
        Log.d(TAG, "closeCamera() called");
        recordingPausedForLifecycle = false;
        sessionNeedsReconfigure = false;
        boolean acquired;
        try {
            acquired = cameraOpenCloseLock.tryAcquire(2500, TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException("interrupted while closing camera", e);
        }
        if (!acquired) throw new IllegalStateException("timed out waiting to close camera");

        try {
            ResourceCleanup.runAll(
                    () -> releaseMediaRecorder(stopRecorder),
                    this::closeSession,
                    this::closeCameraDevice,
                    this::closeFrameReader,
                    this::closePhotoReader);
            Log.d(TAG, "closeCamera() all resources released");
        } finally {
            cameraOpenCloseLock.release();
        }
    }

    private void releaseMediaRecorder(boolean stopRecorder) {
        MediaRecorder recorder = mediaRecorder;
        if (recorder == null) return;
        ResourceCleanup.runAll(
                () -> {
                    if (stopRecorder) recorder.stop();
                },
                recorder::reset,
                () -> {
                    recorder.release();
                    if (mediaRecorder == recorder) mediaRecorder = null;
                });
    }

    private void closeCameraDevice() {
        CameraDevice device = cameraDevice;
        if (device == null) return;
        device.close();
        if (cameraDevice == device) cameraDevice = null;
    }

    private void closePhotoReader() {
        ImageReader reader = photoReader;
        if (reader == null) return;
        reader.close();
        if (photoReader == reader) photoReader = null;
    }

    private void closeSession() {
        CameraCaptureSession session = captureSession;
        android.view.Surface surface = previewSurface;
        SurfaceTexture texture = previewTexture;
        ResourceCleanup.runAll(
                () -> {
                    if (session != null) {
                        session.close();
                        if (captureSession == session) captureSession = null;
                    }
                },
                () -> {
                    if (surface != null) {
                        surface.release();
                        if (previewSurface == surface) previewSurface = null;
                    }
                },
                () -> {
                    if (texture != null) {
                        texture.release();
                        if (previewTexture == texture) previewTexture = null;
                    }
                });
    }

    private void startPreviewSession() throws CameraAccessException {
        Log.d(TAG, "startPreviewSession() called");
        if (cameraDevice == null) {
            Log.e(TAG, "startPreviewSession() cameraDevice is null, returning");
            return;
        }

        List<android.view.Surface> surfaces = new ArrayList<>();

        // Use a dummy SurfaceTexture for preview (we don't render preview on screen)
        // Release previous preview resources if any
        if (previewSurface != null) {
            previewSurface.release();
        }
        if (previewTexture != null) {
            previewTexture.release();
        }
        previewTexture = new SurfaceTexture(0);
        previewTexture.setDefaultBufferSize(previewSize.getWidth(), previewSize.getHeight());
        previewSurface = new android.view.Surface(previewTexture);
        surfaces.add(previewSurface);
        Log.d(TAG, "startPreviewSession() created preview surface");

        // Pre-create photoReader so its surface is registered in the session
        if (photoReader != null) {
            photoReader.close();
        }
        photoReader = ImageReader.newInstance(
                photoSize.getWidth(), photoSize.getHeight(),
                ImageFormat.JPEG, 1);
        surfaces.add(photoReader.getSurface());
        Log.d(TAG, "startPreviewSession() created photo reader surface");

        previewRequestBuilder = cameraDevice.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
        previewRequestBuilder.addTarget(previewSurface);

        if (frameListening.get() && frameReader != null) {
            Log.d(TAG, "startPreviewSession() adding frame reader surface");
            surfaces.add(frameReader.getSurface());
            previewRequestBuilder.addTarget(frameReader.getSurface());
        }

        applyFlashMode(previewRequestBuilder);
        applyZoom(previewRequestBuilder);
        Log.d(TAG, "startPreviewSession() applied flash and zoom settings");

        Log.d(TAG, "startPreviewSession() calling createCaptureSession with " + surfaces.size() + " surfaces");
        cameraDevice.createCaptureSession(surfaces,
                new CameraCaptureSession.StateCallback() {
                    @Override
                    public void onConfigured(CameraCaptureSession session) {
                        Log.d(TAG, "startPreviewSession() onConfigured callback called");
                        if (cameraDevice == null) {
                            Log.e(TAG, "startPreviewSession() onConfigured cameraDevice is null");
                            cameraOpenCompleteLock.release();
                            return;
                        }
                        captureSession = session;
                        sessionNeedsReconfigure = false;
                        if (!cameraActivityRequest.isActive()) {
                            Log.d(TAG, "startPreviewSession() configured while suspended");
                            cameraOpenCompleteLock.release();
                            return;
                        }
                        try {
                            Log.d(TAG, "startPreviewSession() setting repeating request");
                            captureSession.setRepeatingRequest(
                                    previewRequestBuilder.build(), null, backgroundHandler);
                            Log.d(TAG, "startPreviewSession() repeating request set successfully");
                            Log.d(TAG, "startPreviewSession() onConfigured completed, releasing lock");
                            cameraOpenCompleteLock.release();
                        } catch (Exception e) {
                            Log.e(TAG, "startPreviewSession() onConfigured exception: " + e.getMessage(), e);
                            cameraOpenCompleteLock.release();
                            fireEvent("error",
                                    "{\"errMsg\":\"" + escapeJson("preview:fail " + e.getMessage()) + "\"}");
                        }
                    }

                    @Override
                    public void onConfigureFailed(CameraCaptureSession session) {
                        Log.e(TAG, "startPreviewSession() onConfigureFailed callback called");
                        sessionNeedsReconfigure = true;
                        cameraOpenCompleteLock.release();
                        fireEvent("error", "{\"errMsg\":\"preview session config failed\"}");
                    }
                }, backgroundHandler);
        Log.d(TAG, "startPreviewSession() createCaptureSession call completed");
    }

    private void restartPreviewSession() throws CameraAccessException {
        if (state.get() == STATE_RECORDING) {
            // Don't restart preview during recording
            return;
        }
        closeSession();
        startPreviewSession();
    }

    /**
     * Close and clear {@link #frameReader} under the CameraManager monitor.
     *
     * <p>Holding this monitor is what makes reader teardown safe against the
     * frame listener, not merely lifecycle-event delivery: after
     * {@link ImageReader#close()} any previously-acquired {@link Image} and its
     * {@link Image.Plane} direct {@code ByteBuffer}s are undefined (per the
     * ImageReader docs), and Q8's native path reads that plane memory in JNI.
     * The listener holds the same monitor across acquire/read/close, so a close
     * routed through here can never invalidate a reader/Image mid-read. Every
     * {@code frameReader.close()} must go through this helper.
     */
    private synchronized void closeFrameReader() {
        ImageReader reader = frameReader;
        if (reader == null) return;
        reader.close();
        if (frameReader == reader) frameReader = null;
    }

    private synchronized void createFrameReader() {
        // Close/replace/configure the reader under the same monitor the listener
        // uses, so the swap is atomic with respect to frame delivery.
        closeFrameReader();

        // Use YUV_420_888 for frame data - widely supported and efficient
        frameReader = ImageReader.newInstance(
                previewSize.getWidth(), previewSize.getHeight(),
                ImageFormat.YUV_420_888, 2);

        frameReader.setOnImageAvailableListener(reader -> {
            // The CameraManager monitor is held across acquire -> plane read ->
            // JNI -> Image close so that no concurrent frameReader close (via
            // closeFrameReader, used by destroy/closeCamera/closeFrameChange/
            // resume/createFrameReader) can invalidate the reader or the
            // acquired Image's direct plane ByteBuffers while native code reads
            // them. This guards direct-buffer USE, not just event delivery.
            synchronized (CameraManager.this) {
                // A stale/replaced reader may still fire; only serve the current
                // frameReader.
                if (reader != frameReader) return;
                if (!frameListening.get() || !cameraActivityRequest.isActive()) return;

                Image image = null;
                try {
                    image = reader.acquireLatestImage();
                    if (image == null) return;

                    // YUV_420_888 must expose exactly three planes; drop
                    // malformed vendor output rather than indexing blindly.
                    Image.Plane[] planes = image.getPlanes();
                    if (planes.length != 3) {
                        Log.w(TAG, "onImageAvailable: expected 3 planes, got " + planes.length);
                        return;
                    }

                    // Pass each plane's direct ByteBuffer by reference plus its
                    // [position, position+remaining) window; native code copies
                    // the windows once, concatenating them in Y/U/V order.
                    // position()/remaining() are pure reads (no get()), so the
                    // window matches position..limit and the planes are untouched.
                    ByteBuffer yBuffer = planes[0].getBuffer();
                    ByteBuffer uBuffer = planes[1].getBuffer();
                    ByteBuffer vBuffer = planes[2].getBuffer();

                    int yOff = yBuffer.position();
                    int yLen = yBuffer.remaining();
                    int uOff = uBuffer.position();
                    int uLen = uBuffer.remaining();
                    int vOff = vBuffer.position();
                    int vLen = vBuffer.remaining();

                    // Synchronous native copy while the Image is still open.
                    NativeMethods.onCameraFrameData(sessionId, token.generation(), cameraId,
                            yBuffer, yOff, yLen,
                            uBuffer, uOff, uLen,
                            vBuffer, vOff, vLen,
                            image.getWidth(), image.getHeight());
                } finally {
                    // Close the Image BEFORE releasing the monitor so no closer
                    // holding the same monitor can observe a half-consumed reader.
                    if (image != null) image.close();
                }
            }
        }, backgroundHandler);
    }

    // ========================================================================
    // Video recording internals
    // ========================================================================

    private void setupMediaRecorder() throws IOException {
        mediaRecorder = new MediaRecorder();
        mediaRecorder.setAudioSource(MediaRecorder.AudioSource.MIC);
        mediaRecorder.setVideoSource(MediaRecorder.VideoSource.SURFACE);
        mediaRecorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
        mediaRecorder.setOutputFile(videoFilePath);
        mediaRecorder.setVideoEncodingBitRate(10_000_000);
        mediaRecorder.setVideoFrameRate(30);
        mediaRecorder.setVideoSize(videoSize.getWidth(), videoSize.getHeight());
        mediaRecorder.setVideoEncoder(MediaRecorder.VideoEncoder.H264);
        mediaRecorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);

        // Handle front camera orientation
        if ("front".equals(position)) {
            mediaRecorder.setOrientationHint(270);
        } else {
            mediaRecorder.setOrientationHint(90);
        }

        mediaRecorder.prepare();
    }

    private String stopRecordInternal() {
        Log.d(TAG, "stopRecordInternal() called");
        if (state.get() != STATE_RECORDING) {
            Log.e(TAG, "stopRecordInternal() not recording");
            return errorJson("camera.stopRecord:fail not recording");
        }

        state.set(STATE_OPENED);
        recordingPausedForLifecycle = false;
        Log.d(TAG, "stopRecordInternal() state set to OPENED");

        try {
            captureSession.stopRepeating();
            Log.d(TAG, "stopRecordInternal() stopped repeating request");
        } catch (Exception e) {
            Log.w(TAG, "stopRecordInternal() error stopping repeating: " + e.getMessage());
        }

        try {
            mediaRecorder.stop();
            Log.d(TAG, "stopRecordInternal() media recorder stopped");
        } catch (Exception e) {
            Log.w(TAG, "stopRecordInternal() error stopping media recorder: " + e.getMessage());
        }

        mediaRecorder.reset();
        mediaRecorder.release();
        mediaRecorder = null;

        // Restart preview
        try {
            sessionNeedsReconfigure = false;
            startPreviewSession();
            Log.d(TAG, "stopRecordInternal() preview session restarted");
        } catch (Exception e) {
            Log.w(TAG, "stopRecordInternal() error restarting preview: " + e.getMessage());
            // Best effort
        }

        try {
            JSONObject result = new JSONObject();
            result.put("tempThumbPath", "");
            result.put("tempVideoPath", videoFilePath);
            Log.d(TAG, "stopRecordInternal() returning: " + result.toString());
            return result.toString();
        } catch (JSONException e) {
            return "{\"tempThumbPath\":\"\",\"tempVideoPath\":\"" + escapeJson(videoFilePath) + "\"}";
        }
    }

    // ========================================================================
    // Camera configuration helpers
    // ========================================================================

    private void parseOptions(String json) {
        if (json == null || json.isEmpty()) return;
        try {
            JSONObject opts = new JSONObject(json);
            position = opts.optString("devicePosition", "back");
            flash = opts.optString("flash", "auto");
            sizePreset = opts.optString("size", "small");
        } catch (JSONException ignored) {}
    }

    private String findCameraId() throws CameraAccessException {
        Log.d(TAG, "findCameraId() called, looking for position: " + position);
        int targetFacing = "front".equals(position)
                ? CameraCharacteristics.LENS_FACING_FRONT
                : CameraCharacteristics.LENS_FACING_BACK;
        Log.d(TAG, "findCameraId() target facing: " + targetFacing);

        String[] cameraIdList = cameraManager.getCameraIdList();
        Log.d(TAG, "findCameraId() available cameras: " + cameraIdList.length);
        for (String id : cameraIdList) {
            Log.d(TAG, "findCameraId() checking camera id: " + id);
            CameraCharacteristics chars = cameraManager.getCameraCharacteristics(id);
            Integer facing = chars.get(CameraCharacteristics.LENS_FACING);
            Log.d(TAG, "findCameraId() camera " + id + " facing: " + facing);
            if (facing != null && facing == targetFacing) {
                Log.d(TAG, "findCameraId() found matching camera: " + id);
                return id;
            }
        }
        Log.w(TAG, "findCameraId() no matching camera found for position: " + position);
        return null;
    }

    private void resolveSizes() throws CameraAccessException {
        Log.d(TAG, "resolveSizes() called");
        CameraCharacteristics chars =
                cameraManager.getCameraCharacteristics(hardwareCameraId);

        // Get max zoom
        Float maxDigitalZoom = chars.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM);
        maxZoom = maxDigitalZoom != null ? maxDigitalZoom : 1.0f;
        Log.d(TAG, "resolveSizes() max zoom: " + maxZoom);

        StreamConfigurationMap map = chars.get(
                CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
        if (map == null) {
            Log.e(TAG, "resolveSizes() stream configuration map is null");
            throw new CameraAccessException(CameraAccessException.CAMERA_ERROR,
                    "No stream configuration map");
        }
        Log.d(TAG, "resolveSizes() got stream configuration map");

        // Determine target sizes based on preset
        Size[] jpegSizes = map.getOutputSizes(ImageFormat.JPEG);
        Size[] yuvSizes = map.getOutputSizes(ImageFormat.YUV_420_888);
        Log.d(TAG, "resolveSizes() JPEG sizes available: " + jpegSizes.length + ", YUV sizes available: " + yuvSizes.length);

        photoSize = choosePhotoSize(jpegSizes);
        previewSize = choosePreviewSize(yuvSizes);
        videoSize = chooseVideoSize(map.getOutputSizes(MediaRecorder.class));
        Log.d(TAG, "resolveSizes() selected - photo: " + photoSize + ", preview: " + previewSize + ", video: " + videoSize);
    }

    private Size choosePhotoSize(Size[] sizes) {
        if (sizes == null || sizes.length == 0) {
            return new Size(1920, 1080);
        }

        int targetPixels;
        switch (sizePreset) {
            case "small":  targetPixels = 640 * 480; break;
            case "large":  targetPixels = 3840 * 2160; break;
            default:       targetPixels = 1920 * 1080; break;
        }

        return chooseClosestSize(sizes, targetPixels);
    }

    private Size choosePreviewSize(Size[] sizes) {
        if (sizes == null || sizes.length == 0) {
            return new Size(1280, 720);
        }

        // For frame streaming, use moderate resolution for performance
        int targetPixels;
        switch (sizePreset) {
            case "small":  targetPixels = 320 * 240; break;
            case "large":  targetPixels = 1920 * 1080; break;
            default:       targetPixels = 1280 * 720; break;
        }

        return chooseClosestSize(sizes, targetPixels);
    }

    private Size chooseVideoSize(Size[] sizes) {
        if (sizes == null || sizes.length == 0) {
            return new Size(1920, 1080);
        }

        // Prefer 1080p for video
        int targetPixels = 1920 * 1080;
        return chooseClosestSize(sizes, targetPixels);
    }

    private Size chooseClosestSize(Size[] sizes, int targetPixels) {
        Size best = sizes[0];
        int bestDiff = Math.abs(best.getWidth() * best.getHeight() - targetPixels);

        for (Size size : sizes) {
            int diff = Math.abs(size.getWidth() * size.getHeight() - targetPixels);
            if (diff < bestDiff) {
                best = size;
                bestDiff = diff;
            }
        }
        return best;
    }

    private void applyFlashMode(CaptureRequest.Builder builder) {
        switch (flash) {
            case "on":
                builder.set(CaptureRequest.FLASH_MODE, CaptureRequest.FLASH_MODE_SINGLE);
                builder.set(CaptureRequest.CONTROL_AE_MODE,
                        CaptureRequest.CONTROL_AE_MODE_ON_ALWAYS_FLASH);
                break;
            case "off":
                builder.set(CaptureRequest.FLASH_MODE, CaptureRequest.FLASH_MODE_OFF);
                builder.set(CaptureRequest.CONTROL_AE_MODE,
                        CaptureRequest.CONTROL_AE_MODE_ON);
                break;
            case "torch":
                builder.set(CaptureRequest.FLASH_MODE, CaptureRequest.FLASH_MODE_TORCH);
                builder.set(CaptureRequest.CONTROL_AE_MODE,
                        CaptureRequest.CONTROL_AE_MODE_ON);
                break;
            case "auto":
            default:
                builder.set(CaptureRequest.CONTROL_AE_MODE,
                        CaptureRequest.CONTROL_AE_MODE_ON_AUTO_FLASH);
                break;
        }
    }

    private void applyZoom(CaptureRequest.Builder builder) {
        if (currentZoom <= 1.0f) return;

        try {
            CameraCharacteristics chars =
                    cameraManager.getCameraCharacteristics(hardwareCameraId);
            android.graphics.Rect sensorRect =
                    chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE);
            if (sensorRect == null) return;

            float clampedZoom = Math.min(currentZoom, maxZoom);
            int cropW = (int) (sensorRect.width() / clampedZoom);
            int cropH = (int) (sensorRect.height() / clampedZoom);
            int left = (sensorRect.width() - cropW) / 2;
            int top = (sensorRect.height() - cropH) / 2;

            android.graphics.Rect cropRegion =
                    new android.graphics.Rect(left, top, left + cropW, top + cropH);
            builder.set(CaptureRequest.SCALER_CROP_REGION, cropRegion);
        } catch (Exception ignored) {}
    }

    // ========================================================================
    // Thread management
    // ========================================================================

    private void startBackgroundThread() {
        Log.d(TAG, "startBackgroundThread() called");
        backgroundThread = new HandlerThread("CameraBackground-" + cameraId);
        backgroundThread.start();
        backgroundHandler = new Handler(backgroundThread.getLooper());
        Log.d(TAG, "startBackgroundThread() background thread started");
    }

    private void stopBackgroundThread() {
        Log.d(TAG, "stopBackgroundThread() called");
        if (backgroundThread != null) {
            backgroundThread.quitSafely();
            try {
                backgroundThread.join(1000);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                throw new IllegalStateException("interrupted while stopping camera thread",
                        interrupted);
            }
            if (backgroundThread.isAlive()) {
                throw new IllegalStateException("camera background thread did not stop");
            }
            backgroundThread = null;
            backgroundHandler = null;
            Log.d(TAG, "stopBackgroundThread() background thread stopped");
        }
    }

    // ========================================================================
    // Utility
    // ========================================================================

    private String createTempFilePath(String prefix, String extension) {
        File cacheDir = activity.getCacheDir();
        File cameraDir = new File(cacheDir, "camera");
        if (!cameraDir.exists()) {
            cameraDir.mkdirs();
        }
        return new File(cameraDir,
                prefix + "_" + cameraId + "_" + System.currentTimeMillis() + extension)
                .getAbsolutePath();
    }

    private void fireEvent(String eventType, String jsonPayload) {
        Log.d(TAG, "fireEvent() eventType: " + eventType + ", payload: " + jsonPayload);
        NativeMethods.onCameraEvent(
                sessionId, token.generation(), cameraId, eventType, jsonPayload);
    }

    private String createResultJson() throws JSONException {
        JSONObject result = new JSONObject();
        result.put("cameraId", cameraId);
        return result.toString();
    }

    private static String errorJson(String errMsg) {
        return "{\"_error\":{\"errMsg\":\"" + escapeJson(errMsg) + "\"}}";
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
