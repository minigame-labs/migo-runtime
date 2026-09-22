#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AHB="$ROOT/engine/crates/shared/src/protocol/ahb.rs"
DECODER="$ROOT/engine/crates/graphics/src/image_decode_ahb.rs"
GRAPHICS_LIB="$ROOT/engine/crates/graphics/src/lib.rs"
JNI_ENV="$ROOT/engine/crates/platform/src/android/jni/env.rs"
PBO="$ROOT/engine/crates/graphics/src/canvas/manager/pbo_upload.rs"
GPU_CAPS="$ROOT/engine/crates/shared/src/device/gpu_caps.rs"
DEVICE_CAPS="$ROOT/engine/crates/graphics/src/device_caps.rs"
CANVAS_MANAGER="$ROOT/engine/crates/graphics/src/canvas/manager/mod.rs"
IMAGE_MANAGER="$ROOT/engine/crates/graphics/src/canvas/manager/image.rs"
TEXTURE_IMPORT="$ROOT/engine/crates/graphics/src/texture_import.rs"
IMAGE_OPS="$ROOT/engine/crates/io/src/image_ops.rs"
# The inline decode itself is a service both executions call; what stayed in
# the runtime is fetching an `http(s)` source under its network policy.
INLINE_DECODE="$ROOT/engine/crates/services/src/image/inline.rs"
INLINE_SRC="$ROOT/engine/crates/runtime-v8/src/rendering/image/inline_src.rs"
# Loading an image -- data URL, package path, cache -- is a service both
# executions call (`migo-services::image`); what stayed in the runtime is the
# op adapters and the policy-checked fetch of an `http(s)` source.
IMAGE_LOADER="$ROOT/engine/crates/services/src/image/loader.rs"

fail() {
  printf 'R7 contract failure: %s\n' "$*" >&2
  exit 1
}

grep -Fq 'pub mod image_decode_ahb;' "$GRAPHICS_LIB" || fail 'graphics decoder module is not exported'
grep -Fq 'graphics::image_decode_ahb::decode_image_to_ahb' "$JNI_ENV" || fail 'Android does not register the Rust/Skia AHB decoder'
grep -Fq 'register_platform_decoder' "$JNI_ENV" || fail 'RGBA fallback registration was removed'
if grep -Fq 'decode_image_ahb_jni(data)' "$JNI_ENV"; then
  fail 'Android still routes the preferred decoder through the disabled Java bridge'
fi

grep -Fq 'AhbDesc::rgba_sampled_cpu_decode' "$PBO" || fail 'legacy RGBA-to-AHB upload lacks CPU write allocation usage'
grep -Fq 'CPU_WRITE_RARELY' "$AHB" || fail 'one-shot CPU write usage is absent'
grep -Fq 'pub fn finish' "$AHB" || fail 'AHB lock has no explicit fallible finish'
grep -Fq 'AHardwareBuffer_unlock(self.ahb.inner.ptr, ptr::null_mut())' "$AHB" || fail 'Android unlock is not synchronous'
grep -Fq 'cpu_lock: parking_lot::Mutex<()>' "$AHB" || fail 'Android AHB clones do not serialize safe mutable CPU locks'
grep -Fq '_cpu_lock: parking_lot::MutexGuard' "$AHB" || fail 'Android AHB lock guard does not retain CPU-lock serialization'
grep -Fq 'pub unsafe fn from_raw_acquire' "$AHB" || fail 'borrowed raw AHB adoption lacks an unsafe ownership boundary'
grep -Fq 'pub unsafe fn from_raw_owned' "$AHB" || fail 'owned raw AHB adoption lacks an unsafe ownership boundary'

grep -Fq 'Data::new_bytes' "$DECODER" || fail 'Skia input is copied instead of borrowed synchronously'
grep -Fq 'ColorType::RGBA8888' "$DECODER" || fail 'decoder output is not explicit RGBA8888'
grep -Fq 'AlphaType::Unpremul' "$DECODER" || fail 'decoder output is not straight alpha'
grep -Fq 'EncodedOrigin::TopLeft' "$DECODER" || fail 'EXIF orientation fallback guard is absent'
grep -Fq 'get_pixels_with_options' "$DECODER" || fail 'Skia does not decode directly into the locked slice'
grep -Fq 'lock.finish()' "$DECODER" || fail 'decoder publishes before explicit unlock completion'

grep -Fq 'pub ahb: bool' "$GPU_CAPS" || fail 'GPU capability snapshot does not publish AHB import support'
grep -Fq 'pub fn set(&self, etc2: bool, astc: bool, ahb: bool)' "$GPU_CAPS" || fail 'GPU capabilities are not published atomically as one complete snapshot'
grep -Fq 'CompressedFormatSupport::detect(gl)' "$DEVICE_CAPS" || fail 'compressed-format detection still publishes an incomplete capability snapshot'
grep -Fq 'device_caps.ahb_available' "$CANVAS_MANAGER" || fail 'final AHB import availability is not available at capability publication'
grep -Fq 'gpu_caps.set(' "$CANVAS_MANAGER" || fail 'CanvasManager does not publish final GPU capabilities'
grep -Fq 'has_extension(&gl_extensions, "GL_OES_EGL_image")' "$DEVICE_CAPS" || fail 'AHB capability uses substring extension matching'
grep -Fq 'gpu_caps.disable_ahb()' "$IMAGE_MANAGER" || fail 'direct runtime import failure does not disable repeated AHB decode/readback attempts'
grep -Fq 'gpu_caps.snapshot().ahb' "$PBO" || fail 'legacy RGBA-to-AHB upload ignores the session circuit breaker'
grep -Fq 'gpu_caps.disable_ahb()' "$PBO" || fail 'legacy AHB failure does not trip the session circuit breaker'
grep -Fq 'gpu_caps: &shared::device::gpu_caps::GpuCaps' "$IMAGE_MANAGER" || fail 'ImageRegistry does not propagate the shared AHB circuit breaker to legacy uploads'
grep -Fq 'gpu_caps.ahb' "$IMAGE_OPS" || fail 'filesystem decode does not gate AHB on renderer import support'
grep -Fq 'run_image_job_with_live_caps' "$IMAGE_OPS" || fail 'queued filesystem decode snapshots AHB caps before its worker starts'
grep -Fq 'run_bounded_inline_image_job' "$IMAGE_OPS" || fail 'inline decode has no shared IO budget/semaphore scheduler path'
grep -Fq 'allow_ahb: bool' "$INLINE_DECODE" || fail 'inline decode does not require an explicit AHB capability decision'
grep -Fq 'decode_image_to_any(bytes, hint_mime, allow_ahb)' "$INLINE_DECODE" || fail 'inline decode bypasses the AHB capability gate'
grep -Fq 'probe_image_dimensions(bytes)' "$INLINE_DECODE" || fail 'untrusted inline images lack a pre-allocation dimension guard'
grep -Fq 'validate_data_url_cache_input(&src)' "$IMAGE_LOADER" || fail 'data URL cache keying happens before hostile metadata/payload size validation'
grep -Fq 'data:sha256:' "$IMAGE_LOADER" || fail 'multi-megabyte data URLs are retained verbatim as shared cache keys'
grep -Fq 'gpu_caps.snapshot().ahb' "$IMAGE_LOADER" || fail 'data/http image decode does not consume the final AHB capability snapshot'
if [[ "$(grep -Fc 'run_bounded_inline_image_job' "$IMAGE_LOADER")" -lt 2 ]]; then
  fail 'data/http image decode does not share the bounded image scheduler path'
fi
data_begin_line="$(awk '/async fn load_image_from_inline_bytes/ { in_fn=1 } in_fn && /c\.begin_load/ { print NR; exit }' "$IMAGE_LOADER")"
data_parse_line="$(awk '/async fn load_image_from_inline_bytes/ { in_fn=1 } in_fn && /parse_data_url/ { print NR; exit }' "$IMAGE_LOADER")"
if [[ -z "$data_begin_line" || -z "$data_parse_line" || "$data_begin_line" -ge "$data_parse_line" ]]; then
  fail 'data URL is parsed/decoded before shared-cache begin_load deduplication'
fi
grep -Fq 'drain_stale_gl_errors' "$TEXTURE_IMPORT" || fail 'AHB import attributes stale GL errors to the new texture'
drain_line="$(awk '/pub unsafe fn import_ahb_as_texture/ { in_fn=1 } in_fn && /drain_stale_gl_errors/ { print NR; exit }' "$TEXTURE_IMPORT")"
target_line="$(awk '/pub unsafe fn import_ahb_as_texture/ { in_fn=1 } in_fn && /image_target_texture/ { print NR; exit }' "$TEXTURE_IMPORT")"
if [[ -z "$drain_line" || -z "$target_line" || "$drain_line" -ge "$target_line" ]]; then
  fail 'stale GL errors are not drained before EGLImage target import'
fi
awk '
  /pub\(crate\) fn load_ahb_image/ { in_ahb = 1 }
  in_ahb && /pub\(crate\) fn load_compressed_image/ { in_ahb = 0 }
  in_ahb && /PIXEL_STORE/ { found = 1 }
  END { exit(found ? 0 : 1) }
' "$CANVAS_MANAGER" || fail 'AHB fallback does not invalidate pixel-store state changed by PBO upload'

for banned in AImageDecoder AHardwareBuffer_isSupported ASurfaceControl Bitmap getHardwareBuffer ImageDecoder; do
  if grep -Fq "$banned" "$DECODER"; then
    fail "decoder references banned or above-floor API: $banned"
  fi
done

printf 'R7 AHB image-decode static contract: PASS\n'
