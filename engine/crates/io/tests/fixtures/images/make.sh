#!/usr/bin/env bash
# ffmpeg's own test pattern at 32x16, in the formats the image crate decodes for Migo: lossy
# WebP (VP8, what mini-game packages ship), lossless WebP (VP8L) and GIF. Generated, so they
# carry nobody's artwork; committed, so the tests need no ffmpeg.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
pattern=(-loglevel error -y -f lavfi -i "testsrc=size=32x16:rate=1" -frames:v 1)
ffmpeg "${pattern[@]}" testsrc-lossy.webp
ffmpeg "${pattern[@]}" -lossless 1 testsrc-lossless.webp
ffmpeg "${pattern[@]}" testsrc.gif

# Independent EXIF/rotation references. Pillow is needed only to regenerate
# this small JSON fixture; cargo tests use the committed bytes and pixels.
python3 - <<'PY_EXIF'
import base64
import io
import json
from pathlib import Path

from PIL import Image, ImageOps, __version__

image = Image.new("RGBA", (3, 2))
image.putdata([(240, 10, 20, 255), (30, 220, 40, 128), (50, 60, 210, 64),
               (0, 0, 0, 0), (160, 170, 180, 200), (190, 200, 210, 1)])
cases = []
for format in ("JPEG", "PNG", "WEBP"):
    for orientation in range(1, 9):
        exif = Image.Exif()
        exif[274] = orientation
        output = io.BytesIO()
        source = image.convert("RGB") if format == "JPEG" else image
        options = {"quality": 100, "subsampling": 0} if format == "JPEG" else {"lossless": True, "exact": True} if format == "WEBP" else {}
        source.save(output, format=format, exif=exif, **options)
        encoded = output.getvalue()
        expected = ImageOps.exif_transpose(Image.open(io.BytesIO(encoded))).convert("RGBA")
        cases.append({"format": format, "orientation": orientation,
                      "encoded": base64.b64encode(encoded).decode("ascii"),
                      "width": expected.width, "height": expected.height,
                      "rgba": list(expected.tobytes())})

# A broken unrelated EXIF field must not hide the valid primary orientation.
partial = Image.Exif()
partial[274] = 6
partial[306] = "2026:11:07 12:00:00"
output = io.BytesIO()
image.convert("RGB").save(output, format="JPEG", exif=partial, quality=100, subsampling=0)
encoded = bytearray(output.getvalue())
entry = encoded.index(bytes.fromhex("0132000200000014"))
encoded[entry + 8:entry + 12] = b"\xff" * 4
cases.append({**cases[5], "kind": "broken_unrelated_field",
              "encoded": base64.b64encode(encoded).decode("ascii")})

# Malformed JPEG metadata must not invent a rotation from arbitrary field
# bytes. Corrupt only the orientation field, leaving the image data intact.
jpeg = base64.b64decode(cases[5]["encoded"])
entry = jpeg.index(bytes.fromhex("011200030000000100060000"))
invalid = []
for name, offset, value in [("wrong_type", entry + 3, 7),
                            ("wrong_count", entry + 7, 0),
                            ("out_of_range", entry + 9, 9)]:
    data = bytearray(jpeg)
    data[offset] = value
    invalid.append({"name": name, "encoded": base64.b64encode(data).decode("ascii")})
fixture = {"generator": "Pillow " + __version__ + " ImageOps.exif_transpose",
           "cases": cases, "invalid": invalid}
Path("orientation-cases.json").write_text(json.dumps(fixture, indent=2) + "\n")
PY_EXIF

# RGB ICC references use LittleCMS independently of the engine's moxcms/Skia.
python3 - <<'PY_ICC'
import base64
import ctypes as C
import ctypes.util
import io
import json
import struct
from pathlib import Path
from PIL import Image, ImageCms, ImageOps, __version__

class xyY(C.Structure):
    _fields_ = [(name, C.c_double) for name in ("x", "y", "Y")]
class Primaries(C.Structure):
    _fields_ = [(name, xyY) for name in ("red", "green", "blue")]
cms = C.CDLL(ctypes.util.find_library("lcms2"))
cms.cmsBuildGamma.argtypes = [C.c_void_p, C.c_double]
cms.cmsBuildGamma.restype = C.c_void_p
cms.cmsCreateRGBProfile.argtypes = [C.POINTER(xyY), C.POINTER(Primaries), C.POINTER(C.c_void_p)]
cms.cmsCreateRGBProfile.restype = C.c_void_p
cms.cmsSaveProfileToMem.argtypes = [C.c_void_p, C.c_void_p, C.POINTER(C.c_uint32)]
cms.cmsSaveProfileToMem.restype = C.c_int
cms.cmsCloseProfile.argtypes = [C.c_void_p]
cms.cmsFreeToneCurve.argtypes = [C.c_void_p]

def canonical(data):
    data = bytearray(data)
    data[24:36] = struct.pack(">6H", 2026, 11, 7, 0, 0, 0)
    data[84:100] = bytes(16)  # Optional profile ID; avoid timestamp-dependent bytes.
    return bytes(data)

def profile(gamma, coordinates):
    white = xyY(0.3127, 0.3290, 1.0)
    primaries = Primaries(*(xyY(x, y, 1.0) for x, y in coordinates))
    curve = cms.cmsBuildGamma(None, gamma)
    handle = cms.cmsCreateRGBProfile(C.byref(white), C.byref(primaries), (C.c_void_p * 3)(curve, curve, curve))
    assert handle
    size = C.c_uint32()
    assert cms.cmsSaveProfileToMem(handle, None, C.byref(size))
    buffer = C.create_string_buffer(size.value)
    assert cms.cmsSaveProfileToMem(handle, buffer, C.byref(size))
    cms.cmsCloseProfile(handle)
    cms.cmsFreeToneCurve(curve)
    return canonical(buffer.raw[:size.value])

srgb = ImageCms.createProfile("sRGB")
profiles = [("linear-srgb", profile(1.0, [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)])),
            ("wide-gamut-g22", profile(2.2, [(0.68, 0.32), (0.265, 0.69), (0.15, 0.06)])),
            ("srgb", canonical(ImageCms.ImageCmsProfile(srgb).tobytes())),
            ("untagged", None), ("invalid", b"not an ICC profile")]
image = Image.new("RGBA", (3, 2))
image.putdata([(64, 100, 160, 255), (150, 60, 90, 128), (20, 170, 100, 64),
               (0, 0, 0, 0), (110, 130, 180, 200), (190, 100, 50, 1)])
cases = []
def encode_case(image, format, name, icc, orientation=1):
    output = io.BytesIO()
    source = image.convert("RGB") if format == "JPEG" else image
    exif = Image.Exif()
    exif[274] = orientation
    options = {"quality": 100, "subsampling": 0} if format == "JPEG" else {"lossless": True, "exact": True} if format == "WEBP" else {}
    source.save(output, format=format, icc_profile=icc, exif=exif, **options)
    encoded = output.getvalue()
    decoded = Image.open(io.BytesIO(encoded)).convert("RGBA")
    stored = decoded
    if icc and name != "invalid":
        stored = ImageCms.profileToProfile(decoded, ImageCms.ImageCmsProfile(io.BytesIO(icc)), srgb,
                                          renderingIntent=ImageCms.Intent.RELATIVE_COLORIMETRIC, outputMode="RGBA")
    oriented = stored.copy()
    oriented.getexif()[274] = orientation
    oriented = ImageOps.exif_transpose(oriented)
    cases.append({"format": format, "profile": name, "orientation": orientation,
                  "encoded": base64.b64encode(encoded).decode("ascii"),
                  "stored_width": decoded.width, "stored_height": decoded.height,
                  "width": oriented.width, "height": oriented.height,
                  "stored_rgba": list(stored.tobytes()), "rgba": list(oriented.tobytes())})

for name, icc in profiles:
    for format in ("JPEG", "PNG", "WEBP"):
        encode_case(image, format, name, icc)
encode_case(image, "PNG", profiles[0][0], profiles[0][1], orientation=6)
wide = Image.new("RGBA", (1025, 1))
wide.putdata([((i * 7) % 256, (i * 11) % 256, (i * 13) % 256, i % 256) for i in range(1025)])
encode_case(wide, "PNG", profiles[0][0], profiles[0][1])
Path("icc-cases.json").write_text(json.dumps({"generator": "Pillow " + __version__ + " / LittleCMS RGB to sRGB relative colorimetric", "cases": cases}, indent=2) + "\n")
PY_ICC
