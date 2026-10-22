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
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.media.MediaMetadataRetriever;
import android.media.MediaRecorder;
import android.graphics.Bitmap;
import android.os.Build;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Looper;
import android.util.Log;
import android.util.Size;
import android.view.View;

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
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Platform-level camera manager using Camera2 API.
 * <p>
 * Supports photo capture, video recording, zoom control, and real-time frame streaming.
 * Every request is the engine's, carrying content's {@code cameraId} and a call's
 * {@code requestId}; a call is answered through {@link NativeMethods#onCameraResult}
 * once what it asked has happened -- {@code create} once the preview runs -- and
 * never blocks the thread that made it. What happens to a camera on its own is
 * {@link NativeMethods#onCameraEvent}; frames go to
 * {@link NativeMethods#onCameraFrameData}.
 *
 * @hide
 */
public final class CameraManager implements RuntimeScoped {

    private static final String TAG = "CameraManager";

    /** The camera's calls and events by number, as the engine numbers them
     *  ({@code shared::services::camera_method}, {@code MIGO_CAMERA_*}). */
    public static final int METHOD_CREATE = 0;
    public static final int METHOD_TAKE_PHOTO = 3;
    public static final int METHOD_START_RECORD = 4;
    public static final int METHOD_STOP_RECORD = 5;
    public static final int METHOD_SET_ZOOM = 6;
    public static final int EVENT_STOP = 0;
    public static final int EVENT_AUTH_CANCEL = 1;

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

    // The preview. The camera draws into previewTexture, this manager's from
    // construction to destroy, through previewSurface, one per capture session;
    // preview (main thread only) shows it over the game, in the rectangle
    // content gave -- in the game's logical pixels.
    private final View gameView;
    private final SurfaceTexture previewTexture = new SurfaceTexture(false);
    private android.view.Surface previewSurface;
    private CameraPreview preview;
    /** Set once create has found the camera: there is a preview to show. */
    private volatile boolean previewWanted = false;
    private volatile float previewX = 0f;
    private volatile float previewY = 0f;
    private volatile float previewWidth = 300f;
    private volatile float previewHeight = 150f;

    // Photo capture
    private ImageReader photoReader;

    // Video recording
    private MediaRecorder mediaRecorder;
    private String videoFilePath;
    private long recordStartTime;

    // Semaphore to prevent concurrent open/close
    private final Semaphore cameraOpenCloseLock = new Semaphore(1);

    /** Every call in flight, request id to method: each is answered once, and
     *  those still waiting when the camera goes fail with it. */
    private final ConcurrentHashMap<Integer, Integer> pendingCalls = new ConcurrentHashMap<>();
    /** The create request waiting for the preview to run, or 0 once answered. */
    private final AtomicInteger pendingCreate = new AtomicInteger();
    /** The start-record request waiting for the recorder to run, or 0. */
    private final AtomicInteger pendingRecordStart = new AtomicInteger();

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

    // Resolved sizes: the preview's picture, content's frames (its "size"), a
    // photo, a video; and how the sensor is turned from the device's natural
    // orientation.
    private Size displaySize;
    private Size frameSize;
    private int sensorOrientation;
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

    /**
     * @param gameView the game's view, which the preview's rectangle is from
     */
    public CameraManager(
            int sessionId,
            int cameraId,
            Activity activity,
            View gameView,
            boolean lifecycleSuspended) {
        this.sessionId = sessionId;
        this.token = RuntimeGenerationBoundary.acquire(sessionId);
        this.cameraId = cameraId;
        this.activity = activity;
        this.gameView = gameView;
        this.mainHandler = new Handler(Looper.getMainLooper());
        this.cameraActivityRequest = new LifecycleRequestState<>(lifecycleSuspended);
    }

    /**
     * Open the camera content asked for and answer {@code requestId} once its
     * preview runs -- or with why it could not. Returns at once: the camera
     * opens on its own thread.
     *
     * @param requestJson cameraId, devicePosition, flash, size, and where the preview goes
     */
    public void create(int requestId, String requestJson) {
        pendingCalls.put(requestId, METHOD_CREATE);
        pendingCreate.set(requestId);
        parseOptions(requestJson);

        if (activity.checkSelfPermission(Manifest.permission.CAMERA)
                != PackageManager.PERMISSION_GRANTED) {
            fireEvent(EVENT_AUTH_CANCEL);
            failOrStop("auth deny");
            return;
        }
        // Keyed by position, not by "camera": a device has several physical cameras
        // and two games using different ones is legitimate.
        if (!ExclusiveDeviceArbiter.tryAcquire(
                ExclusiveDeviceArbiter.camera(position), sessionId)) {
            failOrStop("in use by another game");
            return;
        }
        cameraManager = (android.hardware.camera2.CameraManager)
                activity.getSystemService(Context.CAMERA_SERVICE);
        if (cameraManager == null) {
            failOrStop("camera service unavailable");
            return;
        }
        try {
            hardwareCameraId = findCameraId();
            if (hardwareCameraId == null) {
                failOrStop("no " + position + " camera");
                return;
            }
            resolveSizes();
            synchronized (initializationLock) {
                if (cameraActivityRequest.isDestroyed()) {
                    failOrStop("camera destroyed");
                    return;
                }
                startBackgroundThread();
                previewWanted = true;
                mainHandler.post(this::placePreview);
                // Lifecycle-suspended: it opens when the session resumes, and is
                // answered then.
                if (cameraActivityRequest.requestStart(Boolean.TRUE)
                        == LifecycleRequestState.Action.NONE) {
                    return;
                }
                backgroundHandler.post(this::openOrFail);
            }
        } catch (Exception e) {
            cameraActivityRequest.startFailed(false);
            failOrStop(String.valueOf(e.getMessage()));
        }
    }

    /** Open the camera on its own thread; a failure fails its creation. */
    private void openOrFail() {
        try {
            openCamera();
        } catch (Exception e) {
            cameraActivityRequest.startFailed(false);
            failOrStop(String.valueOf(e.getMessage()));
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
        // The camera has let go of the texture; once the view has too, it goes.
        mainHandler.post(() -> {
            if (preview != null) {
                preview.remove();
                preview = null;
            }
            previewTexture.release();
        });
        // Content let it go: no onStop, which is for a camera that stops on its
        // own. What it still waited for fails with it.
        pendingCreate.set(0);
        pendingRecordStart.set(0);
        for (Integer requestId : new ArrayList<>(pendingCalls.keySet())) {
            fail(requestId, "camera destroyed");
        }
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
            try {
                openCamera();
            } catch (Exception e) {
                cameraActivityRequest.startFailed(true);
                failOrStop("camera resume failed: " + e.getMessage());
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
                failOrStop("camera resume failed: " + e.getMessage());
            }
            return;
        }

        if (captureSession == null) {
            if (state.get() == STATE_OPENED) {
                try {
                    startPreviewSession();
                } catch (Exception e) {
                    failOrStop("camera resume failed: " + e.getMessage());
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
            failOrStop("camera resume failed: " + e.getMessage());
        }
    }

    /**
     * Take a photo at {@code quality} and answer {@code requestId} with its file
     * once written. Returns at once: the capture and the write run on the
     * camera's thread.
     */
    public void takePhoto(final int requestId, String requestJson) {
        pendingCalls.put(requestId, METHOD_TAKE_PHOTO);
        if (!cameraActivityRequest.isActive()) {
            fail(requestId, "camera suspended");
            return;
        }
        if (state.get() == STATE_CLOSED || captureSession == null || cameraDevice == null
                || photoReader == null) {
            fail(requestId, "camera not ready");
            return;
        }
        String quality = "normal";
        try {
            quality = new JSONObject(requestJson).optString("quality", "normal");
        } catch (JSONException ignored) {
            // The public default for a quality content did not give.
        }
        final int jpegQuality;
        switch (quality) {
            case "high": jpegQuality = 95; break;
            case "low": jpegQuality = 60; break;
            default: jpegQuality = 80; break;
        }
        try {
            final String tempPath = createTempFilePath("photo", ".jpg");
            photoReader.setOnImageAvailableListener(reader -> {
                Image image = null;
                try {
                    image = reader.acquireLatestImage();
                    if (image == null) {
                        fail(requestId, "no image");
                        return;
                    }
                    ByteBuffer buffer = image.getPlanes()[0].getBuffer();
                    byte[] bytes = new byte[buffer.remaining()];
                    buffer.get(bytes);
                    try (FileOutputStream fos = new FileOutputStream(tempPath)) {
                        fos.write(bytes);
                    }
                    JSONObject result = new JSONObject();
                    result.put("tempImagePath", tempPath);
                    result.put("width", photoSize.getWidth());
                    result.put("height", photoSize.getHeight());
                    succeed(requestId, result);
                } catch (Exception e) {
                    new File(tempPath).delete();
                    fail(requestId, String.valueOf(e.getMessage()));
                } finally {
                    if (image != null) image.close();
                }
            }, backgroundHandler);

            CaptureRequest.Builder captureBuilder =
                    cameraDevice.createCaptureRequest(CameraDevice.TEMPLATE_STILL_CAPTURE);
            captureBuilder.addTarget(photoReader.getSurface());
            captureBuilder.set(CaptureRequest.JPEG_QUALITY, (byte) jpegQuality);
            applyFlashMode(captureBuilder);
            applyZoom(captureBuilder);
            captureSession.capture(captureBuilder.build(), null, backgroundHandler);
        } catch (Exception e) {
            fail(requestId, String.valueOf(e.getMessage()));
        }
    }

    /**
     * Start recording and answer {@code requestId} once the recorder runs.
     * Returns at once: the recording session is made on the camera's thread.
     */
    public void startRecord(final int requestId) {
        pendingCalls.put(requestId, METHOD_START_RECORD);
        if (!cameraActivityRequest.isActive()) {
            fail(requestId, "camera suspended");
            return;
        }
        if (state.get() != STATE_OPENED || cameraDevice == null || backgroundHandler == null) {
            fail(requestId, state.get() == STATE_RECORDING ? "already recording" : "camera not ready");
            return;
        }
        if (!pendingRecordStart.compareAndSet(0, requestId)) {
            fail(requestId, "already starting");
            return;
        }
        backgroundHandler.post(this::startRecordInternal);
    }

    private void startRecordInternal() {
        try {
            closeSession();
            videoFilePath = createTempFilePath("video", ".mp4");
            setupMediaRecorder();
            recordingPausedForLifecycle = false;

            // Recording: the preview, which shows what is recorded, the
            // recorder's surface, and the frames' while content listens to them.
            List<android.view.Surface> surfaces = new ArrayList<>();
            android.view.Surface shown = openPreviewSurface();
            android.view.Surface recorderSurface = mediaRecorder.getSurface();
            surfaces.add(shown);
            surfaces.add(recorderSurface);
            if (frameListening.get() && frameReader != null) {
                surfaces.add(frameReader.getSurface());
            }
            previewRequestBuilder = cameraDevice.createCaptureRequest(CameraDevice.TEMPLATE_RECORD);
            previewRequestBuilder.addTarget(shown);
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
                            int requestId = pendingRecordStart.getAndSet(0);
                            try {
                                if (cameraActivityRequest.isDestroyed()) {
                                    session.close();
                                    captureSession = null;
                                    return;
                                }
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
                                succeed(requestId, new JSONObject());
                            } catch (Exception e) {
                                fail(requestId, String.valueOf(e.getMessage()));
                                restorePreview();
                            }
                        }

                        @Override
                        public void onConfigureFailed(CameraCaptureSession session) {
                            fail(pendingRecordStart.getAndSet(0), "recording session config failed");
                            restorePreview();
                        }
                    }, backgroundHandler);
        } catch (Exception e) {
            fail(pendingRecordStart.getAndSet(0), String.valueOf(e.getMessage()));
            restorePreview();
        }
    }

    /** After a recording that did not start: the recorder let go, the preview back. */
    private void restorePreview() {
        releaseMediaRecorder(false);
        try {
            sessionNeedsReconfigure = false;
            startPreviewSession();
        } catch (Exception e) {
            failOrStop("preview failed: " + e.getMessage());
        }
    }

    /**
     * Stop recording and answer {@code requestId} with the video and its cover,
     * its first frame, once both are written. Returns at once.
     */
    public void stopRecord(final int requestId) {
        pendingCalls.put(requestId, METHOD_STOP_RECORD);
        if (state.get() != STATE_RECORDING || backgroundHandler == null) {
            fail(requestId, "not recording");
            return;
        }
        backgroundHandler.post(() -> stopRecordInternal(requestId));
    }

    /**
     * Zoom to what content asked, within what the camera can, and answer
     * {@code requestId} with the zoom taken.
     */
    public synchronized void setZoom(int requestId, String requestJson) {
        pendingCalls.put(requestId, METHOD_SET_ZOOM);
        if (state.get() == STATE_CLOSED && !cameraActivityRequest.isRequested()) {
            fail(requestId, "camera not ready");
            return;
        }
        float zoom = 1.0f;
        try {
            zoom = (float) new JSONObject(requestJson).optDouble("zoom", 1.0);
        } catch (JSONException ignored) {
            // The engine checked the zoom content gave.
        }
        currentZoom = Math.max(1.0f, Math.min(zoom, maxZoom));
        if (cameraActivityRequest.isActive()
                && previewRequestBuilder != null
                && captureSession != null) {
            try {
                applyZoom(previewRequestBuilder);
                captureSession.setRepeatingRequest(
                        previewRequestBuilder.build(), null, backgroundHandler);
            } catch (Exception e) {
                fail(requestId, String.valueOf(e.getMessage()));
                return;
            }
        }
        try {
            JSONObject result = new JSONObject();
            // To a tenth, as the reference reports it.
            result.put("zoom", Math.round(currentZoom * 10) / 10.0);
            succeed(requestId, result);
        } catch (JSONException e) {
            fail(requestId, "result");
        }
    }

    /** Show the preview where content put it, or move it there. Main thread. */
    private void placePreview() {
        if (!previewWanted || cameraActivityRequest.isDestroyed()) return;
        if (preview == null) preview = new CameraPreview(activity, gameView, previewTexture);
        preview.place(previewX, previewY, previewWidth, previewHeight);
    }

    /**
     * What content changed on the camera: where its preview stands and the
     * flash at once; the camera it faces or its frames' size by opening it
     * again, when it is not recording.
     */
    public void set(String requestJson) {
        JSONObject changed;
        try {
            changed = new JSONObject(requestJson);
        } catch (JSONException e) {
            return;
        }
        if (changed.has("x") || changed.has("y") || changed.has("width") || changed.has("height")) {
            previewX = (float) changed.optDouble("x", previewX);
            previewY = (float) changed.optDouble("y", previewY);
            previewWidth = (float) changed.optDouble("width", previewWidth);
            previewHeight = (float) changed.optDouble("height", previewHeight);
            mainHandler.post(this::placePreview);
        }
        Handler handler = backgroundHandler;
        if (handler == null) return;
        handler.post(() -> {
            if (cameraActivityRequest.isDestroyed()) return;
            if (changed.has("flash")) {
                flash = changed.optString("flash", flash);
                if (state.get() != STATE_CLOSED && previewRequestBuilder != null
                        && captureSession != null && cameraActivityRequest.isActive()) {
                    try {
                        applyFlashMode(previewRequestBuilder);
                        captureSession.setRepeatingRequest(
                                previewRequestBuilder.build(), null, backgroundHandler);
                    } catch (Exception e) {
                        Log.w(TAG, "set() flash: " + e.getMessage());
                    }
                }
            }
            String newPosition = changed.optString("devicePosition", position);
            String newSize = changed.optString("size", sizePreset);
            if ((newPosition.equals(position) && newSize.equals(sizePreset))
                    || state.get() == STATE_RECORDING) {
                return;
            }
            reopen(newPosition, newSize);
        });
    }

    /** The same camera, facing {@code newPosition} with frames of {@code newSize}. */
    private void reopen(String newPosition, String newSize) {
        if (!newPosition.equals(position)) {
            if (!ExclusiveDeviceArbiter.tryAcquire(
                    ExclusiveDeviceArbiter.camera(newPosition), sessionId)) {
                Log.w(TAG, "set() the " + newPosition + " camera is another game's");
                return;
            }
            ExclusiveDeviceArbiter.release(ExclusiveDeviceArbiter.camera(position), sessionId);
        }
        position = newPosition;
        sizePreset = newSize;
        try {
            closeCamera(false);
            hardwareCameraId = findCameraId();
            if (hardwareCameraId == null) {
                failOrStop("no " + position + " camera");
                return;
            }
            resolveSizes();
            if (frameListening.get()) createFrameReader();
            if (cameraActivityRequest.isActive()) openCamera();
        } catch (Exception e) {
            failOrStop("camera reopen failed: " + e.getMessage());
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
            failOrStop("camera frames failed: " + e.getMessage());
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
            fireEvent(EVENT_AUTH_CANCEL);
            throw new CameraAccessException(
                    CameraAccessException.CAMERA_ERROR, "auth deny");
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
                            return;
                        }
                        // Opened as the session was suspended: it is the camera
                        // content asked for, and its preview runs on resume.
                        if (!cameraActivityRequest.isActive()) {
                            answerCreate();
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
                        failOrStop("camera open failed: " + e.getMessage());
                    }
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    Log.w(TAG, "openCamera() onDisconnected callback called");
                    cameraOpenCloseLock.release();
                    camera.close();
                    cameraDevice = null;
                    state.set(STATE_CLOSED);
                    cameraActivityRequest.startFailed(true);
                    failOrStop("camera disconnected");
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    Log.e(TAG, "openCamera() onError callback called with error code: " + error);
                    cameraOpenCloseLock.release();
                    camera.close();
                    cameraDevice = null;
                    state.set(STATE_CLOSED);
                    cameraActivityRequest.startFailed(true);
                    failOrStop("camera device error " + error);
                }
            }, backgroundHandler);
            Log.d(TAG, "openCamera() openCamera() call completed");
        } catch (SecurityException e) {
            Log.e(TAG, "openCamera() SecurityException: " + e.getMessage(), e);
            cameraOpenCloseLock.release();
            fireEvent(EVENT_AUTH_CANCEL);
            throw new CameraAccessException(CameraAccessException.CAMERA_ERROR, "auth deny");
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
                });
    }

    private void startPreviewSession() throws CameraAccessException {
        Log.d(TAG, "startPreviewSession() called");
        if (cameraDevice == null) {
            Log.e(TAG, "startPreviewSession() cameraDevice is null, returning");
            return;
        }

        List<android.view.Surface> surfaces = new ArrayList<>();
        surfaces.add(openPreviewSurface());

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
                            failOrStop("camera closed");
                            return;
                        }
                        captureSession = session;
                        sessionNeedsReconfigure = false;
                        if (!cameraActivityRequest.isActive()) {
                            Log.d(TAG, "startPreviewSession() configured while suspended");
                            answerCreate();
                            return;
                        }
                        try {
                            Log.d(TAG, "startPreviewSession() setting repeating request");
                            captureSession.setRepeatingRequest(
                                    previewRequestBuilder.build(), null, backgroundHandler);
                            Log.d(TAG, "startPreviewSession() repeating request set successfully");
                            answerCreate();
                        } catch (Exception e) {
                            Log.e(TAG, "startPreviewSession() onConfigured exception: " + e.getMessage(), e);
                            failOrStop("preview failed: " + e.getMessage());
                        }
                    }

                    @Override
                    public void onConfigureFailed(CameraCaptureSession session) {
                        Log.e(TAG, "startPreviewSession() onConfigureFailed callback called");
                        sessionNeedsReconfigure = true;
                        failOrStop("preview session config failed");
                    }
                }, backgroundHandler);
        Log.d(TAG, "startPreviewSession() createCaptureSession call completed");
    }

    /**
     * The camera's way into the preview texture for a new session, at the
     * picture size it draws, which the preview is told so it can show it.
     */
    private android.view.Surface openPreviewSurface() {
        if (previewSurface != null) previewSurface.release();
        int width = displaySize.getWidth();
        int height = displaySize.getHeight();
        int orientation = sensorOrientation;
        previewTexture.setDefaultBufferSize(width, height);
        previewSurface = new android.view.Surface(previewTexture);
        mainHandler.post(() -> {
            if (preview != null) preview.setBuffer(width, height, orientation);
        });
        return previewSurface;
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
                frameSize.getWidth(), frameSize.getHeight(),
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
                    // [position, position+remaining) window and strides; native
                    // code reads them once, into the RGBA content is given.
                    // position()/remaining() are pure reads (no get()), so the
                    // window matches position..limit and the planes are untouched.
                    // YUV_420_888's U and V planes share their row and pixel
                    // strides; its Y plane's pixel stride is always 1.
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
                            yBuffer, yOff, yLen, planes[0].getRowStride(),
                            uBuffer, uOff, uLen,
                            vBuffer, vOff, vLen,
                            planes[1].getRowStride(), planes[1].getPixelStride(),
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
        // With its sound where the microphone may be used and there is one; the
        // picture alone otherwise, rather than no video at all.
        boolean withSound = activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO)
                == PackageManager.PERMISSION_GRANTED;
        if (withSound) {
            try {
                mediaRecorder.setAudioSource(MediaRecorder.AudioSource.MIC);
            } catch (RuntimeException e) {
                Log.w(TAG, "recording without sound: " + e.getMessage());
                withSound = false;
                mediaRecorder.reset();
            }
        }
        mediaRecorder.setVideoSource(MediaRecorder.VideoSource.SURFACE);
        mediaRecorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
        mediaRecorder.setOutputFile(videoFilePath);
        mediaRecorder.setVideoEncodingBitRate(10_000_000);
        mediaRecorder.setVideoFrameRate(30);
        mediaRecorder.setVideoSize(videoSize.getWidth(), videoSize.getHeight());
        mediaRecorder.setVideoEncoder(MediaRecorder.VideoEncoder.H264);
        if (withSound) mediaRecorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);

        // Handle front camera orientation
        if ("front".equals(position)) {
            mediaRecorder.setOrientationHint(270);
        } else {
            mediaRecorder.setOrientationHint(90);
        }

        mediaRecorder.prepare();
    }

    private void stopRecordInternal(int requestId) {
        if (state.get() != STATE_RECORDING) {
            fail(requestId, "not recording");
            return;
        }
        state.set(STATE_OPENED);
        recordingPausedForLifecycle = false;
        try {
            captureSession.stopRepeating();
        } catch (Exception e) {
            Log.w(TAG, "stopRecord() stopping the repeating request: " + e.getMessage());
        }
        String video = videoFilePath;
        boolean written = true;
        try {
            mediaRecorder.stop();
        } catch (RuntimeException e) {
            // MediaRecorder throws when it stopped before writing a frame.
            written = false;
            Log.w(TAG, "stopRecord() " + e.getMessage());
        }
        releaseMediaRecorder(false);
        try {
            sessionNeedsReconfigure = false;
            startPreviewSession();
        } catch (Exception e) {
            failOrStop("preview failed: " + e.getMessage());
        }
        if (!written) {
            new File(video).delete();
            fail(requestId, "nothing was recorded");
            return;
        }
        try {
            JSONObject result = new JSONObject();
            result.put("tempVideoPath", video);
            result.put("tempThumbPath", writeCover(video));
            succeed(requestId, result);
        } catch (Exception e) {
            new File(video).delete();
            fail(requestId, String.valueOf(e.getMessage()));
        }
    }

    /** The video's first frame as a JPEG beside it: its cover. */
    private String writeCover(String video) throws IOException {
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        try {
            retriever.setDataSource(video);
            Bitmap frame = retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST_SYNC);
            if (frame == null) throw new IOException("the video has no frame");
            String cover = createTempFilePath("cover", ".jpg");
            try (FileOutputStream out = new FileOutputStream(cover)) {
                frame.compress(Bitmap.CompressFormat.JPEG, 80, out);
            } finally {
                frame.recycle();
            }
            return cover;
        } finally {
            retriever.release();
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
            previewX = (float) opts.optDouble("x", 0);
            previewY = (float) opts.optDouble("y", 0);
            previewWidth = (float) opts.optDouble("width", 300);
            previewHeight = (float) opts.optDouble("height", 150);
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

        Integer orientation = chars.get(CameraCharacteristics.SENSOR_ORIENTATION);
        sensorOrientation = orientation != null ? orientation : 0;
        float density = DisplayCompat.getDensity(activity);
        boolean sideways = (sensorOrientation + 90 * DisplayCompat.getRotation(activity)) % 180 != 0;
        displaySize = CameraPreview.chooseBufferSize(map.getOutputSizes(SurfaceTexture.class),
                Math.round(previewWidth * density), Math.round(previewHeight * density), sideways);
        photoSize = choosePhotoSize(jpegSizes);
        frameSize = chooseFrameSize(yuvSizes);
        videoSize = chooseVideoSize(map.getOutputSizes(MediaRecorder.class));
        Log.d(TAG, "resolveSizes() selected - display: " + displaySize + ", photo: " + photoSize
                + ", frames: " + frameSize + ", video: " + videoSize);
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

    private Size chooseFrameSize(Size[] sizes) {
        if (sizes == null || sizes.length == 0) {
            return new Size(1280, 720);
        }

        // Content's frames, at the size it asked for
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

    /** What happened to the camera on its own: {@code EVENT_*}. */
    private void fireEvent(int event) {
        NativeMethods.onCameraEvent(sessionId, cameraId, event);
    }

    /** Answer call {@code requestId} with {@code result}, if it waits for one. */
    private void succeed(int requestId, JSONObject result) {
        Integer method = pendingCalls.remove(requestId);
        if (method == null) return;
        try {
            result.put("requestId", requestId);
        } catch (JSONException ignored) {
            // A request id always fits.
        }
        NativeMethods.onCameraResult(sessionId, method, result.toString());
    }

    /** Fail call {@code requestId} with {@code reason}, if it waits for an answer. */
    private void fail(int requestId, String reason) {
        Integer method = pendingCalls.remove(requestId);
        if (method == null) return;
        JSONObject result = new JSONObject();
        try {
            result.put("requestId", requestId);
            result.put("error", reason);
        } catch (JSONException ignored) {
            // Strings and an int always fit.
        }
        NativeMethods.onCameraResult(sessionId, method, result.toString());
    }

    /** The preview runs: the create request, if it still waits, succeeds. */
    private void answerCreate() {
        int requestId = pendingCreate.getAndSet(0);
        if (requestId != 0) succeed(requestId, new JSONObject());
    }

    /**
     * The camera cannot run: its create request fails with {@code reason} if it
     * still waits; a camera content already has has stopped.
     */
    private void failOrStop(String reason) {
        int requestId = pendingCreate.getAndSet(0);
        if (requestId != 0) {
            fail(requestId, reason);
        } else {
            Log.w(TAG, "camera stopped: " + reason);
            fireEvent(EVENT_STOP);
        }
    }
}
