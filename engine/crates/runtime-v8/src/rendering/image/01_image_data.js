import { domException } from "ext:host_v8_base/06_dom_exception.js";


const MAX_IMAGE_DATA_PIXELS = 8192 * 8192;
const MAX_IMAGE_DATA_BYTES = 64 * 1024 * 1024;

function checkedDimensions(rawWidth, rawHeight) {
    let width = Math.trunc(Number(rawWidth));
    let height = Math.trunc(Number(rawHeight));
    if (!Number.isFinite(width) || !Number.isFinite(height)) {
        throw new TypeError("createImageData: width and height must be finite numbers");
    }
    if (width === 0 || height === 0) {
        throw domException(
            "createImageData: width and height must be non-zero",
            "IndexSizeError",
        );
    }
    width = Math.abs(width);
    height = Math.abs(height);
    const pixels = width * height;
    const bytes = pixels * 4;
    if (!Number.isSafeInteger(pixels) || pixels > MAX_IMAGE_DATA_PIXELS
            || !Number.isSafeInteger(bytes) || bytes > MAX_IMAGE_DATA_BYTES) {
        throw new RangeError("createImageData: dimensions exceed the implementation limit");
    }
    return { width, height, bytes };
}

function createImageData(a, b) {
    // two overloads:
    // createImageData(width, height)
    // createImageData(imageData)
    const isNumber = (v) => typeof v === "number" && Number.isFinite(v);
    if (isNumber(a)) {
        const dimensions = checkedDimensions(a, b);
        const { width, height } = dimensions;
        if (typeof ImageData !== "undefined") {
            return new ImageData(width, height);
        }
        const data = new Uint8ClampedArray(dimensions.bytes);
        return { data, width, height };
    }

    // assume image-like object (has width, height, data)
    if (a && typeof a === "object" && "width" in a && "height" in a && "data" in a) {
        const dimensions = checkedDimensions(a.width, a.height);
        const { width, height } = dimensions;
        const srcData = a.data;
        const sourceLength = srcData instanceof ArrayBuffer
            ? srcData.byteLength
            : srcData && typeof srcData.length === "number"
            ? Number(srcData.length)
            : -1;
        if (!Number.isSafeInteger(sourceLength) || sourceLength !== dimensions.bytes) {
            throw new TypeError("createImageData: source data length does not match dimensions");
        }
        // Only bounded array-like inputs are accepted. Avoid Array.from on an
        // arbitrary iterable: it can materialize unbounded content before a
        // dimension check has any chance to reject it.
        const cloned = new Uint8ClampedArray(srcData);
        if (typeof ImageData !== "undefined") {
            return new ImageData(cloned, width, height);
        }
        return { data: cloned, width, height };
    }

    throw new TypeError("createImageData: invalid arguments");
}

export { createImageData }
