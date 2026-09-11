package com.migo.runtime.internal;

import android.app.Activity;

import com.migo.runtime.internal.platform.AudioRecorderManager;
import com.migo.runtime.internal.platform.CameraManager;
import com.migo.runtime.internal.platform.ImageApiManager;
import com.migo.runtime.internal.platform.VideoManager;

import java.util.concurrent.ConcurrentHashMap;

/**
 * Domain class for per-session media manager delegation (recorder, camera, image API, video).
 *
 * @hide
 */
public final class MediaExports {

    private MediaExports() {}

    private static final Object sRecorderLock = new Object();
    private static final Object sImageApiLock = new Object();
    private static final Object sVideoLock = new Object();

    private static final ConcurrentHashMap<Integer, AudioRecorderManager> sRecorderManagers =
            new ConcurrentHashMap<>();

    static final class CameraSlot<T> {
        private final ResourceCleanup.DestroyAction<T> destroy;
        private final ResourceCleanup.Retained<T> retained =
                new ResourceCleanup.Retained<>(null);

        CameraSlot(ResourceCleanup.DestroyAction<T> destroy) {
            this.destroy = destroy;
        }

        T get() {
            return retained.get();
        }

        void replace(T replacement) {
            try {
                retained.replace(replacement, destroy);
            } catch (RuntimeException replacementFailure) {
                try {
                    destroy.destroy(replacement);
                } catch (RuntimeException replacementCleanupFailure) {
                    replacementFailure.addSuppressed(replacementCleanupFailure);
                }
                throw replacementFailure;
            }
        }

        void destroy() {
            retained.destroy(destroy);
        }
    }

    /** Per-session camera managers, keyed by "sessionId:cameraId". */
    private static final ConcurrentHashMap<String, CameraSlot<CameraManager>>
            sCameraManagers =
            new ConcurrentHashMap<>();

    private static final ConcurrentHashMap<Integer, ImageApiManager> sImageApiManagers =
            new ConcurrentHashMap<>();

    private static final ConcurrentHashMap<Integer, VideoManager> sVideoManagers =
            new ConcurrentHashMap<>();

    // ==================== Recorder ====================

    public static void recorderStart(int sessionId, String optionsJson) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            NativeMethods.onRecorderEvent(sessionId, RuntimeGenerationBoundary.UNFENCED, "error",
                    "{\"errMsg\":\"recorderManager.start:fail no context\"}");
            return;
        }
        Activity activity = ctx.getActivity();
        if (activity == null) {
            NativeMethods.onRecorderEvent(sessionId, RuntimeGenerationBoundary.UNFENCED, "error",
                    "{\"errMsg\":\"recorderManager.start:fail no activity\"}");
            return;
        }

        AudioRecorderManager mgr = liveRecorder(sessionId);
        if (mgr == null) {
            synchronized (sRecorderLock) {
                mgr = liveRecorder(sessionId);
                if (mgr == null) {
                    mgr = new AudioRecorderManager(sessionId, activity);
                    sRecorderManagers.put(sessionId, mgr);
                }
            }
        }
        mgr.start(optionsJson);
    }

    public static void recorderPause(int sessionId) {
        AudioRecorderManager mgr = liveRecorder(sessionId);
        if (mgr != null) {
            mgr.pause();
        }
    }

    public static void recorderResume(int sessionId) {
        AudioRecorderManager mgr = liveRecorder(sessionId);
        if (mgr != null) {
            mgr.resume();
        }
    }

    public static void recorderStop(int sessionId) {
        AudioRecorderManager mgr = liveRecorder(sessionId);
        if (mgr != null) {
            mgr.stop();
        }
    }

    public static void destroyRecorderManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sRecorderManagers,
                id -> id == sessionId,
                AudioRecorderManager::destroy);
    }

    // ==================== Camera ====================

    private static String cameraKey(int sessionId, int cameraId) {
        return sessionId + ":" + cameraId;
    }

    private static int extractCameraId(String optionsJson) {
        if (optionsJson == null) return 0;
        try {
            org.json.JSONObject opts = new org.json.JSONObject(optionsJson);
            return opts.optInt("cameraId", 0);
        } catch (Exception e) {
            return 0;
        }
    }

    private static void syncCameraLifecycle(int sessionId, CameraManager manager) {
        LifecycleStateSynchronizer.synchronize(
                manager,
                () -> NativeExports.isSessionResourceSuspended(sessionId),
                manager::setLifecycleSuspended);
    }

    /**
     * The recorder belonging to the runtime that is running now.
     *
     * <p>A retired one still holds the microphone. Reusing it would hand the
     * replacement isolate a recorder whose events the engine drops, so the game
     * would start recording and hear nothing back while the mic stayed busy.
     */
    private static AudioRecorderManager liveRecorder(int sessionId) {
        return RuntimeGenerationBoundary.liveEntry(
                sRecorderManagers, sessionId, AudioRecorderManager::destroy);
    }

    private static VideoManager liveVideoManager(int sessionId) {
        return RuntimeGenerationBoundary.liveEntry(
                sVideoManagers, sessionId, VideoManager::destroy);
    }

    private static CameraManager getCameraManager(int sessionId, int cameraId) {
        String key = cameraKey(sessionId, cameraId);
        CameraSlot<CameraManager> retained = sCameraManagers.get(key);
        CameraManager manager = retained != null ? retained.get() : null;
        if (manager == null) return null;
        // The same sweep RuntimeGenerationBoundary.liveEntry performs, written
        // out because this cache is keyed by "sessionId:cameraId" and holds a
        // slot rather than the manager. A camera built by a replaced runtime
        // still owns the device: handing it over would leave the new isolate
        // with a camera whose frames the engine drops, and the arbiter still
        // recording this session as the owner.
        if (!manager.runtimeToken().isCurrent()) {
            sCameraManagers.computeIfPresent(key, (ignored, slot) -> {
                slot.destroy();
                return null;
            });
            return null;
        }
        syncCameraLifecycle(sessionId, manager);
        return manager;
    }

    public static String cameraCreate(int sessionId, String optionsJson) {
        RuntimeContext ctx = RuntimeRegistry.get(sessionId);
        if (ctx == null) {
            return "{\"_error\":{\"errMsg\":\"createCamera:fail no context\"}}";
        }
        Activity activity = ctx.getActivity();
        if (activity == null) {
            return "{\"_error\":{\"errMsg\":\"createCamera:fail no activity\"}}";
        }

        int cameraId = 0;
        try {
            org.json.JSONObject opts = new org.json.JSONObject(optionsJson);
            cameraId = opts.optInt("cameraId", 0);
        } catch (Exception ignored) {}

        String key = cameraKey(sessionId, cameraId);
        if (NativeExports.isSessionTerminated(sessionId)) {
            return "{\"_error\":{\"errMsg\":\"createCamera:fail session destroyed\"}}";
        }
        boolean suspended = NativeExports.isSessionResourceSuspended(sessionId);
        CameraManager mgr = new CameraManager(sessionId, cameraId, activity, suspended);
        sCameraManagers.compute(key, (ignored, existing) -> {
            CameraSlot<CameraManager> retained = existing != null
                    ? existing
                    : new CameraSlot<>(CameraManager::destroy);
            retained.replace(mgr);
            return retained;
        });
        syncCameraLifecycle(sessionId, mgr);
        String result = mgr.create(optionsJson);
        syncCameraLifecycle(sessionId, mgr);
        return result;
    }

    public static void cameraDestroy(int sessionId, int cameraId) {
        String key = cameraKey(sessionId, cameraId);
        sCameraManagers.computeIfPresent(key, (ignored, retained) -> {
            retained.destroy();
            return null;
        });
    }


    public static void cameraTakePhotoAsync(
            int sessionId, int requestId, String optionsJson) {
        int cameraId = extractCameraId(optionsJson);
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr == null) {
            String payload = "{\"requestId\":" + requestId
                    + ",\"_error\":{\"errMsg\":\"camera.takePhoto:fail camera not found\"}}";
            NativeMethods.onCameraEvent(
                    sessionId,
                    RuntimeGenerationBoundary.UNFENCED,
                    cameraId,
                    "takePhotoResult",
                    payload);
            return;
        }
        mgr.takePhotoAsync(requestId, optionsJson);
    }

    public static String cameraStartRecord(int sessionId, String optionsJson) {
        int cameraId = extractCameraId(optionsJson);
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr == null) {
            return "{\"_error\":{\"errMsg\":\"camera.startRecord:fail camera not found\"}}";
        }
        return mgr.startRecord(optionsJson);
    }

    public static String cameraStopRecord(int sessionId, String optionsJson) {
        int cameraId = extractCameraId(optionsJson);
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr == null) {
            return "{\"_error\":{\"errMsg\":\"camera.stopRecord:fail camera not found\"}}";
        }
        return mgr.stopRecord(optionsJson);
    }

    public static String cameraSetZoom(int sessionId, String optionsJson) {
        int cameraId = extractCameraId(optionsJson);
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr == null) {
            return "{\"_error\":{\"errMsg\":\"camera.setZoom:fail camera not found\"}}";
        }
        return mgr.setZoom(optionsJson);
    }

    public static void cameraListenFrameChange(int sessionId, int cameraId) {
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr != null) {
            mgr.listenFrameChange();
        }
    }

    public static void cameraCloseFrameChange(int sessionId, int cameraId) {
        CameraManager mgr = getCameraManager(sessionId, cameraId);
        if (mgr != null) {
            mgr.closeFrameChange();
        }
    }

    public static void destroyCameraManagers(int sessionId) {
        String prefix = sessionId + ":";
        ResourceCleanup.destroyMatching(
                sCameraManagers,
                key -> key.startsWith(prefix),
                CameraSlot::destroy);
    }

    // ==================== Image API ====================

    private static ImageApiManager getOrCreateImageApiManager(int sessionId) {
        ImageApiManager existing = sImageApiManagers.get(sessionId);
        if (existing != null) return existing;
        synchronized (sImageApiLock) {
            existing = sImageApiManagers.get(sessionId);
            if (existing != null) return existing;
            RuntimeContext ctx = RuntimeRegistry.get(sessionId);
            if (ctx == null) return null;
            Activity activity = ctx.getActivity();
            if (activity == null) return null;
            ImageApiManager mgr = new ImageApiManager(sessionId, activity);
            sImageApiManagers.put(sessionId, mgr);
            return mgr;
        }
    }

    public static void imageSaveToPhotosAlbum(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) return;
        mgr.saveToPhotosAlbum(optionsJson);
    }

    public static void imagePreviewMedia(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) return;
        mgr.previewMedia(optionsJson);
    }

    public static void imagePreviewImage(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) return;
        mgr.previewImage(optionsJson);
    }

    public static void imageCompress(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) {
            NativeMethods.onCompressImageResult(sessionId, CallbackCorrelation.failure(
                    CallbackCorrelation.requestIdOf(optionsJson), "compressImage", "no context"));
            return;
        }
        mgr.compressAsync(optionsJson);
    }

    public static void imageChooseMessageFile(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) {
            // Returning silently leaves the request pending until the runtime's
            // thirty-second timeout, and under the FIFO fallback that timeout
            // then rejects whichever request happens to be oldest.
            NativeMethods.onChooseMessageFileResult(sessionId, CallbackCorrelation.failure(
                    CallbackCorrelation.requestIdOf(optionsJson), "chooseMessageFile", "no context"));
            return;
        }
        mgr.chooseMessageFile(optionsJson);
    }

    public static void imageChooseImage(int sessionId, String optionsJson) {
        ImageApiManager mgr = getOrCreateImageApiManager(sessionId);
        if (mgr == null) {
            NativeMethods.onChooseImageResult(sessionId, CallbackCorrelation.failure(
                    CallbackCorrelation.requestIdOf(optionsJson), "chooseImage", "no context"));
            return;
        }
        mgr.chooseImage(optionsJson);
    }

    public static void destroyImageApiManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sImageApiManagers,
                id -> id == sessionId,
                ImageApiManager::destroy);
    }

    // ==================== Video ====================

    private static void syncVideoLifecycle(int sessionId, VideoManager manager) {
        LifecycleStateSynchronizer.synchronize(
                manager,
                () -> NativeExports.isSessionResourceSuspended(sessionId),
                manager::setLifecycleSuspended);
    }

    private static VideoManager getOrCreateVideoManager(int sessionId) {
        VideoManager existing = liveVideoManager(sessionId);
        if (existing != null) {
            syncVideoLifecycle(sessionId, existing);
            return existing;
        }
        synchronized (sVideoLock) {
            existing = liveVideoManager(sessionId);
            if (existing != null) {
                syncVideoLifecycle(sessionId, existing);
                return existing;
            }
            if (NativeExports.isSessionTerminated(sessionId)) return null;
            RuntimeContext ctx = RuntimeRegistry.get(sessionId);
            if (ctx == null) return null;
            Activity activity = ctx.getActivity();
            if (activity == null) return null;
            boolean suspended = NativeExports.isSessionResourceSuspended(sessionId);
            VideoManager mgr = new VideoManager(sessionId, activity, suspended);
            sVideoManagers.put(sessionId, mgr);
            syncVideoLifecycle(sessionId, mgr);
            return mgr;
        }
    }

    public static String videoCreate(int sessionId, String optionsJson) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) throw new RuntimeException("videoCreate:fail no context");
        return mgr.create(optionsJson);
    }

    public static void videoPlay(int sessionId, int videoId) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.play(videoId);
    }

    public static void videoPause(int sessionId, int videoId) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.pause(videoId);
    }

    public static void videoStop(int sessionId, int videoId) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.stop(videoId);
    }

    public static void videoSeek(int sessionId, String json) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.seek(json);
    }

    public static void videoRequestFullscreen(int sessionId, String json) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.requestFullscreen(json);
    }

    public static void videoExitFullscreen(int sessionId, int videoId) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.exitFullscreen(videoId);
    }

    public static void videoSetProperty(int sessionId, String json) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.setProperty(json);
    }

    public static void videoDestroy(int sessionId, int videoId) {
        VideoManager mgr = getOrCreateVideoManager(sessionId);
        if (mgr == null) return;
        mgr.destroyVideo(videoId);
    }

    public static void destroyVideoManager(int sessionId) {
        ResourceCleanup.destroyMatching(
                sVideoManagers,
                id -> id == sessionId,
                VideoManager::destroy);
    }

    /**
     * Release the hardware while the app is in the background.
     *
     * <p>Deliberately reads the caches directly rather than through the
     * generation sweep the other accessors use: this is driven by the Activity's
     * lifecycle, not by a runtime, and a manager left over from a replaced
     * runtime still owns the camera or the microphone. Suspending it is exactly
     * what should happen; destroying it as a side effect of the app going to
     * the background is not.
     */
    public static void suspendPowerSensitiveManagers(int sessionId) {
        String cameraPrefix = sessionId + ":";
        for (String key : sCameraManagers.keySet()) {
            if (key.startsWith(cameraPrefix)) {
                CameraSlot<CameraManager> retained = sCameraManagers.get(key);
                CameraManager camera = retained != null ? retained.get() : null;
                if (camera != null) {
                    camera.suspendForLifecycle();
                }
            }
        }

        VideoManager video = sVideoManagers.get(sessionId);
        if (video != null) {
            video.suspendForLifecycle();
        }
    }

    public static void resumePowerSensitiveManagers(int sessionId) {
        String cameraPrefix = sessionId + ":";
        for (String key : sCameraManagers.keySet()) {
            if (key.startsWith(cameraPrefix)) {
                CameraSlot<CameraManager> retained = sCameraManagers.get(key);
                CameraManager camera = retained != null ? retained.get() : null;
                if (camera != null) {
                    camera.resumeForLifecycle();
                }
            }
        }

        VideoManager video = sVideoManagers.get(sessionId);
        if (video != null) {
            video.resumeForLifecycle();
        }
    }

    // ==================== Bulk Destroy ====================

    public static void destroyAll(int sessionId) {
        ResourceCleanup.runAll(
                () -> destroyRecorderManager(sessionId),
                () -> destroyCameraManagers(sessionId),
                () -> destroyImageApiManager(sessionId),
                () -> destroyVideoManager(sessionId));
    }
}
