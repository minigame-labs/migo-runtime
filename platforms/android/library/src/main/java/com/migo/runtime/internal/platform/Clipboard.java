package com.migo.runtime.internal.platform;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.widget.Toast;

/**
 * Clipboard utilities.
 * <p>
 * Provides clipboard read/write operations compatible with Android API 26+.
 * setClipboardData shows a toast "内容已复制" for 1.5 seconds.
 *
 * @hide
 */
public final class Clipboard {

    private static final String CLIP_LABEL = "migo_clipboard";

    private Clipboard() {}

    /**
     * Set the system clipboard content.
     * <p>
     * Shows a toast "内容已复制" (Content copied) for ~1.5 seconds.
     *
     * @param context The context
     * @param data    The text data to copy to clipboard
     * @return 0 on success, -1 on failure
     */
    public static int setClipboardData(Context context, String data) {
        if (context == null) return -1;

        try {
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) return -1;

            ClipData clip = ClipData.newPlainText(CLIP_LABEL, data != null ? data : "");
            clipboard.setPrimaryClip(clip);

            // Show toast on UI thread
            // Android 13+ shows clipboard toast automatically
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
                new Handler(Looper.getMainLooper()).post(() -> {
                    Toast.makeText(context, "内容已复制", Toast.LENGTH_SHORT).show();
                });
            }

            return 0;
        } catch (Exception e) {
            return -1;
        }
    }

    /**
     * Get the system clipboard content.
     *
     * @param context The context
     * @return The clipboard text content, or empty string if unavailable
     */
    public static String getClipboardData(Context context) {
        if (context == null) return "";

        try {
            ClipboardManager clipboard = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) return "";

            if (!clipboard.hasPrimaryClip()) return "";

            ClipData clip = clipboard.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) return "";

            ClipData.Item item = clip.getItemAt(0);
            CharSequence text = item.getText();
            return text != null ? text.toString() : "";
        } catch (Exception e) {
            return "";
        }
    }
}
