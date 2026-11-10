package com.migo.runtime.internal.platform;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.ImageFormat;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.graphics.SurfaceTexture;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Log;
import android.util.Size;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.Surface;
import android.view.TextureView;
import android.view.View;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.TextView;

import com.google.zxing.BarcodeFormat;
import com.migo.runtime.R;

import java.io.InputStream;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.EnumSet;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * scanCode's scanner: the back camera's picture under a frame, read for a code as it comes,
 * and -- unless content asked for the camera only -- a picture from the album read instead.
 *
 * <p>It is the SDK's own screen, as a system picker is the system's: content hears only the
 * code the player chose to scan, never the camera's picture, so it uses the app's camera grant
 * without content's {@code scope.camera}. Where the app has not been granted the camera it asks
 * the system, which it can only where the app declares the permission. Back, or a camera that
 * cannot be used when only the camera is allowed, ends it without a code.
 *
 * <p>Frames come from an {@link ImageReader} beside the preview and are read on a thread of
 * their own, one at a time, the newest first: a frame that arrives while one is being read
 * replaces the one waiting.
 *
 * @hide
 */
public final class ScanActivity extends Activity {

    private static final String TAG = "MigoScan";

    /** The kinds of code to read: {@link BarcodeFormat} names. */
    static final String EXTRA_FORMATS = "com.migo.runtime.scan.FORMATS";
    /** Whether only the camera may be scanned from (scanCode's {@code onlyFromCamera}). */
    static final String EXTRA_CAMERA_ONLY = "com.migo.runtime.scan.CAMERA_ONLY";
    /** The code read: its text, its kind as the reference names it, its bytes. */
    static final String RESULT_TEXT = "com.migo.runtime.scan.TEXT";
    static final String RESULT_SCAN_TYPE = "com.migo.runtime.scan.SCAN_TYPE";
    static final String RESULT_RAW_DATA = "com.migo.runtime.scan.RAW_DATA";
    /** Why it ended without a code, when it was not the player's Back. */
    static final String RESULT_ERROR = "com.migo.runtime.scan.ERROR";

    private static final int REQUEST_CAMERA = 1;
    private static final int REQUEST_ALBUM = 2;
    /** The frame's side, as a share of the camera image's shorter side and of the screen's. */
    private static final float FRAME_FRACTION = 0.7f;
    /** The largest camera image read: enough for a code across the frame, little to copy. */
    private static final int MAX_IMAGE_PIXELS = 1920 * 1080;
    /** The longest side an album picture is read at. */
    private static final int MAX_PICTURE_SIDE = 2048;

    private boolean cameraOnly;
    private EnumSet<BarcodeFormat> formats;
    private TextureView preview;
    private TextView hint;
    private final AtomicBoolean delivered = new AtomicBoolean();
    private boolean askedForCamera;

    private HandlerThread cameraThread;
    private Handler cameraHandler;
    private HandlerThread decodeThread;
    private Handler decodeHandler;
    private CodeDecoder decoder;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private ImageReader reader;
    private Size imageSize;
    /** A picture the player picked from the album, read once the decode thread runs again. */
    private Uri pendingPicture;
    /** The luminance of the frame being read, reused frame to frame. */
    private byte[] luminance = new byte[0];

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        cameraOnly = getIntent().getBooleanExtra(EXTRA_CAMERA_ONLY, false);
        formats = EnumSet.noneOf(BarcodeFormat.class);
        String[] names = getIntent().getStringArrayExtra(EXTRA_FORMATS);
        for (String name : names == null ? new String[0] : names) {
            try {
                formats.add(BarcodeFormat.valueOf(name));
            } catch (IllegalArgumentException unknown) {
                Log.w(TAG, "no such code kind: " + name);
            }
        }
        if (formats.isEmpty()) formats.addAll(CodeDecoder.formatsOf(new ArrayList<>()));
        setResult(RESULT_CANCELED);
        setContentView(layout());
    }

    private View layout() {
        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        preview = new TextureView(this);
        preview.setSurfaceTextureListener(new TextureView.SurfaceTextureListener() {
            @Override
            public void onSurfaceTextureAvailable(SurfaceTexture texture, int width, int height) {
                startCameraIfAllowed();
            }

            @Override
            public void onSurfaceTextureSizeChanged(SurfaceTexture texture, int width, int height) {
                fitPreview();
            }

            @Override
            public boolean onSurfaceTextureDestroyed(SurfaceTexture texture) {
                return true;
            }

            @Override
            public void onSurfaceTextureUpdated(SurfaceTexture texture) {
            }
        });
        root.addView(preview, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));
        root.addView(new FrameOverlay(this), new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        hint = new TextView(this);
        hint.setText(R.string.migo_scan_hint);
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        hint.setGravity(Gravity.CENTER);
        FrameLayout.LayoutParams hintAt = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.CENTER);
        int shorter = Math.min(getResources().getDisplayMetrics().widthPixels,
                getResources().getDisplayMetrics().heightPixels);
        hintAt.topMargin = Math.round(shorter * FRAME_FRACTION / 2f) + dp(24);
        root.addView(hint, hintAt);

        if (!cameraOnly) {
            Button album = new Button(this);
            album.setId(R.id.migo_scan_album);
            album.setText(R.string.migo_scan_album);
            album.setOnClickListener(view -> openAlbum());
            FrameLayout.LayoutParams albumAt = new FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT,
                    Gravity.BOTTOM | Gravity.CENTER_HORIZONTAL);
            albumAt.bottomMargin = dp(48);
            root.addView(album, albumAt);
        }
        return root;
    }

    private int dp(int value) {
        return Math.round(TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics()));
    }

    @Override
    protected void onResume() {
        super.onResume();
        decodeThread = new HandlerThread("migo-scan-decode");
        decodeThread.start();
        decodeHandler = new Handler(decodeThread.getLooper());
        decodeHandler.post(() -> decoder = new CodeDecoder(formats));
        Uri picture = pendingPicture;
        pendingPicture = null;
        if (picture != null) readPictureLater(picture);
        if (preview.isAvailable()) startCameraIfAllowed();
    }

    @Override
    protected void onPause() {
        stopCamera();
        if (decodeThread != null) {
            decodeThread.quitSafely();
            decodeThread = null;
            decodeHandler = null;
        }
        super.onPause();
    }

    // ==================== the camera ====================

    private void startCameraIfAllowed() {
        if (camera != null || cameraThread != null || delivered.get()) return;
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startCamera();
        } else if (!askedForCamera && declaresCamera()) {
            askedForCamera = true;
            requestPermissions(new String[] {Manifest.permission.CAMERA}, REQUEST_CAMERA);
        } else {
            cameraUnavailable();
        }
    }

    /** Whether the app declares the camera permission: without it the system never grants it. */
    private boolean declaresCamera() {
        try {
            PackageInfo info = getPackageManager().getPackageInfo(
                    getPackageName(), PackageManager.GET_PERMISSIONS);
            return info.requestedPermissions != null
                    && Arrays.asList(info.requestedPermissions).contains(Manifest.permission.CAMERA);
        } catch (PackageManager.NameNotFoundException impossible) {
            return false;
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        if (requestCode != REQUEST_CAMERA) return;
        // Granted, the camera starts as the screen resumes, its decode thread running again.
        if (results.length == 0 || results[0] != PackageManager.PERMISSION_GRANTED) {
            cameraUnavailable();
        } else if (decodeHandler != null && preview.isAvailable()) {
            startCameraIfAllowed();
        }
    }

    /** No camera: the album alone, or -- the camera being all content allowed -- the end. */
    private void cameraUnavailable() {
        if (cameraOnly) {
            finishWithError("system permission denied");
        } else {
            hint.setText(R.string.migo_scan_no_camera);
        }
    }

    private void startCamera() {
        CameraManager manager = (CameraManager) getSystemService(CAMERA_SERVICE);
        try {
            String id = backCamera(manager);
            if (id == null) {
                cameraUnavailable();
                return;
            }
            StreamConfigurationMap map = manager.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            imageSize = map == null ? null : imageSizeOf(map);
            if (imageSize == null) {
                cameraUnavailable();
                return;
            }
            cameraThread = new HandlerThread("migo-scan-camera");
            cameraThread.start();
            cameraHandler = new Handler(cameraThread.getLooper());
            reader = ImageReader.newInstance(
                    imageSize.getWidth(), imageSize.getHeight(), ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(this::onFrame, decodeHandler);
            fitPreview();
            manager.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice device) {
                    camera = device;
                    startPreview();
                }

                @Override
                public void onDisconnected(CameraDevice device) {
                    device.close();
                    if (camera == device) camera = null;
                }

                @Override
                public void onError(CameraDevice device, int error) {
                    Log.w(TAG, "camera error " + error);
                    device.close();
                    if (camera == device) camera = null;
                    runOnUiThread(ScanActivity.this::cameraUnavailable);
                }
            }, cameraHandler);
        } catch (CameraAccessException | SecurityException e) {
            Log.w(TAG, "camera unavailable", e);
            stopCamera();
            cameraUnavailable();
        }
    }

    private static String backCamera(CameraManager manager) throws CameraAccessException {
        String[] ids = manager.getCameraIdList();
        for (String id : ids) {
            Integer facing = manager.getCameraCharacteristics(id).get(CameraCharacteristics.LENS_FACING);
            if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) return id;
        }
        return ids.length > 0 ? ids[0] : null;
    }

    /** The largest image both the preview and the reader take, at most {@link #MAX_IMAGE_PIXELS}. */
    private static Size imageSizeOf(StreamConfigurationMap map) {
        Size[] yuv = map.getOutputSizes(ImageFormat.YUV_420_888);
        Size[] texture = map.getOutputSizes(SurfaceTexture.class);
        if (yuv == null || texture == null) return null;
        Size best = null;
        for (Size size : yuv) {
            long pixels = (long) size.getWidth() * size.getHeight();
            if (pixels > MAX_IMAGE_PIXELS || !Arrays.asList(texture).contains(size)) continue;
            if (best == null || pixels > (long) best.getWidth() * best.getHeight()) best = size;
        }
        return best;
    }

    private void startPreview() {
        CameraDevice device = camera;
        SurfaceTexture texture = preview.getSurfaceTexture();
        if (device == null || texture == null || reader == null) return;
        texture.setDefaultBufferSize(imageSize.getWidth(), imageSize.getHeight());
        Surface shown = new Surface(texture);
        Surface read = reader.getSurface();
        try {
            device.createCaptureSession(Arrays.asList(shown, read), new CameraCaptureSession.StateCallback() {
                @Override
                public void onConfigured(CameraCaptureSession configured) {
                    if (camera != device) {
                        configured.close();
                        return;
                    }
                    session = configured;
                    try {
                        CaptureRequest.Builder request = device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                        request.addTarget(shown);
                        request.addTarget(read);
                        request.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE);
                        configured.setRepeatingRequest(request.build(), null, cameraHandler);
                    } catch (CameraAccessException | IllegalStateException e) {
                        Log.w(TAG, "preview did not start", e);
                    }
                }

                @Override
                public void onConfigureFailed(CameraCaptureSession failed) {
                    Log.w(TAG, "preview could not be configured");
                    runOnUiThread(ScanActivity.this::cameraUnavailable);
                }
            }, cameraHandler);
        } catch (CameraAccessException | IllegalStateException e) {
            Log.w(TAG, "preview did not start", e);
            cameraUnavailable();
        }
    }

    private void stopCamera() {
        if (session != null) {
            session.close();
            session = null;
        }
        if (camera != null) {
            camera.close();
            camera = null;
        }
        if (reader != null) {
            reader.close();
            reader = null;
        }
        if (cameraThread != null) {
            cameraThread.quitSafely();
            cameraThread = null;
            cameraHandler = null;
        }
    }

    /**
     * The camera's image over the whole screen, upright and not stretched: the preview is
     * scaled to cover the view, as a camera app shows it, and turned for a sideways screen.
     */
    private void fitPreview() {
        Size size = imageSize;
        int viewWidth = preview.getWidth();
        int viewHeight = preview.getHeight();
        if (size == null || viewWidth == 0 || viewHeight == 0) return;
        int rotation = rotation();
        Matrix matrix = new Matrix();
        RectF view = new RectF(0, 0, viewWidth, viewHeight);
        float centerX = view.centerX();
        float centerY = view.centerY();
        if (rotation == Surface.ROTATION_90 || rotation == Surface.ROTATION_270) {
            RectF buffer = new RectF(0, 0, size.getHeight(), size.getWidth());
            buffer.offset(centerX - buffer.centerX(), centerY - buffer.centerY());
            matrix.setRectToRect(view, buffer, Matrix.ScaleToFit.FILL);
            float scale = Math.max((float) viewHeight / size.getHeight(), (float) viewWidth / size.getWidth());
            matrix.postScale(scale, scale, centerX, centerY);
            matrix.postRotate(90 * (rotation - 2), centerX, centerY);
        } else {
            // Upright already, the image's sides swapped by the sensor's quarter turn: stretched to
            // the view by default, so scaled back to its own shape and then over the view.
            float imageWidth = size.getHeight();
            float imageHeight = size.getWidth();
            float scale = Math.max(viewWidth / imageWidth, viewHeight / imageHeight);
            matrix.setScale(imageWidth * scale / viewWidth, imageHeight * scale / viewHeight, centerX, centerY);
            if (rotation == Surface.ROTATION_180) matrix.postRotate(180, centerX, centerY);
        }
        preview.setTransform(matrix);
    }

    @SuppressWarnings("deprecation")
    private int rotation() {
        android.view.Display display = android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.R
                ? getDisplay() : getWindowManager().getDefaultDisplay();
        return display == null ? Surface.ROTATION_0 : display.getRotation();
    }

    // ==================== reading ====================

    /**
     * The newest frame, on the decode thread: frames that came while the last was read are
     * dropped for it ({@code acquireLatestImage}).
     */
    private void onFrame(ImageReader from) {
        Image image = from.acquireLatestImage();
        if (image == null) return;
        try {
            if (delivered.get() || decoder == null) return;
            Image.Plane y = image.getPlanes()[0];
            int width = image.getWidth();
            int height = image.getHeight();
            int stride = y.getRowStride();
            ByteBuffer bytes = y.getBuffer();
            int length = Math.min(stride * (height - 1) + width, bytes.remaining());
            if (luminance.length < stride * height) luminance = new byte[stride * height];
            bytes.get(luminance, 0, length);
            CodeDecoder.Decoded decoded = decoder.decodeFrame(luminance, width, height, stride, FRAME_FRACTION);
            if (decoded != null) deliver(decoded);
        } finally {
            image.close();
        }
    }

    private void openAlbum() {
        Intent pick = new Intent(Intent.ACTION_GET_CONTENT);
        pick.setType("image/*");
        pick.addCategory(Intent.CATEGORY_OPENABLE);
        startActivityForResult(Intent.createChooser(pick, null), REQUEST_ALBUM);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQUEST_ALBUM || resultCode != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        // The picker paused this screen: its picture is read once the decode thread runs again.
        if (decodeHandler != null) {
            readPictureLater(data.getData());
        } else {
            pendingPicture = data.getData();
        }
    }

    /** Read the album's picture where a frame would be read: the decode thread. */
    private void readPictureLater(Uri picture) {
        decodeHandler.post(() -> {
            CodeDecoder.Decoded decoded = readPicture(picture);
            if (decoded != null) {
                deliver(decoded);
            } else {
                runOnUiThread(() -> hint.setText(R.string.migo_scan_no_code));
            }
        });
    }

    /** The code in the picture at {@code uri}, read at most {@link #MAX_PICTURE_SIDE} on a side. */
    private CodeDecoder.Decoded readPicture(Uri uri) {
        try {
            BitmapFactory.Options bounds = new BitmapFactory.Options();
            bounds.inJustDecodeBounds = true;
            try (InputStream in = getContentResolver().openInputStream(uri)) {
                BitmapFactory.decodeStream(in, null, bounds);
            }
            int sample = 1;
            while (Math.max(bounds.outWidth, bounds.outHeight) / sample > MAX_PICTURE_SIDE) sample *= 2;
            BitmapFactory.Options options = new BitmapFactory.Options();
            options.inSampleSize = sample;
            options.inPreferredConfig = Bitmap.Config.ARGB_8888;
            Bitmap bitmap;
            try (InputStream in = getContentResolver().openInputStream(uri)) {
                bitmap = BitmapFactory.decodeStream(in, null, options);
            }
            if (bitmap == null) return null;
            int width = bitmap.getWidth();
            int height = bitmap.getHeight();
            int[] pixels = new int[width * height];
            bitmap.getPixels(pixels, 0, width, 0, 0, width, height);
            bitmap.recycle();
            return new CodeDecoder(formats).decodePicture(pixels, width, height);
        } catch (Exception e) {
            Log.w(TAG, "the picture could not be read", e);
            return null;
        }
    }

    private void deliver(CodeDecoder.Decoded decoded) {
        if (!delivered.compareAndSet(false, true)) return;
        Intent result = new Intent()
                .putExtra(RESULT_TEXT, decoded.text)
                .putExtra(RESULT_SCAN_TYPE, decoded.scanType)
                .putExtra(RESULT_RAW_DATA, decoded.rawData);
        runOnUiThread(() -> {
            setResult(RESULT_OK, result);
            finish();
        });
    }

    private void finishWithError(String reason) {
        if (!delivered.compareAndSet(false, true)) return;
        setResult(RESULT_CANCELED, new Intent().putExtra(RESULT_ERROR, reason));
        finish();
    }

    /** The screen dimmed outside the square the camera's image is read in, and its corners marked. */
    private static final class FrameOverlay extends View {
        private final Paint dim = new Paint();
        private final Paint corner = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path outside = new Path();

        FrameOverlay(Activity activity) {
            super(activity);
            dim.setColor(0x99000000);
            corner.setColor(0xFF20C060);
            corner.setStyle(Paint.Style.STROKE);
            corner.setStrokeWidth(TypedValue.applyDimension(
                    TypedValue.COMPLEX_UNIT_DIP, 4, activity.getResources().getDisplayMetrics()));
        }

        @Override
        protected void onDraw(Canvas canvas) {
            float side = Math.min(getWidth(), getHeight()) * FRAME_FRACTION;
            float left = (getWidth() - side) / 2f;
            float top = (getHeight() - side) / 2f;
            outside.reset();
            outside.setFillType(Path.FillType.EVEN_ODD);
            outside.addRect(0, 0, getWidth(), getHeight(), Path.Direction.CW);
            outside.addRect(left, top, left + side, top + side, Path.Direction.CW);
            canvas.drawPath(outside, dim);
            float arm = side / 8f;
            float right = left + side;
            float bottom = top + side;
            canvas.drawLine(left, top, left + arm, top, corner);
            canvas.drawLine(left, top, left, top + arm, corner);
            canvas.drawLine(right, top, right - arm, top, corner);
            canvas.drawLine(right, top, right, top + arm, corner);
            canvas.drawLine(left, bottom, left + arm, bottom, corner);
            canvas.drawLine(left, bottom, left, bottom - arm, corner);
            canvas.drawLine(right, bottom, right - arm, bottom, corner);
            canvas.drawLine(right, bottom, right, bottom - arm, corner);
        }
    }
}
