package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.util.Log;
import android.webkit.MimeTypeMap;

import com.migo.runtime.internal.CallbackCorrelation;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ResultProxyActivity;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Future;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.Semaphore;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Manages image-related APIs for a session.
 * Sync-style APIs (save, preview, compress) throw on failure.
 * Async-style APIs (chooseImage, chooseMessageFile) call back via
 * {@link NativeMethods#onChooseImageResult} / {@link NativeMethods#onChooseMessageFileResult}.
 *
 * @hide
 */
public class ImageApiManager {

    private static final String TAG = "ImageApiManager";

    /**
     * Compression is deliberately bounded: the old fixed pool's implicit
     * unbounded queue retained one task per request while a session was slow.
     */
    static final int COMPRESS_QUEUE_CAPACITY = 8;
    static final int MAX_DECODED_PIXELS = 50_000_000;
    static final Semaphore DECODED_PIXEL_PERMITS =
            new Semaphore(MAX_DECODED_PIXELS, true);

    static boolean tryReserveDecodedPixels(int pixels) {
        return pixels > 0 && DECODED_PIXEL_PERMITS.tryAcquire(pixels);
    }

    static void releaseDecodedPixels(int pixels) {
        if (pixels > 0) {
            DECODED_PIXEL_PERMITS.release(pixels);
        }
    }

    static final ExecutorService IMAGE_WORKERS =
            new ThreadPoolExecutor(
                    2,
                    2,
                    0L,
                    TimeUnit.MILLISECONDS,
                    new ArrayBlockingQueue<Runnable>(COMPRESS_QUEUE_CAPACITY),
                    new ThreadFactory() {
                        private final AtomicInteger counter = new AtomicInteger(1);

                        @Override
                        public Thread newThread(Runnable runnable) {
                            Thread thread = new Thread(runnable,
                                    "Migo-Image-" + counter.getAndIncrement());
                            thread.setDaemon(true);
                            return thread;
                        }
                    },
                    new ThreadPoolExecutor.AbortPolicy());

    private static final int REQUEST_CHOOSE_IMAGE = 9001;
    private static final int REQUEST_CAPTURE_IMAGE = 9002;
    private static final int REQUEST_CHOOSE_FILE = 9003;

    private final int sessionId;
    private final WeakReference<Activity> activityRef;
    private final Handler mainHandler;

    /**
     * Everything one picker launch needs to answer the request that started it.
     *
     * <p>This used to be three mutable fields on the manager, which meant a
     * second {@code chooseImage} overwrote the first one's state while the
     * first picker was still open: the first request's reply then carried the
     * second's correlation id and the second's item limit. A picker owns its
     * own request because two of them can be open at once.
     */
    private static final class PickerRequest {
        /** The runtime's correlation id, or {@link CallbackCorrelation#ABSENT}. */
        final int requestId;
        /** The most items {@code chooseImage} may return for this request. */
        final int count;
        /** Where the camera app was told to write, for a capture request. */
        Uri cameraOutput;

        PickerRequest(int requestId, int count) {
            this.requestId = requestId;
            this.count = count;
        }
    }
    private static final class PendingCompression {
        final int requestId;
        final AtomicBoolean settled = new AtomicBoolean(false);
        volatile Future<?> future;

        PendingCompression(int requestId) {
            this.requestId = requestId;
        }
    }

    private final CopyOnWriteArrayList<PendingCompression> pendingCompressions =
            new CopyOnWriteArrayList<>();

    public ImageApiManager(int sessionId, Activity activity) {
        this.sessionId = sessionId;
        this.activityRef = new WeakReference<>(activity);
        this.mainHandler = new Handler(Looper.getMainLooper());
    }

    private Activity getActivity() {
        return activityRef.get();
    }

    // ==================== saveImageToPhotosAlbum ====================

    /**
     * Save image to system photo album via MediaStore.
     * Options JSON: { "filePath": "..." }
     */
    public void saveToPhotosAlbum(String optionsJson) {
        try {
            JSONObject opts = new JSONObject(optionsJson);
            String filePath = opts.getString("filePath");

            File srcFile = new File(filePath);
            if (!srcFile.exists()) {
                throw new RuntimeException("saveImageToPhotosAlbum:fail file not found");
            }

            // Decode to verify it's a valid image and get format
            BitmapFactory.Options bmOpts = new BitmapFactory.Options();
            bmOpts.inJustDecodeBounds = true;
            BitmapFactory.decodeFile(filePath, bmOpts);
            String mimeType = bmOpts.outMimeType;
            if (mimeType == null) {
                mimeType = "image/jpeg";
            }

            String displayName = srcFile.getName();
            String extension = "";
            int dotIdx = displayName.lastIndexOf('.');
            if (dotIdx > 0) {
                extension = displayName.substring(dotIdx);
            }
            if (extension.isEmpty()) {
                extension = mimeType.contains("png") ? ".png" : ".jpg";
            }

            Activity activity = getActivity();
            if (activity == null) {
                throw new RuntimeException("saveImageToPhotosAlbum:fail activity is gone");
            }
            ContentResolver resolver = activity.getContentResolver();
            ContentValues values = new ContentValues();
            values.put(MediaStore.Images.Media.DISPLAY_NAME, "IMG_" + System.currentTimeMillis() + extension);
            values.put(MediaStore.Images.Media.MIME_TYPE, mimeType);
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                values.put(MediaStore.Images.Media.RELATIVE_PATH, Environment.DIRECTORY_PICTURES);
                values.put(MediaStore.Images.Media.IS_PENDING, 1);
            }

            Uri uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
            if (uri == null) {
                throw new RuntimeException("saveImageToPhotosAlbum:fail insert failed");
            }

            try (InputStream in = new java.io.FileInputStream(srcFile);
                 OutputStream out = resolver.openOutputStream(uri)) {
                if (out == null) {
                    throw new RuntimeException("saveImageToPhotosAlbum:fail cannot open output");
                }
                byte[] buf = new byte[8192];
                int len;
                while ((len = in.read(buf)) > 0) {
                    out.write(buf, 0, len);
                }
            }

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                values.clear();
                values.put(MediaStore.Images.Media.IS_PENDING, 0);
                resolver.update(uri, values, null, null);
            }

            Log.d(TAG, "saveToPhotosAlbum: saved to " + uri);
        } catch (RuntimeException e) {
            throw e;
        } catch (Exception e) {
            throw new RuntimeException("saveImageToPhotosAlbum:fail " + e.getMessage());
        }
    }

    // ==================== previewMedia ====================

    /**
     * Preview media (images and videos) in fullscreen using system viewer.
     * Options JSON: { "sources": [{"url":"...","type":"image|video","poster":"..."}], "current": 0 }
     */
    public void previewMedia(String optionsJson) {
        try {
            JSONObject opts = new JSONObject(optionsJson);
            JSONArray sources = opts.getJSONArray("sources");
            int current = opts.optInt("current", 0);

            if (sources.length() == 0) {
                throw new RuntimeException("previewMedia:fail sources is empty");
            }
            if (current < 0 || current >= sources.length()) {
                current = 0;
            }

            JSONObject item = sources.getJSONObject(current);
            String url = item.getString("url");
            String type = item.optString("type", "image");

            Uri uri = resolveUri(url);
            String mime = "video".equals(type) ? "video/*" : "image/*";

            Intent intent = new Intent(Intent.ACTION_VIEW);
            intent.setDataAndType(uri, mime);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            Activity activity = getActivity();
            if (activity == null) {
                throw new RuntimeException("previewMedia:fail activity is gone");
            }
            activity.startActivity(intent);
        } catch (RuntimeException e) {
            throw e;
        } catch (Exception e) {
            throw new RuntimeException("previewMedia:fail " + e.getMessage());
        }
    }

    // ==================== previewImage ====================

    /**
     * Preview images in fullscreen using system viewer.
     * Options JSON: { "urls": [...], "current": "..." }
     */
    public void previewImage(String optionsJson) {
        try {
            JSONObject opts = new JSONObject(optionsJson);
            JSONArray urls = opts.getJSONArray("urls");
            String current = opts.optString("current", "");

            if (urls.length() == 0) {
                throw new RuntimeException("previewImage:fail urls is empty");
            }

            // Find the current URL or default to first
            String targetUrl = current;
            if (targetUrl.isEmpty()) {
                targetUrl = urls.getString(0);
            }

            Uri uri = resolveUri(targetUrl);

            Intent intent = new Intent(Intent.ACTION_VIEW);
            intent.setDataAndType(uri, "image/*");
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            Activity activity = getActivity();
            if (activity == null) {
                throw new RuntimeException("previewImage:fail activity is gone");
            }
            activity.startActivity(intent);
        } catch (RuntimeException e) {
            throw e;
        } catch (Exception e) {
            throw new RuntimeException("previewImage:fail " + e.getMessage());
        }
    }

    // ==================== compressImage (async) ====================

    /**
     * Compress an image asynchronously.
     * Options JSON: { "src": "...", "quality": 80, "compressedWidth": 0, "compressedHeight": 0 }
     * Result delivered via NativeMethods.onCompressImageResult with { "tempFilePath": "..." }
     */
    public void compressAsync(final String optionsJson) {
        final JSONObject opts;
        try {
            opts = new JSONObject(optionsJson);
        } catch (JSONException malformed) {
            NativeMethods.onCompressImageResult(sessionId, CallbackCorrelation.failure(
                    CallbackCorrelation.ABSENT, "compressImage", malformed.getMessage()));
            return;
        }
        final int requestId = CallbackCorrelation.requestIdOf(opts);
        final PendingCompression pending = new PendingCompression(requestId);
        pendingCompressions.add(pending);
        final Runnable work = new Runnable() {
            @Override
            public void run() {
                try {
                    if (!pending.settled.get()) {
                        String result = compressSync(opts, requestId);
                        if (pending.settled.compareAndSet(false, true)) {
                            NativeMethods.onCompressImageResult(sessionId, result);
                        }
                    }
                } catch (Exception e) {
                    if (pending.settled.compareAndSet(false, true)) {
                        NativeMethods.onCompressImageResult(sessionId,
                                CallbackCorrelation.failure(
                                        requestId, "compressImage", e.getMessage()));
                    }
                } finally {
                    pendingCompressions.remove(pending);
                }
            }
        };
        try {
            pending.future = IMAGE_WORKERS.submit(work);
        } catch (RejectedExecutionException rejected) {
            pendingCompressions.remove(pending);
            NativeMethods.onCompressImageResult(sessionId,
                    CallbackCorrelation.failure(
                            requestId, "compressImage", "queue full"));
        }
    }

    private String compressSync(JSONObject opts, int requestId) throws Exception {
        String src = opts.getString("src");
        int quality = opts.optInt("quality", 80);
        int targetWidth = opts.optInt("compressedWidth", 0);
        int targetHeight = opts.optInt("compressedHeight", 0);

        quality = Math.max(0, Math.min(100, quality));

        File srcFile = new File(src);
        if (!srcFile.exists()) {
            throw new RuntimeException("file not found");
        }

        // First pass: get original dimensions
        BitmapFactory.Options bmOpts = new BitmapFactory.Options();
        bmOpts.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(src, bmOpts);
        int origWidth = bmOpts.outWidth;
        int origHeight = bmOpts.outHeight;

        if (origWidth <= 0 || origHeight <= 0) {
            throw new RuntimeException("invalid image");
        }

        // Calculate target dimensions
        int finalWidth = origWidth;
        int finalHeight = origHeight;
        if (targetWidth > 0 && targetHeight > 0) {
            finalWidth = targetWidth;
            finalHeight = targetHeight;
        } else if (targetWidth > 0) {
            float ratio = (float) targetWidth / origWidth;
            finalWidth = targetWidth;
            finalHeight = Math.round(origHeight * ratio);
        } else if (targetHeight > 0) {
            float ratio = (float) targetHeight / origHeight;
            finalHeight = targetHeight;
            finalWidth = Math.round(origWidth * ratio);
        }

        // Reserve sampled source pixels, the exact target, and an encoding
        // scratch allowance before decode.  Permits remain held through
        // resize and output, then release even when any stage throws.
        bmOpts.inJustDecodeBounds = false;
        bmOpts.inSampleSize = calculateInSampleSize(
                origWidth, origHeight, finalWidth, finalHeight);
        long sample = Math.max(1, bmOpts.inSampleSize);
        long sampledWidth = (origWidth + sample - 1L) / sample;
        long sampledHeight = (origHeight + sample - 1L) / sample;
        long requiredPixels = sampledWidth * sampledHeight
                + (long) finalWidth * finalHeight * 2L;
        if (requiredPixels <= 0 || requiredPixels > MAX_DECODED_PIXELS) {
            throw new RuntimeException("image too large");
        }
        int reservedPixels = (int) requiredPixels;
        if (!tryReserveDecodedPixels(reservedPixels)) {
            throw new RuntimeException("image pixel budget exhausted");
        }

        Bitmap bitmap = null;
        try {
            bitmap = BitmapFactory.decodeFile(src, bmOpts);
            if (bitmap == null) {
                throw new RuntimeException("decode failed");
            }

            // Scale to exact target if needed.
            if (bitmap.getWidth() != finalWidth || bitmap.getHeight() != finalHeight) {
                Bitmap scaled = Bitmap.createScaledBitmap(bitmap, finalWidth, finalHeight, true);
                if (scaled != bitmap) {
                    bitmap.recycle();
                }
                bitmap = scaled;
            }

            String mimeType = bmOpts.outMimeType;
            Bitmap.CompressFormat format = Bitmap.CompressFormat.JPEG;
            String ext = ".jpg";
            if (mimeType != null && mimeType.contains("png")) {
                format = Bitmap.CompressFormat.PNG;
                ext = ".png";
            }

            File tempFile = createTempFile("compress", ext);
            try (FileOutputStream fos = new FileOutputStream(tempFile)) {
                boolean ok = bitmap.compress(format, quality, fos);
                if (!ok) {
                    throw new RuntimeException("compress failed");
                }
            }
            return compressImageResultJson(requestId, tempFile.getAbsolutePath());
        } finally {
            if (bitmap != null && !bitmap.isRecycled()) {
                bitmap.recycle();
            }
            releaseDecodedPixels(reservedPixels);
        }
    }

    // ==================== chooseMessageFile (async) ====================

    /**
     * Choose files from system file picker (async, results via callback).
     * Options JSON: { "count": 10, "type": "all", "extension": [] }
     */
    public void chooseMessageFile(String optionsJson) {
        try {
            JSONObject opts = new JSONObject(optionsJson);
            final PickerRequest request =
                    new PickerRequest(CallbackCorrelation.requestIdOf(opts), opts.optInt("count", 1));
            int count = request.count;
            String type = opts.optString("type", "all");

            String mimeType;
            switch (type) {
                case "image":
                    mimeType = "image/*";
                    break;
                case "video":
                    mimeType = "video/*";
                    break;
                case "file":
                    mimeType = "*/*";
                    break;
                default: // "all"
                    mimeType = "*/*";
                    break;
            }

            mainHandler.post(() -> {
                try {
                    Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
                    intent.setType(mimeType);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    if (count > 1) {
                        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
                    }
                    Activity activity = getActivity();
                    if (activity == null) {
                        sendChooseMessageFileError(request.requestId, "activity is gone");
                        return;
                    }
                    ResultProxyActivity.launch(activity,
                            Intent.createChooser(intent, "Choose File"),
                            REQUEST_CHOOSE_FILE,
                            (code, resultCode, data) -> onChooseFileResult(request, resultCode, data));
                } catch (Exception e) {
                    Log.e(TAG, "chooseMessageFile: failed to launch picker", e);
                    sendChooseMessageFileError(request.requestId, e.getMessage());
                }
            });
        } catch (Exception e) {
            throw new RuntimeException("chooseMessageFile:fail " + e.getMessage());
        }
    }

    // ==================== chooseImage (async) ====================

    /**
     * Choose images from album or camera (async, results via callback).
     * Options JSON: { "count": 9, "sizeType": [...], "sourceType": [...] }
     */
    public void chooseImage(String optionsJson) {
        try {
            JSONObject opts = new JSONObject(optionsJson);
            int count = opts.optInt("count", 9);
            JSONArray sourceType = opts.optJSONArray("sourceType");

            boolean hasAlbum = true;
            boolean hasCamera = true;
            if (sourceType != null) {
                hasAlbum = jsonArrayContains(sourceType, "album");
                hasCamera = jsonArrayContains(sourceType, "camera");
            }

            final PickerRequest request =
                    new PickerRequest(CallbackCorrelation.requestIdOf(opts), count);
            final boolean useCameraOnly = hasCamera && !hasAlbum;

            mainHandler.post(() -> {
                try {
                    if (useCameraOnly) {
                        launchCameraCapture(request);
                    } else {
                        // Album only, or both: the system chooser covers each.
                        launchImagePicker(request);
                    }
                } catch (Exception e) {
                    Log.e(TAG, "chooseImage: failed to launch", e);
                    sendChooseImageError(request.requestId, e.getMessage());
                }
            });
        } catch (Exception e) {
            throw new RuntimeException("chooseImage:fail " + e.getMessage());
        }
    }

    // ==================== ActivityResult handling ====================

    private void onPickImageResult(PickerRequest request, int resultCode, Intent data) {
        if (resultCode != Activity.RESULT_OK || data == null) {
            sendChooseImageError(request.requestId, "cancel");
            return;
        }
        handleChooseImageResult(request, data);
    }

    private void onCaptureImageResult(PickerRequest request, int resultCode) {
        if (resultCode != Activity.RESULT_OK) {
            sendChooseImageError(request.requestId, "cancel");
            return;
        }
        handleCaptureImageResult(request);
    }

    private void onChooseFileResult(PickerRequest request, int resultCode, Intent data) {
        if (resultCode != Activity.RESULT_OK || data == null) {
            sendChooseMessageFileError(request.requestId, "cancel");
            return;
        }
        handleChooseFileResult(request, data);
    }

    /**
     * Cancel queued and running compression work for this session.  A canceled
     * task keeps its pixel permits until its runnable exits; this prevents
     * cancellation from making a second task over-admit native bitmap memory.
     */
    public void cancelPendingCompression() {
        for (PendingCompression pending : pendingCompressions) {
            if (pending.settled.compareAndSet(false, true)) {
                Future<?> future = pending.future;
                if (future != null) {
                    future.cancel(false);
                }
                NativeMethods.onCompressImageResult(sessionId,
                        CallbackCorrelation.failure(
                                pending.requestId, "compressImage", "session closed"));
            }
        }
        pendingCompressions.clear();
    }

    public void destroy() {
        cancelPendingCompression();
    }

    // ========================================================================
    // Internal: chooseImage result handling
    // ========================================================================

    private void handleChooseImageResult(PickerRequest request, Intent data) {
        try {
            List<String> paths = new ArrayList<>();
            List<Long> sizes = new ArrayList<>();

            if (data.getClipData() != null) {
                int clipCount = Math.min(data.getClipData().getItemCount(), request.count);
                for (int i = 0; i < clipCount; i++) {
                    Uri uri = data.getClipData().getItemAt(i).getUri();
                    String path = copyUriToTemp(uri, "chooseimg", ".jpg");
                    if (path != null) {
                        paths.add(path);
                        sizes.add(new File(path).length());
                    }
                }
            } else if (data.getData() != null) {
                String path = copyUriToTemp(data.getData(), "chooseimg", ".jpg");
                if (path != null) {
                    paths.add(path);
                    sizes.add(new File(path).length());
                }
            }

            if (paths.isEmpty()) {
                sendChooseImageError(request.requestId, "no image selected");
                return;
            }

            NativeMethods.onChooseImageResult(sessionId,
                    chooseImageResultJson(request.requestId, paths, sizes));
        } catch (Exception e) {
            Log.e(TAG, "handleChooseImageResult error", e);
            sendChooseImageError(request.requestId, e.getMessage());
        }
    }

    private void handleCaptureImageResult(PickerRequest request) {
        try {
            Uri captured = request.cameraOutput;
            if (captured == null) {
                sendChooseImageError(request.requestId, "no capture uri");
                return;
            }

            String path = copyUriToTemp(captured, "capture", ".jpg");
            request.cameraOutput = null;

            if (path == null) {
                sendChooseImageError(request.requestId, "copy failed");
                return;
            }

            NativeMethods.onChooseImageResult(sessionId, chooseImageResultJson(
                    request.requestId,
                    Collections.singletonList(path),
                    Collections.singletonList(new File(path).length())));
        } catch (Exception e) {
            Log.e(TAG, "handleCaptureImageResult error", e);
            sendChooseImageError(request.requestId, e.getMessage());
        }
    }

    // ========================================================================
    // Internal: chooseMessageFile result handling
    // ========================================================================

    private void handleChooseFileResult(PickerRequest request, Intent data) {
        try {
            List<JSONObject> files = new ArrayList<>();

            if (data.getClipData() != null) {
                int clipCount = data.getClipData().getItemCount();
                for (int i = 0; i < clipCount; i++) {
                    Uri uri = data.getClipData().getItemAt(i).getUri();
                    JSONObject info = resolveFileInfo(uri);
                    if (info != null) {
                        files.add(info);
                    }
                }
            } else if (data.getData() != null) {
                JSONObject info = resolveFileInfo(data.getData());
                if (info != null) {
                    files.add(info);
                }
            }

            if (files.isEmpty()) {
                sendChooseMessageFileError(request.requestId, "no file selected");
                return;
            }

            NativeMethods.onChooseMessageFileResult(sessionId,
                    chooseMessageFileResultJson(request.requestId, files));
        } catch (Exception e) {
            Log.e(TAG, "handleChooseFileResult error", e);
            sendChooseMessageFileError(request.requestId, e.getMessage());
        }
    }

    /**
     * Resolve file info from a content URI: copy to temp, get name/size/type.
     */
    private JSONObject resolveFileInfo(Uri uri) {
        try {
            Activity activity = getActivity();
            if (activity == null) return null;
            ContentResolver resolver = activity.getContentResolver();
            String name = "unknown";
            long size = 0;

            try (Cursor cursor = resolver.query(uri, null, null, null, null)) {
                if (cursor != null && cursor.moveToFirst()) {
                    int nameIdx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                    int sizeIdx = cursor.getColumnIndex(OpenableColumns.SIZE);
                    if (nameIdx >= 0) name = cursor.getString(nameIdx);
                    if (sizeIdx >= 0) size = cursor.getLong(sizeIdx);
                }
            }

            // Determine type from MIME
            String mime = resolver.getType(uri);
            String fileType = "file";
            if (mime != null) {
                if (mime.startsWith("image/")) fileType = "image";
                else if (mime.startsWith("video/")) fileType = "video";
            }

            // Determine extension
            String ext = "";
            int dotIdx = name.lastIndexOf('.');
            if (dotIdx > 0) {
                ext = name.substring(dotIdx);
            } else if (mime != null) {
                String guessed = MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
                if (guessed != null) ext = "." + guessed;
            }

            String tempPath = copyUriToTemp(uri, "msgfile", ext.isEmpty() ? ".tmp" : ext);
            if (tempPath == null) return null;

            if (size == 0) {
                size = new File(tempPath).length();
            }

            JSONObject info = new JSONObject();
            info.put("path", tempPath);
            info.put("size", size);
            info.put("name", name);
            info.put("type", fileType);
            info.put("time", System.currentTimeMillis() / 1000);
            return info;
        } catch (Exception e) {
            Log.e(TAG, "resolveFileInfo error", e);
            return null;
        }
    }

    // ========================================================================
    // Intent launchers
    // ========================================================================

    private void launchImagePicker(PickerRequest request) {
        Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
        intent.setType("image/*");
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        if (request.count > 1) {
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        }
        Activity activity = getActivity();
        if (activity == null) {
            sendChooseImageError(request.requestId, "activity is gone");
            return;
        }
        ResultProxyActivity.launch(activity,
                Intent.createChooser(intent, "Choose Image"),
                REQUEST_CHOOSE_IMAGE,
                (code, resultCode, data) -> onPickImageResult(request, resultCode, data));
    }

    private void launchCameraCapture(PickerRequest request) {
        Intent intent = new Intent(MediaStore.ACTION_IMAGE_CAPTURE);
        Activity activity = getActivity();
        if (activity == null) {
            sendChooseImageError(request.requestId, "activity is gone");
            return;
        }
        if (intent.resolveActivity(activity.getPackageManager()) == null) {
            sendChooseImageError(request.requestId, "no camera app");
            return;
        }

        try {
            // Use MediaStore to create a URI the camera app can write to
            ContentValues values = new ContentValues();
            values.put(MediaStore.Images.Media.DISPLAY_NAME, "capture_" + System.currentTimeMillis() + ".jpg");
            values.put(MediaStore.Images.Media.MIME_TYPE, "image/jpeg");
            Uri output = activity.getContentResolver().insert(
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
            if (output == null) {
                sendChooseImageError(request.requestId, "cannot create capture uri");
                return;
            }
            request.cameraOutput = output;
            intent.putExtra(MediaStore.EXTRA_OUTPUT, output);
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            ResultProxyActivity.launch(activity, intent, REQUEST_CAPTURE_IMAGE,
                    (code, resultCode, data) -> onCaptureImageResult(request, resultCode));
        } catch (Exception e) {
            Log.e(TAG, "launchCameraCapture error", e);
            sendChooseImageError(request.requestId, e.getMessage());
        }
    }

    // ========================================================================
    // Utility
    // ========================================================================

    private File createTempFile(String prefix, String extension) throws IOException {
        Activity activity = getActivity();
        if (activity == null) {
            throw new IOException("activity is gone");
        }
        File cacheDir = new File(activity.getCacheDir(), "image_api");
        if (!cacheDir.exists()) {
            cacheDir.mkdirs();
        }
        return new File(cacheDir, prefix + "_" + System.currentTimeMillis() + extension);
    }

    /**
     * Copy content from a Uri to a temp file and return the temp path.
     */
    private String copyUriToTemp(Uri uri, String prefix, String ext) {
        try {
            File tempFile = createTempFile(prefix, ext);
            Activity activity = getActivity();
            if (activity == null) return null;
            try (InputStream in = activity.getContentResolver().openInputStream(uri);
                 FileOutputStream out = new FileOutputStream(tempFile)) {
                if (in == null) return null;
                byte[] buf = new byte[8192];
                int len;
                while ((len = in.read(buf)) > 0) {
                    out.write(buf, 0, len);
                }
            }
            return tempFile.getAbsolutePath();
        } catch (Exception e) {
            Log.e(TAG, "copyUriToTemp error", e);
            return null;
        }
    }

    private Uri resolveUri(String url) {
        if (url.startsWith("content://") || url.startsWith("file://")) {
            return Uri.parse(url);
        }
        // Local file path
        File f = new File(url);
        if (f.exists()) {
            return getFileUri(f);
        }
        // Treat as a URL (http/https)
        return Uri.parse(url);
    }

    /**
     * Get a URI for a local file that is safe to pass to external apps.
     * file:// URIs cause FileUriExposedException on supported devices, so
     * we use MediaStore to obtain a content:// URI instead. Returns null
     * if the content URI cannot be created.
     */
    private Uri getFileUri(File file) {
        Activity activity = getActivity();
        if (activity == null) return null;
        try {
            ContentValues values = new ContentValues();
            values.put(MediaStore.Images.Media.DATA, file.getAbsolutePath());
            values.put(MediaStore.Images.Media.MIME_TYPE, "image/*");
            Uri uri = activity.getContentResolver().insert(
                    MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
            if (uri != null) {
                return uri;
            }
        } catch (Exception e) {
            Log.w(TAG, "getFileUri: MediaStore insert failed", e);
        }
        // Do not fall back to Uri.fromFile; let the caller handle the failure.
        return null;
    }

    private int calculateInSampleSize(int srcW, int srcH, int reqW, int reqH) {
        int inSampleSize = 1;
        if (srcH > reqH || srcW > reqW) {
            int halfH = srcH / 2;
            int halfW = srcW / 2;
            while ((halfH / inSampleSize) >= reqH && (halfW / inSampleSize) >= reqW) {
                inSampleSize *= 2;
            }
        }
        return inSampleSize;
    }

    private void sendChooseImageError(int requestId, String reason) {
        NativeMethods.onChooseImageResult(sessionId,
                CallbackCorrelation.failure(requestId, "chooseImage", reason));
    }

    private void sendChooseMessageFileError(int requestId, String reason) {
        NativeMethods.onChooseMessageFileResult(sessionId,
                CallbackCorrelation.failure(requestId, "chooseMessageFile", reason));
    }

    // ========================================================================
    // Result documents
    //
    // Static and free of Android types on purpose: whether a result answers the
    // request that asked for it is a property of the JSON, so it is decided
    // where a test can read it, not inside a picker callback.
    // ========================================================================

    /** The reply to a {@code chooseImage} request, for album and camera alike. */
    static String chooseImageResultJson(int requestId, List<String> paths, List<Long> sizes)
            throws JSONException {
        JSONObject result = new JSONObject();
        JSONArray tempFilePaths = new JSONArray();
        JSONArray tempFiles = new JSONArray();
        for (int i = 0; i < paths.size(); i++) {
            tempFilePaths.put(paths.get(i));
            JSONObject file = new JSONObject();
            file.put("path", paths.get(i));
            file.put("size", sizes.get(i).longValue());
            tempFiles.put(file);
        }
        result.put("tempFilePaths", tempFilePaths);
        result.put("tempFiles", tempFiles);
        CallbackCorrelation.stamp(result, requestId);
        return result.toString();
    }

    /** The reply to a {@code chooseMessageFile} request. */
    static String chooseMessageFileResultJson(int requestId, List<JSONObject> files)
            throws JSONException {
        JSONObject result = new JSONObject();
        JSONArray tempFiles = new JSONArray();
        for (JSONObject file : files) {
            tempFiles.put(file);
        }
        result.put("tempFiles", tempFiles);
        CallbackCorrelation.stamp(result, requestId);
        return result.toString();
    }

    /** The reply to a {@code compressImage} request. */
    static String compressImageResultJson(int requestId, String tempFilePath)
            throws JSONException {
        JSONObject result = new JSONObject();
        result.put("tempFilePath", tempFilePath);
        CallbackCorrelation.stamp(result, requestId);
        return result.toString();
    }

    private static boolean jsonArrayContains(JSONArray arr, String value) {
        if (arr == null) return false;
        for (int i = 0; i < arr.length(); i++) {
            if (value.equals(arr.optString(i))) return true;
        }
        return false;
    }
}
