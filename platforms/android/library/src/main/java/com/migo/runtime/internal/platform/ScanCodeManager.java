package com.migo.runtime.internal.platform;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

import com.migo.runtime.internal.NativeMethods;
import com.migo.runtime.internal.ResultProxyActivity;

import org.json.JSONArray;
import com.migo.runtime.internal.CallbackCorrelation;

import org.json.JSONObject;

import java.lang.ref.WeakReference;
import java.util.List;

/**
 * Barcode/QR code scanning manager.
 * <p>
 * Uses ZXing-compatible Intent ({@code com.google.zxing.client.android.SCAN})
 * to delegate scanning to an installed barcode scanner app.
 * Compatible with Android API 26+.
 *
 * @hide
 */
public final class ScanCodeManager {

    private static final String TAG = "ScanCodeManager";
    private static final String ZXING_SCAN_ACTION = "com.google.zxing.client.android.SCAN";
    private static final int REQUEST_SCAN_CODE = 9001;

    private final int sessionId;

    /**
     * The id of the scan currently in flight, or {@link CallbackCorrelation#ABSENT}.
     *
     * <p>Held on the instance rather than passed down because the result comes
     * back through an Activity result, not through the call that started it.
     * One field is enough for the same reason the manager holds one pending
     * scan: the platform surfaces one scanner at a time, and a second request
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
     * Launch barcode scanner.
     *
     * @param optionsJson JSON with: onlyFromCamera (bool), scanType (string array)
     */
    public void scanCode(String optionsJson) {
        mainHandler.post(() -> {
            try {
                JSONObject opts = new JSONObject(optionsJson);
                requestId = CallbackCorrelation.requestIdOf(opts);
                JSONArray scanType = opts.optJSONArray("scanType");

                Intent intent = new Intent(ZXING_SCAN_ACTION);
                intent.addCategory(Intent.CATEGORY_DEFAULT);

                // Map scanType to ZXing SCAN_FORMATS
                String formats = buildScanFormats(scanType);
                if (formats != null && !formats.isEmpty()) {
                    intent.putExtra("SCAN_FORMATS", formats);
                }

                // Check if any app can handle the scan intent
                Activity activity = getActivity();
                if (activity == null) {
                    sendError("scanCode:fail activity is gone");
                    return;
                }
                PackageManager pm = activity.getPackageManager();
                List<ResolveInfo> resolveInfos = pm.queryIntentActivities(intent, 0);
                if (resolveInfos == null || resolveInfos.isEmpty()) {
                    sendError("scanCode:fail no scanner app available");
                    return;
                }

                ResultProxyActivity.launch(activity, intent,
                        REQUEST_SCAN_CODE, this::onActivityResult);
            } catch (Exception e) {
                Log.e(TAG, "scanCode error", e);
                sendError("scanCode:fail " + e.getMessage());
            }
        });
    }

    private void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode != REQUEST_SCAN_CODE) return;

        if (resultCode != Activity.RESULT_OK || data == null) {
            sendError("scanCode:fail cancel");
            return;
        }

        try {
            String result = data.getStringExtra("SCAN_RESULT");
            String format = data.getStringExtra("SCAN_RESULT_FORMAT");
            byte[] rawBytes = data.getByteArrayExtra("SCAN_RESULT_BYTES");

            JSONObject json = new JSONObject();
            json.put("errMsg", "scanCode:ok");
            json.put("result", result != null ? result : "");
            json.put("scanType", mapFormatToScanType(format));
            json.put("charSet", "UTF-8");
            json.put("path", "");

            if (rawBytes != null) {
                json.put("rawData", android.util.Base64.encodeToString(
                        rawBytes, android.util.Base64.NO_WRAP));
            } else {
                json.put("rawData", "");
            }

            CallbackCorrelation.stamp(json, requestId);
            NativeMethods.onScanCodeResult(sessionId, json.toString());
        } catch (Exception e) {
            Log.e(TAG, "scanCode result error", e);
            sendError("scanCode:fail " + e.getMessage());
        }
    }

    /**
     * Map JS scanType array to ZXing SCAN_FORMATS comma-separated string.
     */
    private static String buildScanFormats(JSONArray scanType) {
        if (scanType == null || scanType.length() == 0) return null;

        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < scanType.length(); i++) {
            String type = scanType.optString(i, "");
            String formats = mapScanTypeToFormats(type);
            if (formats != null) {
                if (sb.length() > 0) sb.append(',');
                sb.append(formats);
            }
        }
        return sb.toString();
    }

    /**
     * Map a single JS scanType value to ZXing format names.
     */
    private static String mapScanTypeToFormats(String type) {
        switch (type) {
            case "barCode":
                return "UPC_A,UPC_E,EAN_8,EAN_13,CODE_39,CODE_93,CODE_128,ITF," +
                       "CODABAR,RSS_14,RSS_EXPANDED";
            case "qrCode":
                return "QR_CODE,AZTEC";
            case "datamatrix":
                return "DATA_MATRIX";
            case "pdf417":
                return "PDF_417";
            default:
                return null;
        }
    }

    /**
     * Map ZXing format name to the API's scanType response value.
     */
    private static String mapFormatToScanType(String format) {
        if (format == null) return "QR_CODE";
        switch (format) {
            case "QR_CODE":     return "QR_CODE";
            case "AZTEC":       return "AZTEC";
            case "CODABAR":     return "CODABAR";
            case "CODE_39":     return "CODE_39";
            case "CODE_93":     return "CODE_93";
            case "CODE_128":    return "CODE_128";
            case "DATA_MATRIX": return "DATA_MATRIX";
            case "EAN_8":       return "EAN_8";
            case "EAN_13":      return "EAN_13";
            case "ITF":         return "ITF";
            case "MAXICODE":    return "MAXICODE";
            case "PDF_417":     return "PDF_417";
            case "RSS_14":      return "RSS_14";
            case "RSS_EXPANDED":return "RSS_EXPANDED";
            case "UPC_A":       return "UPC_A";
            case "UPC_E":       return "UPC_E";
            case "UPC_EAN_EXTENSION": return "UPC_EAN_EXTENSION";
            default:            return format;
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
