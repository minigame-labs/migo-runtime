package com.migo.runtime.internal.platform;

import com.google.zxing.BarcodeFormat;
import com.google.zxing.BinaryBitmap;
import com.google.zxing.DecodeHintType;
import com.google.zxing.LuminanceSource;
import com.google.zxing.MultiFormatReader;
import com.google.zxing.NotFoundException;
import com.google.zxing.PlanarYUVLuminanceSource;
import com.google.zxing.RGBLuminanceSource;
import com.google.zxing.ReaderException;
import com.google.zxing.Result;
import com.google.zxing.ResultMetadataType;
import com.google.zxing.common.HybridBinarizer;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collection;
import java.util.EnumMap;
import java.util.EnumSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * scanCode's decoding, apart from the camera and the screen: the code in a camera frame's
 * luminance or in a picture's pixels, for the kinds of code content asked for.
 *
 * <p>A frame is decoded in the square the scanner shows its frame over, at the centre of the
 * camera's image. QR, Data Matrix and Aztec codes read in any orientation; a one-dimensional
 * barcode only across its bars, and the camera's image is the sensor's -- a quarter turn from
 * a phone held upright -- so a frame with no code across is read again turned. Not
 * thread-safe: one decoder per decoding thread.
 *
 * @hide
 */
public final class CodeDecoder {

    /** A decoded code, as scanCode reports it. */
    public static final class Decoded {
        /** The code's text. */
        public final String text;
        /** The code's kind as the reference names it: {@code "QR_CODE"}, {@code "EAN_13"}, ... */
        public final String scanType;
        /** The code's data: the bytes of its byte segments, or its text in UTF-8. */
        public final byte[] rawData;

        Decoded(String text, String scanType, byte[] rawData) {
            this.text = text;
            this.scanType = scanType;
            this.rawData = rawData;
        }
    }

    private static final EnumSet<BarcodeFormat> BAR_CODES = EnumSet.of(
            BarcodeFormat.UPC_A, BarcodeFormat.UPC_E, BarcodeFormat.EAN_8, BarcodeFormat.EAN_13,
            BarcodeFormat.CODE_39, BarcodeFormat.CODE_93, BarcodeFormat.CODE_128, BarcodeFormat.ITF,
            BarcodeFormat.CODABAR, BarcodeFormat.RSS_14, BarcodeFormat.RSS_EXPANDED);

    private final MultiFormatReader reader = new MultiFormatReader();
    private final boolean barCodes;
    /** The frame's square, and the same turned: kept between frames. */
    private byte[] square = new byte[0];
    private byte[] turned = new byte[0];

    /** A decoder for {@code formats}, which {@link #formatsOf} makes. */
    public CodeDecoder(Set<BarcodeFormat> formats) {
        Map<DecodeHintType, Object> hints = new EnumMap<>(DecodeHintType.class);
        hints.put(DecodeHintType.POSSIBLE_FORMATS, EnumSet.copyOf(formats));
        hints.put(DecodeHintType.TRY_HARDER, Boolean.TRUE);
        hints.put(DecodeHintType.CHARACTER_SET, "UTF-8");
        reader.setHints(hints);
        EnumSet<BarcodeFormat> bars = EnumSet.copyOf(formats);
        bars.retainAll(BAR_CODES);
        barCodes = !bars.isEmpty();
    }

    /**
     * The kinds of code scanCode's {@code scanType} names: {@code barCode}, {@code qrCode},
     * {@code datamatrix}, {@code pdf417}; none -- or none of these -- is the reference's
     * default, bar codes and QR codes.
     */
    public static Set<BarcodeFormat> formatsOf(Collection<String> scanType) {
        EnumSet<BarcodeFormat> formats = EnumSet.noneOf(BarcodeFormat.class);
        for (String type : scanType) {
            switch (type) {
                case "barCode":
                    formats.addAll(BAR_CODES);
                    break;
                case "qrCode":
                    formats.add(BarcodeFormat.QR_CODE);
                    formats.add(BarcodeFormat.AZTEC);
                    break;
                case "datamatrix":
                    formats.add(BarcodeFormat.DATA_MATRIX);
                    break;
                case "pdf417":
                    formats.add(BarcodeFormat.PDF_417);
                    break;
                default:
                    break;
            }
        }
        if (formats.isEmpty()) {
            formats.addAll(BAR_CODES);
            formats.add(BarcodeFormat.QR_CODE);
            formats.add(BarcodeFormat.AZTEC);
        }
        return formats;
    }

    /**
     * The code in the centre square of a frame's luminance -- {@code width} x {@code height}
     * samples, a row every {@code rowStride} bytes, as a camera's Y plane is -- whose side is
     * {@code fraction} of the frame's shorter side; {@code null} when there is none.
     */
    public Decoded decodeFrame(byte[] luminance, int width, int height, int rowStride, float fraction) {
        int side = Math.max(1, Math.round(Math.min(width, height) * fraction));
        int left = (width - side) / 2;
        int top = (height - side) / 2;
        int area = side * side;
        if (square.length != area) {
            square = new byte[area];
            turned = new byte[area];
        }
        for (int row = 0; row < side; row++) {
            System.arraycopy(luminance, (top + row) * rowStride + left, square, row * side, side);
        }
        Decoded decoded = decode(new PlanarYUVLuminanceSource(square, side, side, 0, 0, side, side, false));
        if (decoded != null || !barCodes) return decoded;
        // A bar code held across the phone runs along the sensor's columns: read it turned.
        for (int row = 0; row < side; row++) {
            for (int column = 0; column < side; column++) {
                turned[column * side + (side - 1 - row)] = square[row * side + column];
            }
        }
        return decode(new PlanarYUVLuminanceSource(turned, side, side, 0, 0, side, side, false));
    }

    /** The code in a picture's {@code width} x {@code height} ARGB pixels, or {@code null}. */
    public Decoded decodePicture(int[] argb, int width, int height) {
        return decode(new RGBLuminanceSource(width, height, argb));
    }

    private Decoded decode(LuminanceSource source) {
        try {
            Result result = reader.decodeWithState(new BinaryBitmap(new HybridBinarizer(source)));
            return new Decoded(result.getText(), result.getBarcodeFormat().name(), dataOf(result));
        } catch (NotFoundException notFound) {
            return null;
        } catch (ReaderException unreadable) {
            // Found and not readable -- a checksum or a format error: no code yet.
            return null;
        } finally {
            reader.reset();
        }
    }

    /** The bytes a code carries: its byte segments where it has them, else its text. */
    @SuppressWarnings("unchecked")
    private static byte[] dataOf(Result result) {
        Map<ResultMetadataType, Object> metadata = result.getResultMetadata();
        Object segments = metadata == null ? null : metadata.get(ResultMetadataType.BYTE_SEGMENTS);
        if (segments instanceof List) {
            ByteArrayOutputStream joined = new ByteArrayOutputStream();
            for (byte[] segment : (List<byte[]>) segments) {
                joined.write(segment, 0, segment.length);
            }
            if (joined.size() > 0) return joined.toByteArray();
        }
        return result.getText().getBytes(StandardCharsets.UTF_8);
    }

    /** {@code scanType} as content passed it: the names, in order, of a JSON array's strings. */
    static List<String> namesOf(org.json.JSONArray scanType) {
        List<String> names = new ArrayList<>();
        if (scanType == null) return names;
        for (int i = 0; i < scanType.length(); i++) {
            names.add(scanType.optString(i, ""));
        }
        return names;
    }
}
