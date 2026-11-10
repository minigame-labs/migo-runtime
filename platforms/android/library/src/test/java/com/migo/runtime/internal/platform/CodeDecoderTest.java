package com.migo.runtime.internal.platform;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import com.google.zxing.BarcodeFormat;
import com.google.zxing.EncodeHintType;
import com.google.zxing.common.BitMatrix;
import com.google.zxing.oned.Code128Writer;
import com.google.zxing.qrcode.QRCodeWriter;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Collections;
import java.util.EnumMap;
import java.util.Map;
import java.util.Set;

import org.json.JSONArray;
import org.junit.Test;

/** scanCode's decoding, over codes ZXing's own writers make. */
public final class CodeDecoderTest {
    private static final int FRAME_WIDTH = 640;
    private static final int FRAME_HEIGHT = 480;
    /** Bytes past the frame's width in each row, as a camera's Y plane may have. */
    private static final int ROW_PADDING = 64;

    private static BitMatrix qr(String text) throws Exception {
        Map<EncodeHintType, Object> hints = new EnumMap<>(EncodeHintType.class);
        hints.put(EncodeHintType.CHARACTER_SET, "UTF-8");
        hints.put(EncodeHintType.MARGIN, 2);
        return new QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 240, 240, hints);
    }

    /** A camera frame: white, with {@code code} at its centre, turned a quarter if asked. */
    private static byte[] frame(BitMatrix code, boolean turned) {
        int stride = FRAME_WIDTH + ROW_PADDING;
        byte[] y = new byte[stride * FRAME_HEIGHT];
        Arrays.fill(y, (byte) 0xF0);
        int width = turned ? code.getHeight() : code.getWidth();
        int height = turned ? code.getWidth() : code.getHeight();
        int left = (FRAME_WIDTH - width) / 2;
        int top = (FRAME_HEIGHT - height) / 2;
        for (int row = 0; row < height; row++) {
            for (int column = 0; column < width; column++) {
                boolean dark = turned ? code.get(row, column) : code.get(column, row);
                if (dark) y[(top + row) * stride + left + column] = 0x10;
            }
        }
        return y;
    }

    private static CodeDecoder decoder(String... scanType) {
        return new CodeDecoder(CodeDecoder.formatsOf(Arrays.asList(scanType)));
    }

    @Test
    public void aQrCodeAtTheCentreOfAFrameIsRead() throws Exception {
        CodeDecoder.Decoded decoded = decoder("qrCode").decodeFrame(
                frame(qr("migo-scan-spec"), false), FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f);
        assertNotNull(decoded);
        assertEquals("migo-scan-spec", decoded.text);
        assertEquals("QR_CODE", decoded.scanType);
        assertArrayEquals("migo-scan-spec".getBytes(StandardCharsets.UTF_8), decoded.rawData);
    }

    /** A bar code across the phone held upright runs along the sensor's columns. */
    @Test
    public void aBarCodeAlongTheSensorsColumnsIsReadTurned() throws Exception {
        BitMatrix bars = new Code128Writer().encode("MIGO-128", BarcodeFormat.CODE_128, 300, 80);
        CodeDecoder.Decoded decoded = decoder("barCode").decodeFrame(
                frame(bars, true), FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f);
        assertNotNull("read once turned", decoded);
        assertEquals("MIGO-128", decoded.text);
        assertEquals("CODE_128", decoded.scanType);
    }

    @Test
    public void aCodeOfAKindNotAskedForIsNotRead() throws Exception {
        assertNull(decoder("barCode").decodeFrame(
                frame(qr("migo"), false), FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f));
    }

    @Test
    public void aFrameWithNoCodeReadsNothingAndTheNextFrameIsReadAfterIt() throws Exception {
        CodeDecoder decoder = decoder();
        byte[] blank = new byte[(FRAME_WIDTH + ROW_PADDING) * FRAME_HEIGHT];
        Arrays.fill(blank, (byte) 0x80);
        assertNull(decoder.decodeFrame(blank, FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f));
        CodeDecoder.Decoded decoded = decoder.decodeFrame(
                frame(qr("after"), false), FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f);
        assertNotNull(decoded);
        assertEquals("after", decoded.text);
    }

    @Test
    public void aPicturesCodeIsRead() throws Exception {
        BitMatrix code = qr("from the album");
        int[] argb = new int[code.getWidth() * code.getHeight()];
        for (int row = 0; row < code.getHeight(); row++) {
            for (int column = 0; column < code.getWidth(); column++) {
                argb[row * code.getWidth() + column] = code.get(column, row) ? 0xFF000000 : 0xFFFFFFFF;
            }
        }
        CodeDecoder.Decoded decoded = decoder("qrCode").decodePicture(argb, code.getWidth(), code.getHeight());
        assertNotNull(decoded);
        assertEquals("from the album", decoded.text);
    }

    @Test
    public void aCodesBytesAreItsByteSegments() throws Exception {
        CodeDecoder.Decoded decoded = decoder("qrCode").decodeFrame(
                frame(qr("扫码 ok"), false), FRAME_WIDTH, FRAME_HEIGHT, FRAME_WIDTH + ROW_PADDING, 0.7f);
        assertNotNull(decoded);
        assertEquals("扫码 ok", decoded.text);
        assertArrayEquals("扫码 ok".getBytes(StandardCharsets.UTF_8), decoded.rawData);
    }

    @Test
    public void scanTypeNamesTheKindsAndNoneIsBarCodesAndQrCodes() throws Exception {
        Set<BarcodeFormat> defaults = CodeDecoder.formatsOf(Collections.<String>emptyList());
        assertTrue(defaults.contains(BarcodeFormat.QR_CODE));
        assertTrue(defaults.contains(BarcodeFormat.EAN_13));
        assertTrue(!defaults.contains(BarcodeFormat.DATA_MATRIX));
        assertEquals(defaults, CodeDecoder.formatsOf(Collections.singletonList("unknown")));
        assertEquals(
                java.util.EnumSet.of(BarcodeFormat.DATA_MATRIX, BarcodeFormat.PDF_417),
                CodeDecoder.formatsOf(Arrays.asList("datamatrix", "pdf417")));
        assertEquals(Arrays.asList("qrCode", "barCode"),
                CodeDecoder.namesOf(new JSONArray("[\"qrCode\",\"barCode\"]")));
        assertTrue(CodeDecoder.namesOf(null).isEmpty());
    }
}
