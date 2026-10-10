package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.media.MediaMetadataRetriever;
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
import com.migo.runtime.internal.LentFileProvider;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ResultProxyActivity;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.ref.WeakReference;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Future;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.Semaphore;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * The image and media APIs for one session.
 *
 * <p>Every request carries the runtime's {@code requestId} and is answered exactly
 * once through its {@link NativeMethods} callback -- with the result, or with the
 * reason it failed. The runtime waits on a picker or a viewer without a timeout,
 * so a request that is never answered is a call that never returns.
 *
 * <p>Paths in a request are real paths the runtime resolved from the game's
 * sandbox, readable until the request is answered. A file a result names is
 * handed over: the runtime moves it into the session's {@code /tmp}. So every
 * such file is one this manager created for that request, in its own per-session
 * directory, and nothing here touches it again.
 *
 * <p>File work -- copying a picked item, writing the album, reading video
 * metadata -- runs on {@link #IMAGE_WORKERS}, never on the main thread.
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
    private static final int REQUEST_CHOOSE_MEDIA = 9004;
    private static final int REQUEST_CAPTURE_VIDEO = 9005;

    /** Where this SDK stages the files it hands over, per session. */
    private static final String STAGING_DIR = "migo-image-api";

    /** The request each callback answers, and how it reaches the runtime. */
    enum Api {
        SAVE_IMAGE("saveImageToPhotosAlbum"),
        PREVIEW_IMAGE("previewImage"),
        PREVIEW_MEDIA("previewMedia"),
        COMPRESS_IMAGE("compressImage"),
        CHOOSE_IMAGE("chooseImage"),
        CHOOSE_MESSAGE_FILE("chooseMessageFile"),
        CHOOSE_MEDIA("chooseMedia");

        final String apiName;

        Api(String apiName) {
            this.apiName = apiName;
        }

        void answer(int sessionId, String resultJson) {
            switch (this) {
                case SAVE_IMAGE:
                    NativeMethods.onSaveImageToPhotosAlbumResult(sessionId, resultJson);
                    break;
                case PREVIEW_IMAGE:
                    NativeMethods.onPreviewImageResult(sessionId, resultJson);
                    break;
                case PREVIEW_MEDIA:
                    NativeMethods.onPreviewMediaResult(sessionId, resultJson);
                    break;
                case COMPRESS_IMAGE:
                    NativeMethods.onCompressImageResult(sessionId, resultJson);
                    break;
                case CHOOSE_IMAGE:
                    NativeMethods.onChooseImageResult(sessionId, resultJson);
                    break;
                case CHOOSE_MESSAGE_FILE:
                    NativeMethods.onChooseMessageFileResult(sessionId, resultJson);
                    break;
                case CHOOSE_MEDIA:
                    NativeMethods.onChooseMediaResult(sessionId, resultJson);
                    break;
            }
        }
    }

    private final int sessionId;
    private final WeakReference<Activity> activityRef;
    private final Context appContext;
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
        final Api api;
        /** The runtime's correlation id, or {@link CallbackCorrelation#ABSENT}. */
        final int requestId;
        /** The most items the request may return. */
        final int count;
        /** Media kinds a {@code chooseMedia} request accepts. */
        boolean images = true;
        boolean videos = false;
        /** File extensions a {@code chooseMessageFile} request accepts, lower case; empty is any. */
        Set<String> extensions = Collections.emptySet();
        /** The file a camera app was told to write, and the URI it was lent through. */
        File captureFile;
        Uri captureUri;

        PickerRequest(Api api, int requestId, int count) {
            this.api = api;
            this.requestId = requestId;
            this.count = Math.max(1, count);
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
        this.appContext = activity.getApplicationContext();
        this.mainHandler = new Handler(Looper.getMainLooper());
    }

    private Activity getActivity() {
        return activityRef.get();
    }

    private void fail(Api api, int requestId, String reason) {
        api.answer(sessionId, CallbackCorrelation.failure(requestId, api.apiName, reason));
    }

    private void succeed(Api api, int requestId) {
        api.answer(sessionId, doneJson(requestId));
    }

    /** Run {@code work} off the main thread, or answer that it could not be queued. */
    private void submit(Api api, int requestId, Runnable work) {
        try {
            IMAGE_WORKERS.execute(() -> {
                try {
                    work.run();
                } catch (RuntimeException unexpected) {
                    Log.e(TAG, api.apiName + " failed", unexpected);
                    fail(api, requestId, String.valueOf(unexpected.getMessage()));
                }
            });
        } catch (RejectedExecutionException busy) {
            fail(api, requestId, "busy");
        }
    }

    // ==================== saveImageToPhotosAlbum ====================

    /** Request: {@code {"requestId", "filePath"}}. */
    public void saveToPhotosAlbum(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        final String filePath;
        try {
            filePath = new JSONObject(optionsJson).getString("filePath");
        } catch (JSONException malformed) {
            fail(Api.SAVE_IMAGE, requestId, malformed.getMessage());
            return;
        }
        submit(Api.SAVE_IMAGE, requestId, () -> {
            try {
                writeToAlbum(new File(filePath));
                succeed(Api.SAVE_IMAGE, requestId);
            } catch (IOException e) {
                fail(Api.SAVE_IMAGE, requestId, e.getMessage());
            }
        });
    }

    private void writeToAlbum(File source) throws IOException {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(source.getAbsolutePath(), bounds);
        String mimeType = bounds.outMimeType;
        if (mimeType == null) {
            throw new IOException("not an image");
        }
        String extension = MimeTypeMap.getSingleton().getExtensionFromMimeType(mimeType);

        ContentResolver resolver = appContext.getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.DISPLAY_NAME, "IMG_" + System.currentTimeMillis()
                + (extension != null ? "." + extension : ""));
        values.put(MediaStore.Images.Media.MIME_TYPE, mimeType);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            values.put(MediaStore.Images.Media.RELATIVE_PATH, Environment.DIRECTORY_PICTURES);
            values.put(MediaStore.Images.Media.IS_PENDING, 1);
        }
        Uri uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
        if (uri == null) {
            throw new IOException("the album refused the image");
        }
        try (InputStream in = new FileInputStream(source);
             OutputStream out = resolver.openOutputStream(uri)) {
            if (out == null) {
                throw new IOException("cannot open the album entry");
            }
            copy(in, out);
        } catch (IOException | RuntimeException e) {
            resolver.delete(uri, null, null);
            throw e;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            values.clear();
            values.put(MediaStore.Images.Media.IS_PENDING, 0);
            resolver.update(uri, values, null, null);
        }
    }

    // ==================== previewImage / previewMedia ====================

    /**
     * Request: {@code {"requestId", "urls", "current"}}. The system viewer shows
     * {@code current}; answered once it is launched.
     */
    public void previewImage(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        try {
            JSONObject opts = new JSONObject(optionsJson);
            JSONArray urls = opts.getJSONArray("urls");
            if (urls.length() == 0) {
                fail(Api.PREVIEW_IMAGE, requestId, "urls is empty");
                return;
            }
            String current = opts.optString("current", "");
            preview(Api.PREVIEW_IMAGE, requestId,
                    current.isEmpty() ? urls.getString(0) : current, "image");
        } catch (JSONException malformed) {
            fail(Api.PREVIEW_IMAGE, requestId, malformed.getMessage());
        }
    }

    /**
     * Request: {@code {"requestId", "sources": [{"url", "type"}], "current"}}. The
     * system viewer shows {@code sources[current]}; answered once it is launched.
     */
    public void previewMedia(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        try {
            JSONObject opts = new JSONObject(optionsJson);
            JSONArray sources = opts.getJSONArray("sources");
            if (sources.length() == 0) {
                fail(Api.PREVIEW_MEDIA, requestId, "sources is empty");
                return;
            }
            int current = opts.optInt("current", 0);
            if (current < 0 || current >= sources.length()) {
                current = 0;
            }
            JSONObject item = sources.getJSONObject(current);
            preview(Api.PREVIEW_MEDIA, requestId, item.getString("url"),
                    "video".equals(item.optString("type")) ? "video" : "image");
        } catch (JSONException malformed) {
            fail(Api.PREVIEW_MEDIA, requestId, malformed.getMessage());
        }
    }

    private void preview(Api api, int requestId, String target, String kind) {
        if (target.startsWith("http://") || target.startsWith("https://")) {
            mainHandler.post(() -> view(api, requestId, Uri.parse(target), null));
            return;
        }
        // A local file is lent to the viewer, which opens it after this request
        // is answered -- past the point the runtime keeps the path readable.
        submit(api, requestId, () -> {
            try {
                File file = new File(target);
                Uri uri = LentFileProvider.lend(appContext, sessionId, file);
                String mime = mimeOf(file.getName());
                mainHandler.post(() -> view(api, requestId, uri,
                        mime != null ? mime : kind + "/*"));
            } catch (IOException e) {
                fail(api, requestId, e.getMessage());
            }
        });
    }

    private void view(Api api, int requestId, Uri uri, String mime) {
        Activity activity = getActivity();
        if (activity == null) {
            fail(api, requestId, "no activity to show it in");
            return;
        }
        Intent intent = new Intent(Intent.ACTION_VIEW);
        if (mime != null) {
            intent.setDataAndType(uri, mime);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } else {
            intent.setData(uri);
        }
        try {
            activity.startActivity(intent);
            succeed(api, requestId);
        } catch (ActivityNotFoundException none) {
            fail(api, requestId, "no app can show it");
        }
    }

    // ==================== compressImage ====================

    /**
     * Request: {@code {"requestId", "src", "quality", "compressedWidth",
     * "compressedHeight"}}. Answers {@code {"tempFilePath"}}.
     */
    public void compressAsync(final String optionsJson) {
        final JSONObject opts;
        try {
            opts = new JSONObject(optionsJson);
        } catch (JSONException malformed) {
            fail(Api.COMPRESS_IMAGE, CallbackCorrelation.ABSENT, malformed.getMessage());
            return;
        }
        final int requestId = CallbackCorrelation.requestIdOf(opts);
        final PendingCompression pending = new PendingCompression(requestId);
        pendingCompressions.add(pending);
        final Runnable work = () -> {
            try {
                if (!pending.settled.get()) {
                    String result = compressSync(opts, requestId);
                    if (pending.settled.compareAndSet(false, true)) {
                        Api.COMPRESS_IMAGE.answer(sessionId, result);
                    }
                }
            } catch (Exception e) {
                if (pending.settled.compareAndSet(false, true)) {
                    fail(Api.COMPRESS_IMAGE, requestId, e.getMessage());
                }
            } finally {
                pendingCompressions.remove(pending);
            }
        };
        try {
            pending.future = IMAGE_WORKERS.submit(work);
        } catch (RejectedExecutionException rejected) {
            pendingCompressions.remove(pending);
            fail(Api.COMPRESS_IMAGE, requestId, "queue full");
        }
    }

    private String compressSync(JSONObject opts, int requestId) throws Exception {
        String src = opts.getString("src");
        int quality = opts.optInt("quality", 80);
        int targetWidth = opts.optInt("compressedWidth", 0);
        int targetHeight = opts.optInt("compressedHeight", 0);

        quality = Math.max(0, Math.min(100, quality));

        File srcFile = new File(src);
        if (!srcFile.isFile()) {
            throw new IOException("file not found");
        }

        // First pass: get original dimensions
        BitmapFactory.Options bmOpts = new BitmapFactory.Options();
        bmOpts.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(src, bmOpts);
        int origWidth = bmOpts.outWidth;
        int origHeight = bmOpts.outHeight;

        if (origWidth <= 0 || origHeight <= 0) {
            throw new IOException("invalid image");
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
            throw new IOException("image too large");
        }
        int reservedPixels = (int) requiredPixels;
        if (!tryReserveDecodedPixels(reservedPixels)) {
            throw new IOException("image pixel budget exhausted");
        }

        Bitmap bitmap = null;
        try {
            bitmap = BitmapFactory.decodeFile(src, bmOpts);
            if (bitmap == null) {
                throw new IOException("decode failed");
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

            File tempFile = stagingFile("compress", ext);
            try (FileOutputStream fos = new FileOutputStream(tempFile)) {
                if (!bitmap.compress(format, quality, fos)) {
                    throw new IOException("compress failed");
                }
            } catch (IOException | RuntimeException e) {
                //noinspection ResultOfMethodCallIgnored
                tempFile.delete();
                throw e;
            }
            return compressImageResultJson(requestId, tempFile.getAbsolutePath());
        } finally {
            if (bitmap != null && !bitmap.isRecycled()) {
                bitmap.recycle();
            }
            releaseDecodedPixels(reservedPixels);
        }
    }

    // ==================== chooseMessageFile ====================

    /** Request: {@code {"requestId", "count", "type", "extension"}}. */
    public void chooseMessageFile(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        final PickerRequest request;
        final Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        try {
            JSONObject opts = new JSONObject(optionsJson);
            request = new PickerRequest(Api.CHOOSE_MESSAGE_FILE, requestId, opts.optInt("count", 1));
            String type = opts.optString("type", "all");
            if ("image".equals(type)) {
                intent.setType("image/*");
            } else if ("video".equals(type)) {
                intent.setType("video/*");
            } else {
                intent.setType("*/*");
            }
            JSONArray extension = opts.optJSONArray("extension");
            if ("file".equals(type) && extension != null && extension.length() > 0) {
                Set<String> accepted = new HashSet<>();
                List<String> mimes = new ArrayList<>();
                for (int i = 0; i < extension.length(); i++) {
                    String ext = extension.optString(i).toLowerCase(Locale.ROOT);
                    if (ext.startsWith(".")) {
                        ext = ext.substring(1);
                    }
                    if (ext.isEmpty()) continue;
                    accepted.add(ext);
                    String mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
                    if (mime != null) mimes.add(mime);
                }
                request.extensions = accepted;
                // Only when every extension names a type: a picker filtered by
                // some of them would hide the files the rest accept.
                if (!mimes.isEmpty() && mimes.size() == accepted.size()) {
                    intent.putExtra(Intent.EXTRA_MIME_TYPES, mimes.toArray(new String[0]));
                }
            }
        } catch (JSONException malformed) {
            fail(Api.CHOOSE_MESSAGE_FILE, requestId, malformed.getMessage());
            return;
        }
        if (request.count > 1) {
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        }
        launchPicker(request, Intent.createChooser(intent, null), REQUEST_CHOOSE_FILE);
    }

    // ==================== chooseImage ====================

    /** Request: {@code {"requestId", "count", "sizeType", "sourceType"}}. */
    public void chooseImage(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        final PickerRequest request;
        final boolean cameraOnly;
        try {
            JSONObject opts = new JSONObject(optionsJson);
            request = new PickerRequest(Api.CHOOSE_IMAGE, requestId, opts.optInt("count", 9));
            cameraOnly = cameraOnly(opts.optJSONArray("sourceType"));
        } catch (JSONException malformed) {
            fail(Api.CHOOSE_IMAGE, requestId, malformed.getMessage());
            return;
        }
        if (cameraOnly) {
            launchCapture(request, false, 0);
            return;
        }
        Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
        intent.setType("image/*");
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        if (request.count > 1) {
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        }
        launchPicker(request, Intent.createChooser(intent, null), REQUEST_CHOOSE_IMAGE);
    }

    // ==================== chooseMedia ====================

    /**
     * Request: {@code {"requestId", "count", "mediaType", "sourceType",
     * "maxDuration", "sizeType", "camera"}}. Android's system camera app chooses
     * the lens itself, so {@code camera} is not applied.
     */
    public void chooseMedia(String optionsJson) {
        final int requestId = CallbackCorrelation.requestIdOf(optionsJson);
        final PickerRequest request;
        final boolean cameraOnly;
        final int maxDuration;
        try {
            JSONObject opts = new JSONObject(optionsJson);
            request = new PickerRequest(Api.CHOOSE_MEDIA, requestId, opts.optInt("count", 9));
            JSONArray mediaType = opts.optJSONArray("mediaType");
            boolean mix = jsonArrayContains(mediaType, "mix");
            request.images = mediaType == null || mix || jsonArrayContains(mediaType, "image");
            request.videos = mediaType == null || mix || jsonArrayContains(mediaType, "video");
            if (!request.images && !request.videos) {
                request.images = true;
                request.videos = true;
            }
            cameraOnly = cameraOnly(opts.optJSONArray("sourceType"));
            maxDuration = Math.max(3, Math.min(60, opts.optInt("maxDuration", 10)));
        } catch (JSONException malformed) {
            fail(Api.CHOOSE_MEDIA, requestId, malformed.getMessage());
            return;
        }
        if (cameraOnly) {
            // A camera app takes one kind at a time: a photo when one is allowed.
            launchCapture(request, !request.images, maxDuration);
            return;
        }
        Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        if (request.images && request.videos) {
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_MIME_TYPES, new String[] {"image/*", "video/*"});
        } else {
            intent.setType(request.images ? "image/*" : "video/*");
        }
        if (request.count > 1) {
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        }
        launchPicker(request, Intent.createChooser(intent, null), REQUEST_CHOOSE_MEDIA);
    }

    // ==================== Launching ====================

    private void launchPicker(PickerRequest request, Intent chooser, int requestCode) {
        mainHandler.post(() -> {
            Activity activity = getActivity();
            if (activity == null) {
                fail(request.api, request.requestId, "no activity to show it in");
                return;
            }
            try {
                ResultProxyActivity.launch(activity, chooser, requestCode,
                        (code, resultCode, data) -> onPicked(request, resultCode, data));
            } catch (RuntimeException e) {
                Log.e(TAG, request.api.apiName + ": picker not launched", e);
                fail(request.api, request.requestId, String.valueOf(e.getMessage()));
            }
        });
    }

    /**
     * Have a camera app write a photo -- or, for {@code video}, a recording of at
     * most {@code maxDuration} seconds -- to a file of this session's, lent to it
     * for the capture. It does not go to the player's gallery.
     */
    private void launchCapture(PickerRequest request, boolean video, int maxDuration) {
        submit(request.api, request.requestId, () -> {
            try {
                request.captureFile = stagingFile(video ? "video" : "capture", video ? ".mp4" : ".jpg");
                request.captureUri =
                        LentFileProvider.lendForCapture(appContext, sessionId, request.captureFile);
            } catch (IOException e) {
                fail(request.api, request.requestId, e.getMessage());
                return;
            }
            Intent intent = new Intent(
                    video ? MediaStore.ACTION_VIDEO_CAPTURE : MediaStore.ACTION_IMAGE_CAPTURE);
            intent.putExtra(MediaStore.EXTRA_OUTPUT, request.captureUri);
            intent.setClipData(ClipData.newRawUri("capture", request.captureUri));
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_READ_URI_PERMISSION);
            if (video) {
                intent.putExtra(MediaStore.EXTRA_DURATION_LIMIT, maxDuration);
            }
            mainHandler.post(() -> {
                Activity activity = getActivity();
                if (activity == null) {
                    endCapture(request);
                    fail(request.api, request.requestId, "no activity to show it in");
                    return;
                }
                try {
                    ResultProxyActivity.launch(activity, intent,
                            video ? REQUEST_CAPTURE_VIDEO : REQUEST_CAPTURE_IMAGE,
                            (code, resultCode, data) -> onCaptured(request, video, resultCode));
                } catch (RuntimeException e) {
                    endCapture(request);
                    fail(request.api, request.requestId,
                            e instanceof ActivityNotFoundException ? "no camera app"
                                    : String.valueOf(e.getMessage()));
                }
            });
        });
    }

    private void endCapture(PickerRequest request) {
        if (request.captureUri != null) {
            LentFileProvider.forget(request.captureUri);
            request.captureUri = null;
        }
    }

    // ==================== Results ====================

    private void onPicked(PickerRequest request, int resultCode, Intent data) {
        if (resultCode != Activity.RESULT_OK || data == null) {
            fail(request.api, request.requestId, "cancel");
            return;
        }
        final List<Uri> uris = new ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                Uri uri = clip.getItemAt(i).getUri();
                if (uri != null) uris.add(uri);
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        submit(request.api, request.requestId, () -> answerPicked(request, uris));
    }

    private void onCaptured(PickerRequest request, boolean video, int resultCode) {
        endCapture(request);
        final File captured = request.captureFile;
        if (resultCode != Activity.RESULT_OK || captured == null || captured.length() == 0) {
            if (captured != null) {
                //noinspection ResultOfMethodCallIgnored
                captured.delete();
            }
            fail(request.api, request.requestId, "cancel");
            return;
        }
        submit(request.api, request.requestId, () -> {
            try {
                switch (request.api) {
                    case CHOOSE_IMAGE:
                        request.api.answer(sessionId, chooseImageResultJson(request.requestId,
                                Collections.singletonList(captured.getAbsolutePath()),
                                Collections.singletonList(captured.length())));
                        break;
                    case CHOOSE_MEDIA:
                        JSONObject item = video ? describeVideo(captured) : describeImage(captured);
                        request.api.answer(sessionId, chooseMediaResultJson(request.requestId,
                                Collections.singletonList(item)));
                        break;
                    default:
                        fail(request.api, request.requestId, "not a capture request");
                }
            } catch (IOException | JSONException e) {
                fail(request.api, request.requestId, e.getMessage());
            }
        });
    }

    private void answerPicked(PickerRequest request, List<Uri> uris) {
        ContentResolver resolver = appContext.getContentResolver();
        List<File> staged = new ArrayList<>();
        try {
            switch (request.api) {
                case CHOOSE_IMAGE: {
                    List<String> paths = new ArrayList<>();
                    List<Long> sizes = new ArrayList<>();
                    for (Uri uri : uris) {
                        if (paths.size() == request.count) break;
                        String mime = resolver.getType(uri);
                        if (mime != null && !mime.startsWith("image/")) continue;
                        File file = stage(uri, "image", extensionFor(mime, ".jpg"));
                        staged.add(file);
                        paths.add(file.getAbsolutePath());
                        sizes.add(file.length());
                    }
                    if (paths.isEmpty()) {
                        fail(request.api, request.requestId, "no image selected");
                        return;
                    }
                    request.api.answer(sessionId,
                            chooseImageResultJson(request.requestId, paths, sizes));
                    break;
                }
                case CHOOSE_MESSAGE_FILE: {
                    List<JSONObject> files = new ArrayList<>();
                    for (Uri uri : uris) {
                        if (files.size() == request.count) break;
                        JSONObject info = stageMessageFile(request, resolver, uri, staged);
                        if (info != null) files.add(info);
                    }
                    if (files.isEmpty()) {
                        fail(request.api, request.requestId, "no file selected");
                        return;
                    }
                    request.api.answer(sessionId,
                            chooseMessageFileResultJson(request.requestId, files));
                    break;
                }
                case CHOOSE_MEDIA: {
                    List<JSONObject> items = new ArrayList<>();
                    for (Uri uri : uris) {
                        if (items.size() == request.count) break;
                        String mime = resolver.getType(uri);
                        boolean video = mime != null && mime.startsWith("video/");
                        boolean image = mime != null && mime.startsWith("image/");
                        if (!(video && request.videos) && !(image && request.images)) continue;
                        File file = stage(uri, video ? "video" : "image",
                                extensionFor(mime, video ? ".mp4" : ".jpg"));
                        staged.add(file);
                        JSONObject item = video ? describeVideo(file) : describeImage(file);
                        if (item.has("thumbTempFilePath")) {
                            staged.add(new File(item.getString("thumbTempFilePath")));
                        }
                        items.add(item);
                    }
                    if (items.isEmpty()) {
                        fail(request.api, request.requestId, "nothing selected");
                        return;
                    }
                    request.api.answer(sessionId, chooseMediaResultJson(request.requestId, items));
                    break;
                }
                default:
                    fail(request.api, request.requestId, "not a picker request");
            }
        } catch (IOException | JSONException e) {
            // Nothing was handed over, so what was staged is still this SDK's.
            for (File file : staged) {
                //noinspection ResultOfMethodCallIgnored
                file.delete();
            }
            fail(request.api, request.requestId, e.getMessage());
        }
    }

    private JSONObject stageMessageFile(PickerRequest request, ContentResolver resolver, Uri uri,
                                        List<File> staged) throws IOException, JSONException {
        String name = "file";
        long size = -1;
        try (Cursor cursor = resolver.query(uri, null, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int nameIdx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                int sizeIdx = cursor.getColumnIndex(OpenableColumns.SIZE);
                if (nameIdx >= 0 && cursor.getString(nameIdx) != null) name = cursor.getString(nameIdx);
                if (sizeIdx >= 0 && !cursor.isNull(sizeIdx)) size = cursor.getLong(sizeIdx);
            }
        }
        String mime = resolver.getType(uri);
        int dot = name.lastIndexOf('.');
        String ext = dot > 0 ? name.substring(dot).toLowerCase(Locale.ROOT) : extensionFor(mime, "");
        if (!request.extensions.isEmpty()
                && !request.extensions.contains(ext.startsWith(".") ? ext.substring(1) : ext)) {
            return null;
        }
        File file = stage(uri, "file", ext);
        staged.add(file);
        String fileType = "file";
        if (mime != null && mime.startsWith("image/")) fileType = "image";
        else if (mime != null && mime.startsWith("video/")) fileType = "video";
        JSONObject info = new JSONObject();
        info.put("path", file.getAbsolutePath());
        info.put("size", size >= 0 ? size : file.length());
        info.put("name", name);
        info.put("type", fileType);
        info.put("time", System.currentTimeMillis() / 1000);
        return info;
    }

    /** {@code width}/{@code height} and the rest of a {@code chooseMedia} image item. */
    private static JSONObject describeImage(File file) throws JSONException {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(file.getAbsolutePath(), bounds);
        JSONObject item = new JSONObject();
        item.put("tempFilePath", file.getAbsolutePath());
        item.put("size", file.length());
        item.put("width", Math.max(0, bounds.outWidth));
        item.put("height", Math.max(0, bounds.outHeight));
        item.put("fileType", "image");
        return item;
    }

    /** A video item: its duration, displayed size, and a cover frame of its own. */
    private JSONObject describeVideo(File file) throws IOException, JSONException {
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        try {
            retriever.setDataSource(file.getAbsolutePath());
            long durationMs = parseLong(
                    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION));
            int width = (int) parseLong(
                    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH));
            int height = (int) parseLong(
                    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT));
            long rotation = parseLong(
                    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION));
            if (rotation == 90 || rotation == 270) {
                int swap = width;
                width = height;
                height = swap;
            }
            JSONObject item = new JSONObject();
            item.put("tempFilePath", file.getAbsolutePath());
            item.put("size", file.length());
            item.put("duration", durationMs / 1000.0);
            item.put("width", width);
            item.put("height", height);
            item.put("fileType", "video");
            Bitmap frame = retriever.getFrameAtTime(0);
            if (frame != null) {
                File thumb = stagingFile("thumb", ".jpg");
                try (FileOutputStream out = new FileOutputStream(thumb)) {
                    frame.compress(Bitmap.CompressFormat.JPEG, 80, out);
                } finally {
                    frame.recycle();
                }
                item.put("thumbTempFilePath", thumb.getAbsolutePath());
            }
            return item;
        } catch (RuntimeException unreadable) {
            throw new IOException("unreadable video: " + unreadable.getMessage());
        } finally {
            try {
                retriever.release();
            } catch (IOException | RuntimeException ignored) {
                // Releasing a retriever that failed to open is not a second failure.
            }
        }
    }

    // ==================== Teardown ====================

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
                fail(Api.COMPRESS_IMAGE, pending.requestId, "session closed");
            }
        }
        pendingCompressions.clear();
    }

    public void destroy() {
        cancelPendingCompression();
        LentFileProvider.forgetSession(appContext, sessionId);
        File[] left = stagingDir().listFiles();
        if (left != null) {
            for (File file : left) {
                //noinspection ResultOfMethodCallIgnored
                file.delete();
            }
        }
        //noinspection ResultOfMethodCallIgnored
        stagingDir().delete();
    }

    // ==================== Files ====================

    private File stagingDir() {
        return new File(new File(appContext.getCacheDir(), STAGING_DIR), Integer.toString(sessionId));
    }

    /** A new, empty file of this session's to hand over. */
    private File stagingFile(String prefix, String extension) throws IOException {
        File dir = stagingDir();
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IOException("cannot create " + dir);
        }
        return File.createTempFile(prefix + "_", extension, dir);
    }

    /** Copy what {@code uri} names into a new staging file. */
    private File stage(Uri uri, String prefix, String extension) throws IOException {
        File file = stagingFile(prefix, extension);
        try (InputStream in = appContext.getContentResolver().openInputStream(uri);
             OutputStream out = new FileOutputStream(file)) {
            if (in == null) {
                throw new IOException("cannot read " + uri);
            }
            copy(in, out);
        } catch (IOException | RuntimeException e) {
            //noinspection ResultOfMethodCallIgnored
            file.delete();
            throw e;
        }
        return file;
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        int read;
        while ((read = in.read(buffer)) > 0) {
            out.write(buffer, 0, read);
        }
    }

    /** The extension for a MIME type, with its dot, or {@code fallback}. */
    static String extensionFor(String mime, String fallback) {
        if (mime == null) return fallback;
        String ext = MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
        return ext != null ? "." + ext : fallback;
    }

    private static String mimeOf(String name) {
        int dot = name.lastIndexOf('.');
        if (dot <= 0) return null;
        return MimeTypeMap.getSingleton()
                .getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase(Locale.ROOT));
    }

    private static long parseLong(String value) {
        if (value == null) return 0;
        try {
            return Long.parseLong(value.trim());
        } catch (NumberFormatException notANumber) {
            return 0;
        }
    }

    private static boolean cameraOnly(JSONArray sourceType) {
        return sourceType != null
                && jsonArrayContains(sourceType, "camera")
                && !jsonArrayContains(sourceType, "album");
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

    private static boolean jsonArrayContains(JSONArray arr, String value) {
        if (arr == null) return false;
        for (int i = 0; i < arr.length(); i++) {
            if (value.equals(arr.optString(i))) return true;
        }
        return false;
    }

    // ========================================================================
    // Result documents
    //
    // Static and free of Android types on purpose: whether a result answers the
    // request that asked for it is a property of the JSON, so it is decided
    // where a test can read it, not inside a picker callback.
    // ========================================================================

    /** The answer to a request whose result carries nothing but success. */
    static String doneJson(int requestId) {
        try {
            JSONObject result = new JSONObject();
            CallbackCorrelation.stamp(result, requestId);
            return result.toString();
        } catch (JSONException impossible) {
            return "{}";
        }
    }

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

    /**
     * The reply to a {@code chooseMedia} request: {@code type} is {@code image} or
     * {@code video} when every item is one, {@code mix} otherwise.
     */
    static String chooseMediaResultJson(int requestId, List<JSONObject> items)
            throws JSONException {
        boolean images = false;
        boolean videos = false;
        JSONArray tempFiles = new JSONArray();
        for (JSONObject item : items) {
            if ("video".equals(item.optString("fileType"))) {
                videos = true;
            } else {
                images = true;
            }
            tempFiles.put(item);
        }
        JSONObject result = new JSONObject();
        result.put("type", images && videos ? "mix" : videos ? "video" : "image");
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
}
