package com.migo.runtime.internal.platform;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.view.PointerIcon;

/**
 * {@code setCursor}'s CSS cursors as Android pointer icons.
 *
 * @hide
 */
public final class PointerIcons {

    private PointerIcons() {}

    /** The Android icon type of a CSS cursor keyword, or -1 for one Android has none of. */
    public static int typeOf(String keyword) {
        switch (keyword) {
            case "default": case "auto": return PointerIcon.TYPE_DEFAULT;
            case "none": return PointerIcon.TYPE_NULL;
            case "pointer": return PointerIcon.TYPE_HAND;
            case "text": return PointerIcon.TYPE_TEXT;
            case "vertical-text": return PointerIcon.TYPE_VERTICAL_TEXT;
            case "crosshair": return PointerIcon.TYPE_CROSSHAIR;
            case "help": return PointerIcon.TYPE_HELP;
            case "wait": case "progress": return PointerIcon.TYPE_WAIT;
            case "context-menu": return PointerIcon.TYPE_CONTEXT_MENU;
            case "cell": return PointerIcon.TYPE_CELL;
            case "alias": return PointerIcon.TYPE_ALIAS;
            case "copy": return PointerIcon.TYPE_COPY;
            case "no-drop": case "not-allowed": return PointerIcon.TYPE_NO_DROP;
            case "grab": return PointerIcon.TYPE_GRAB;
            case "grabbing": return PointerIcon.TYPE_GRABBING;
            case "move": case "all-scroll": return PointerIcon.TYPE_ALL_SCROLL;
            case "zoom-in": return PointerIcon.TYPE_ZOOM_IN;
            case "zoom-out": return PointerIcon.TYPE_ZOOM_OUT;
            case "n-resize": case "s-resize": case "ns-resize": case "row-resize":
                return PointerIcon.TYPE_VERTICAL_DOUBLE_ARROW;
            case "e-resize": case "w-resize": case "ew-resize": case "col-resize":
                return PointerIcon.TYPE_HORIZONTAL_DOUBLE_ARROW;
            case "ne-resize": case "sw-resize": case "nesw-resize":
                return PointerIcon.TYPE_TOP_RIGHT_DIAGONAL_DOUBLE_ARROW;
            case "nw-resize": case "se-resize": case "nwse-resize":
                return PointerIcon.TYPE_TOP_LEFT_DIAGONAL_DOUBLE_ARROW;
            default: return -1;
        }
    }

    /** A system icon for {@code keyword}, or null for a keyword Android has none of. */
    public static PointerIcon system(Context context, String keyword) {
        int type = typeOf(keyword);
        return type < 0 ? null : PointerIcon.getSystemIcon(context, type);
    }

    /**
     * An icon from the image at {@code path} with its hotspot at (x, y), clamped
     * into the image; null when the file is no image Android decodes.
     */
    public static PointerIcon image(String path, float x, float y) {
        Bitmap bitmap = BitmapFactory.decodeFile(path);
        if (bitmap == null) return null;
        float hotX = Math.max(0f, Math.min(x, bitmap.getWidth() - 1));
        float hotY = Math.max(0f, Math.min(y, bitmap.getHeight() - 1));
        return PointerIcon.create(bitmap, hotX, hotY);
    }
}
