package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.Intent;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

import com.google.zxing.BarcodeFormat;
import com.migo.runtime.internal.CallbackCorrelation;
import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ResultProxyActivity;

import org.json.JSONObject;

import java.lang.ref.WeakReference;
import java.util.Set;

/**
 * scanCode on Android: the SDK's own scanner ({@link ScanActivity}), which reads the code with
 * the back camera -- or from a picture in the album, unless content asked for the camera only
 * -- and is answered as the reference answers: the code's text, its kind, its character set
 * and its data in base64. One scan at a time, as the screen shows one scanner.
 *
 * @hide
 */
public final class ScanCodeManager {

    private static final String TAG = "ScanCodeManager";
    private static final int REQUEST_SCAN_CODE = 9001;

    private final int sessionId;

    /**
     * The id of the scan currently in flight, or {@link CallbackCorrelation#ABSENT}.
     *
     * <p>Held on the instance rather than passed down because the result comes
     * back through an Activity result, not through the call that started it.
     * One field is enough for the same reason the manager holds one pending
     * scan: the screen shows one scanner at a time, and a second request
     * replaces the first here exactly as it does there.
     */
    private int requestId = CallbackCorrelation.ABSENT;
    private final WeakReference<Activity> activityRef;
    private final Handler mainHandler;

    public ScanCodeManager(int sessionId, Activity activity) {
        this.sessionId = sessionId;
        this.activityRef = new WeakReference<>(activity);
        this.mainHandler = new Handler(Looper.getMainLooper());
    }

    private Activity getActivity() {
        return activityRef.get();
    }

    /**
     * Open the scanner.
     *
     * @param optionsJson {@code onlyFromCamera} (bool) and {@code scanType} (string array)
     */
    public void scanCode(String optionsJson) {
        mainHandler.post(() -> {
            try {
                JSONObject opts = new JSONObject(optionsJson);
                requestId = CallbackCorrelation.requestIdOf(opts);
                Activity activity = getActivity();
                if (activity == null) {
                    sendError("scanCode:fail activity is gone");
                    return;
                }
                Set<BarcodeFormat> formats = CodeDecoder.formatsOf(CodeDecoder.namesOf(opts.optJSONArray("scanType")));
                String[] names = new String[formats.size()];
                int at = 0;
                for (BarcodeFormat format : formats) names[at++] = format.name();
                Intent intent = new Intent(activity, ScanActivity.class)
                        .putExtra(ScanActivity.EXTRA_FORMATS, names)
                        .putExtra(ScanActivity.EXTRA_CAMERA_ONLY, opts.optBoolean("onlyFromCamera", false));
                ResultProxyActivity.launch(activity, intent, REQUEST_SCAN_CODE, this::onActivityResult);
            } catch (Exception e) {
                Log.e(TAG, "scanCode error", e);
                sendError("scanCode:fail " + e.getMessage());
            }
        });
    }

    private void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQUEST_SCAN_CODE) return;
        if (resultCode != Activity.RESULT_OK || data == null) {
            String error = data == null ? null : data.getStringExtra(ScanActivity.RESULT_ERROR);
            sendError("scanCode:fail " + (error != null ? error : "cancel"));
            return;
        }
        try {
            String result = data.getStringExtra(ScanActivity.RESULT_TEXT);
            byte[] rawData = data.getByteArrayExtra(ScanActivity.RESULT_RAW_DATA);
            JSONObject json = new JSONObject();
            json.put("errMsg", "scanCode:ok");
            json.put("result", result != null ? result : "");
            json.put("scanType", data.getStringExtra(ScanActivity.RESULT_SCAN_TYPE));
            json.put("charSet", "UTF-8");
            // The reference's path is the page a code of its own leads to; no code here is one.
            json.put("path", "");
            json.put("rawData", rawData == null ? ""
                    : android.util.Base64.encodeToString(rawData, android.util.Base64.NO_WRAP));
            CallbackCorrelation.stamp(json, requestId);
            NativeMethods.onScanCodeResult(sessionId, json.toString());
        } catch (Exception e) {
            Log.e(TAG, "scanCode result error", e);
            sendError("scanCode:fail " + e.getMessage());
        }
    }

    private void sendError(String errMsg) {
        try {
            JSONObject json = new JSONObject();
            json.put("error", errMsg);
            CallbackCorrelation.stamp(json, requestId);
            NativeMethods.onScanCodeResult(sessionId, json.toString());
        } catch (Exception e) {
            NativeMethods.onScanCodeResult(sessionId,
                    CallbackCorrelation.failure(requestId, "scanCode", "internal error"));
        }
    }

    public void destroy() {
        // No resources to clean up
    }
}
