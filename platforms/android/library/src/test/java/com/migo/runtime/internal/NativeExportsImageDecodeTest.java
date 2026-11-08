package com.migo.runtime.internal;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.ColorSpace;

import java.nio.Buffer;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;

import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.robolectric.RobolectricTestRunner;
import org.robolectric.annotation.Config;
import org.robolectric.annotation.Implementation;
import org.robolectric.annotation.Implements;
import org.robolectric.shadow.api.Shadow;

/** Exercises the production export with controlled native rows, not a device codec. */
@RunWith(RobolectricTestRunner.class)
@Config(sdk = 35, manifest = Config.NONE, shadows = {
        NativeExportsImageDecodeTest.BitmapRows.class,
        NativeExportsImageDecodeTest.Decoder.class
})
public final class NativeExportsImageDecodeTest {
    private static final byte[] ENCODED = {1};
    private static final int[] COLORS = {
        0x80112233, 0x00445566, 0x01A0B0C0,
        0xFF102030, 0x7FDDEEFF, 0x40204060
    };

    @Before public void resetDecoder() {
        Decoder.image = null;
        Decoder.width = 3;
        Decoder.height = 2;
        Decoder.decodes = 0;
    }

    private static BitmapRows image(int width, int height, int padding) {
        Bitmap bitmap = Shadow.newInstanceOf(Bitmap.class);
        BitmapRows rows = Shadow.extract(bitmap);
        rows.width = width;
        rows.height = height;
        rows.rowBytes = width * 4 + padding;
        rows.colors = new int[width * height];
        for (int i = 0; i < rows.colors.length; i++) rows.colors[i] = COLORS[i % COLORS.length];
        rows.raw = new byte[rows.rowBytes * height];
        Arrays.fill(rows.raw, (byte) 0x5A); // Native padding must never enter the result.
        ByteBuffer raw = ByteBuffer.wrap(rows.raw);
        for (int y = 0; y < height; y++) {
            raw.position(y * rows.rowBytes);
            for (int x = 0; x < width; x++) raw.putInt(Integer.rotateLeft(rows.colors[y * width + x], 8));
        }
        Decoder.image = bitmap;
        Decoder.width = width;
        Decoder.height = height;
        return rows;
    }

    private static byte[] expected(BitmapRows rows) {
        ByteBuffer packed = ByteBuffer.allocate(8 + rows.colors.length * 4);
        packed.order(ByteOrder.LITTLE_ENDIAN).putInt(rows.width).putInt(rows.height);
        packed.order(ByteOrder.BIG_ENDIAN);
        for (int color : rows.colors) packed.putInt(Integer.rotateLeft(color, 8));
        return packed.array();
    }

    @Test public void tightStraightSrgbExportsRgbaAfterHeader() {
        BitmapRows rows = image(3, 2, 0);
        assertArrayEquals(expected(rows), NativeExports.decodeImageRgba(ENCODED));
        assertEquals(0, rows.rawCopies); // Offset heap-buffer copies abort on API 26.
        assertEquals(2, rows.pixelReads);
        assertTrue(rows.recycled);
    }

    @Test public void paddedRowsExportOnlyLogicalRgba() {
        BitmapRows rows = image(3, 2, 4);
        byte[] packed = NativeExports.decodeImageRgba(ENCODED);
        assertArrayEquals(new byte[] {
            3, 0, 0, 0, 2, 0, 0, 0,
            17, 34, 51, (byte) 128, 68, 85, 102, 0, (byte) 160, (byte) 176, (byte) 192, 1,
            16, 32, 48, (byte) 255, (byte) 221, (byte) 238, (byte) 255, 127, 32, 64, 96, 64
        }, packed);
        assertEquals(0, rows.rawCopies);
        assertTrue(rows.recycled);
    }

    @Test public void widePaddedRowsUseBoundedScratch() {
        BitmapRows rows = image(2051, 2, 4);
        assertArrayEquals(expected(rows), NativeExports.decodeImageRgba(ENCODED));
        assertTrue(rows.largestRead <= 1024);
        assertEquals(6, rows.pixelReads);
        assertTrue(rows.recycled);
    }

    @Test public void premultipliedResultUsesStraightColorInts() {
        BitmapRows rows = image(3, 2, 0);
        rows.premultiplied = true;
        Arrays.fill(rows.raw, (byte) 0x5A);
        assertArrayEquals(expected(rows), NativeExports.decodeImageRgba(ENCODED));
        assertEquals(0, rows.rawCopies);
        assertTrue(rows.recycled);
    }

    @Test public void nonSrgbResultUsesSrgbColorInts() {
        BitmapRows rows = image(3, 2, 0);
        rows.colorSpace = ColorSpace.get(ColorSpace.Named.DISPLAY_P3);
        Arrays.fill(rows.raw, (byte) 0x5A);
        assertArrayEquals(expected(rows), NativeExports.decodeImageRgba(ENCODED));
        assertEquals(0, rows.rawCopies);
        assertTrue(rows.recycled);
    }

    @Test public void exportOutOfMemoryReturnsNullAndRecycles() {
        BitmapRows rows = image(3, 2, 4);
        rows.failWithOom = true;
        assertNull(NativeExports.decodeImageRgba(ENCODED));
        assertTrue(rows.recycled);
    }

    @Test public void unexpectedExportFailureStillRecycles() {
        BitmapRows rows = image(3, 2, 4);
        rows.failWithRuntime = true;
        assertThrows(IllegalStateException.class, () -> NativeExports.decodeImageRgba(ENCODED));
        assertTrue(rows.recycled);
    }

    @Test public void invalidActualDimensionsAreRejectedAndRecycled() {
        BitmapRows rows = image(0, 2, 0);
        Decoder.width = 3; // A decoder result may differ from the bounds probe.
        assertNull(NativeExports.decodeImageRgba(ENCODED));
        assertEquals(0, rows.rawCopies);
        assertTrue(rows.recycled);
    }

    @Test public void emptyAndOversizedInputsDoNotDecodePixels() {
        assertNull(NativeExports.decodeImageRgba(null));
        assertNull(NativeExports.decodeImageRgba(new byte[0]));
        Decoder.width = 8193;
        Decoder.height = 8193;
        assertNull(NativeExports.decodeImageRgba(ENCODED));
        assertEquals(0, Decoder.decodes);
    }

    @Implements(BitmapFactory.class)
    public static final class Decoder {
        static Bitmap image;
        static int width, height, decodes;

        @Implementation protected static Bitmap decodeByteArray(byte[] data, int offset, int length,
                BitmapFactory.Options options) {
            if (options.inJustDecodeBounds) {
                options.outWidth = width;
                options.outHeight = height;
                return null;
            }
            assertEquals(Bitmap.Config.ARGB_8888, options.inPreferredConfig);
            assertEquals(ColorSpace.get(ColorSpace.Named.SRGB), options.inPreferredColorSpace);
            assertFalse(options.inPremultiplied);
            decodes++;
            return image;
        }
    }

    /** Models raw storage separately from getPixels' straight sRGB color-int contract. */
    @Implements(Bitmap.class)
    public static final class BitmapRows {
        int width, height, rowBytes, rawCopies, pixelReads, largestRead;
        int[] colors;
        byte[] raw;
        boolean recycled, premultiplied, failWithOom, failWithRuntime;
        ColorSpace colorSpace = ColorSpace.get(ColorSpace.Named.SRGB);

        @Implementation protected int getWidth() { return width; }
        @Implementation protected int getHeight() { return height; }
        @Implementation protected int getRowBytes() { return rowBytes; }
        @Implementation protected Bitmap.Config getConfig() { return Bitmap.Config.ARGB_8888; }
        @Implementation protected boolean isPremultiplied() { return premultiplied; }
        @Implementation protected ColorSpace getColorSpace() { return colorSpace; }
        @Implementation protected boolean isRecycled() { return recycled; }
        @Implementation protected void recycle() { recycled = true; }

        @Implementation protected void copyPixelsToBuffer(Buffer destination) {
            assertFalse(recycled);
            rawCopies++;
            ((ByteBuffer) destination).put(raw);
        }

        @Implementation protected void getPixels(int[] output, int offset, int stride,
                int x, int y, int count, int rows) {
            assertFalse(recycled);
            if (failWithOom) throw new OutOfMemoryError("injected pixel export failure");
            if (failWithRuntime) throw new IllegalStateException("injected pixel export failure");
            assertEquals(1, rows);
            assertTrue(x >= 0 && x + count <= width && y >= 0 && y < height);
            pixelReads++;
            largestRead = Math.max(largestRead, count);
            System.arraycopy(colors, y * width + x, output, offset, count);
        }
    }
}
