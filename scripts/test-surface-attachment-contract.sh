#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES="$ROOT/engine/crates"

fail() {
    echo "SurfaceAttachment contract failed: $*" >&2
    exit 1
}

# A gate whose tool is missing must say so, not report the invariant it was
# unable to check.
#
# `require_multiline_regex` runs `rg` inside an `if !` condition, where a
# missing binary exits 127 and reads as "the pattern was not found" -- so on a
# runner without ripgrep this script announced that SurfaceDestroyed is not
# generation-tagged. It said that for weeks on CI. The invariant was fine; the
# gate was checking nothing and blaming the code, which is worse than not
# running at all, because it sends whoever reads it hunting for a bug that does
# not exist.
if ! command -v rg >/dev/null 2>&1; then
    echo "SurfaceAttachment contract could not run: ripgrep (rg) is not installed." >&2
    echo "This is a missing tool, NOT a contract violation. Install ripgrep and re-run." >&2
    exit 127
fi

require_literal() {
    local file="$1"
    local literal="$2"
    local reason="$3"
    if [[ ! -f "$file" ]] || ! grep -Fq "$literal" "$file"; then
        fail "$reason ($file)"
    fi
}

require_multiline_regex() {
    local file="$1"
    local pattern="$2"
    local reason="$3"
    if [[ ! -f "$file" ]] || ! rg -qU "$pattern" "$file"; then
        fail "$reason ($file)"
    fi
}

require_active_run() {
    local file="$1"
    local command="$2"
    if [[ ! -f "$ROOT/$file" ]] || ! awk -v command="$command" '
        {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            sub(/[[:space:]]+$/, "", line)
            sub(/^run:[[:space:]]*/, "", line)
            if (line == command) found = 1
        }
        END { exit !found }
    ' "$ROOT/$file"; then
        fail "missing active workflow command '$command' ($file)"
    fi
}

if rg -n \
    'surface_epoch|DESTROY_EPOCHS|bump_destroy_epoch|current_destroy_epoch' \
    "$CRATES"; then
    fail "legacy destroy-epoch wiring remains"
fi

if rg -n \
    'raw_window_handle|RawWindowHandle|AndroidNdkWindowHandle|onscreen_window_from_surface|last_window' \
    "$CRATES"; then
    fail "legacy raw-window or naked-window recovery boundary remains"
fi
# Choosing which EGL shared object to dlopen at runtime is a platform-provider
# concern: it belongs in that platform's presenter and must never leak into
# graphics/core/runtime-v8, which stay backend-agnostic. Presenters are listed
# one per platform on purpose — a new backend has to add itself here, and that
# review is the point of the gate. Build scripts are exempt because they
# configure link flags at compile time, not runtime implementation selection.
if rg -n 'libEGL\.so' "$CRATES" \
    --glob '!**/platform/src/android/presenter.rs' \
    --glob '!**/platform/src/linux/presenter.rs' \
    --glob '!**/platform/src/ohos/presenter.rs' \
    --glob '!**/build.rs'; then
    fail "EGL implementation selection escaped the platform providers"
fi

require_multiline_regex "$CRATES/shared/src/protocol/host_cmd.rs" \
    'SurfaceDestroyed[[:space:]]*\{[^}]*generation:[[:space:]]*SurfaceGeneration' \
    "Host SurfaceDestroyed command is not generation-tagged"
require_multiline_regex "$CRATES/shared/src/protocol/render_cmd.rs" \
    'SurfaceDestroyed[[:space:]]*\{[[:space:]]*generation:[[:space:]]*SurfaceGeneration' \
    "render SurfaceDestroyed command is not generation-tagged"
# The reverse of what this line used to require, and the reason is the whole
# point of the arrangement it now guards.
#
# `RecreateOnscreen` used to carry the `SurfaceLease`. A lease pins the host's
# native Surface and RELEASED is published by the last one going away, so a lease
# riding a queued command held the Surface hostage behind whatever the render
# thread was doing -- and before the first frame that is EGL initialization,
# measured at 33 ms on macOS and 5.7-41 s on the iOS simulator, where ANGLE
# compiles its Metal shaders cold. `migo_surface_begin_detach` could not complete
# for the whole of it, so a host could not shut down before its renderer came up.
# `RenderService` giving up on the reply after 500 ms did not help: the lease
# stayed queued regardless -- and that reply is itself gone now, for the reason
# the second check below records.
#
# THE DRIFT THIS EXISTS TO CATCH is the payload coming back. The Surface is now a
# level on `SurfaceControl`, which a retirement revokes, and this command is only
# the wake for it -- but re-adding a field is a two-line change that reads like an
# optimisation, and nothing else in the build reports that a detach has quietly
# become gated on the GPU again.
if rg -qU 'RecreateOnscreen[[:space:]]*\{[^}]*SurfaceLease' \
    "$CRATES/shared/src/protocol/render_cmd.rs"; then
    fail "onscreen recreation carries a SurfaceLease again; the Surface must travel \
as a revocable level on SurfaceControl ($CRATES/shared/src/protocol/render_cmd.rs)"
fi
# And nothing waits for the install. The recreate used to answer on a SyncResp that
# `RenderService` blocked the session thread on -- up to 500 ms per attempt, three
# attempts, on the thread that runs JavaScript in the embedded product. EGL is what
# makes that wait long, so a resize arriving mid-initialization stalled content for
# 1.5 s. Both outcomes already travelled on the must-deliver Host stream, so the wait
# bought nothing. Two halves, because either can come back on its own: a reply field
# on the command, or a receive in the service.
if rg -qU 'RecreateOnscreen[[:space:]]*\{[^}]*resp' \
    "$CRATES/shared/src/protocol/render_cmd.rs"; then
    fail "onscreen recreation carries a reply channel again; its outcome must travel \
on the must-deliver Host stream so no caller waits on the session thread \
($CRATES/shared/src/protocol/render_cmd.rs)"
fi
if rg -q 'recv_timeout' "$CRATES/core/src/services/render.rs"; then
    fail "a Surface install waits for an answer again; the session thread must not \
block on EGL ($CRATES/core/src/services/render.rs)"
fi
require_literal "$CRATES/shared/src/surface/control.rs" \
    "candidate: Mutex<Option<(SurfaceCandidateRevision, SurfaceLease)>>" \
    "SurfaceControl does not publish the Surface a render worker installs, paired with \
the publication a request can name"
require_literal "$CRATES/core/src/services/render.rs" \
    "self.surface_control.publish_candidate(lease);" \
    "a Surface update does not publish through the queue-independent control plane, or \
keeps a copy of the lease instead of handing it over"
require_literal "$CRATES/graphics/src/render_thread.rs" \
    "let claimed = surface_control.live_candidate();" \
    "render startup does not read the Surface level; a lease handed in at spawn is \
pinned across GPU initialization"
require_literal "$CRATES/graphics/src/render_thread.rs" \
    "let Some(lease) = surface_control.live_candidate_for(requested)" \
    "onscreen recreation reads the Surface level without saying which generation it \
was asked about; a request that outlived its candidate would then install the one \
that replaced it, under the old request's parameters, and on failure retire the \
generation the host had just attached"
# A *publication* and not a generation, which is a distinction this line learned the
# hard way. `attach_or_update` reuses the live generation, so a resize republishes one
# -- and two queued requests naming the same generation let the older one match the
# newer one's candidate: installed under stale presentation parameters, answered on a
# dead channel, and on failure retiring the generation the host was actively using.
require_multiline_regex "$CRATES/shared/src/protocol/render_cmd.rs" \
    'RecreateOnscreen[[:space:]]*\{[^}]*revision:[[:space:]]*SurfaceCandidateRevision' \
    "the onscreen recreate request must name the publication it is for"
if rg -qU 'RecreateOnscreen[[:space:]]*\{[^}]*generation:[[:space:]]*SurfaceGeneration' \
    "$CRATES/shared/src/protocol/render_cmd.rs"; then
    fail "onscreen recreation is identified by a generation again; one generation is \
published more than once, so it cannot tell two requests apart \
($CRATES/shared/src/protocol/render_cmd.rs)"
fi
# Two literals, because the gate moved behind SurfaceControl: the registry holds
# the control, and the control holds the gate. Pinning only the registry field
# would pass for a SurfaceControl that had quietly stopped owning a gate, which
# is the invariant actually worth protecting.
require_literal "$CRATES/core/src/runtime/registry.rs" \
    "surface_control: Arc<SurfaceControl>" \
    "Host registry is missing the queue-independent Surface control"
require_literal "$CRATES/shared/src/surface/control.rs" \
    "gate: Arc<SurfaceGenerationGate>" \
    "SurfaceControl no longer owns the queue-independent generation gate"
require_literal "$CRATES/core/src/runtime/registry.rs" \
    "surface_control.shutdown();" \
    "shutdown does not retire the current Surface before render join"
# ...and shutdown must still mean retirement, not just a flag. Without this the
# check above would pass for a shutdown() that stopped retiring.
require_multiline_regex "$CRATES/shared/src/surface/control.rs" \
    'pub fn shutdown\(&self\)[^{]*\{[[:space:]]*self\.shutting_down[^;]*;[[:space:]]*self\.retire_current_and_request\(\)' \
    "SurfaceControl::shutdown no longer retires the current Surface"
require_literal "$CRATES/core/src/services/render.rs" \
    "attachment: SurfaceAttachmentSlot" \
    "Host render service is missing its unique attachment slot"
require_literal "$CRATES/graphics/src/render_thread.rs" \
    "let mut render_binding = RenderSurfaceBinding::new();" \
    "render thread is missing its retained Surface binding"
require_literal "$CRATES/graphics/src/canvas/handler.rs" \
    "RecreateOnscreen must be preflighted by the render thread" \
    "CanvasHandler can bypass generation preflight"
require_literal "$CRATES/graphics/src/surface_binding.rs" \
    "pub(crate) fn clear_after_egl_teardown" \
    "native Surface resource release is not ordered after EGL teardown"
require_multiline_regex "$CRATES/graphics/src/render_thread.rs" \
    'binding[[:space:]]*\.[[:space:]]*preflight\(&lease\)' \
    "Surface generation is not rejected before presenter preparation"
require_literal "$CRATES/graphics/src/render_thread.rs" \
    "cm.is_surface_recovery_ready()" \
    "context recovery is not gated on a fully installed prepared target"
require_literal "$CRATES/graphics/src/render_thread.rs" \
    ".validate_prepared(prepared.as_ref())" \
    "prepared presenter backend is not revalidated inside installation"
require_literal "$CRATES/graphics/src/egl_platform.rs" \
    "pub struct PlatformIdentity" \
    "graphics platforms have no immutable native-domain identity"
require_literal "$CRATES/graphics/src/egl_platform.rs" \
    "provider_identity != factory_identity" \
    "graphics platform construction does not reject provider/factory identity mismatch"
identity_check_line="$(
    rg -n 'if let Err\(error\) = validate_platform_identity\(' \
        "$CRATES/capi/src/surface.rs" | cut -d: -f1
)"
context_state_line="$(
    rg -n 'if let Err\(error\) = validate_platform_context_state\(' \
        "$CRATES/capi/src/surface.rs" | cut -d: -f1
)"
context_build_line="$(
    rg -n 'build_target\(descriptor, existing_platform_context\.as_ref\(\)\)' \
        "$CRATES/capi/src/surface.rs" | cut -d: -f1
)"
first_lease_line="$(
    rg -n 'let lease = match lease_surface_tracked\(' \
        "$CRATES/capi/src/surface.rs" | cut -d: -f1
)"
if [[ -z "$identity_check_line" || -z "$first_lease_line" ]] \
    || (( identity_check_line >= first_lease_line )); then
    fail "C ABI reattachment identity is not rejected before Surface lease/enqueue"
fi
if [[ -z "$context_state_line" || -z "$context_build_line" || -z "$first_lease_line" ]] \
    || (( context_state_line >= context_build_line )) \
    || (( context_build_line >= first_lease_line )); then
    fail "C ABI platform context is not validated/reused before Surface lease/enqueue"
fi
require_literal "$CRATES/capi/src/lib.rs" \
    "platform_context: Option<platform::PlatformContext>" \
    "Session does not retain target-specific platform construction state"
require_literal "$CRATES/graphics/src/canvas/manager/mod.rs" \
    "installed_surface: Option<PreparedEglSurfaceRef>" \
    "CanvasManager does not retain the prepared presentation target"
require_literal "$CRATES/graphics/src/canvas/manager/mod.rs" \
    "drawing_buffer: Option<drawing_buffer::DrawingBuffer>" \
    "partial onscreen EGL ownership does not keep the preserved DrawingBuffer paired with its context"
require_literal "$CRATES/graphics/src/canvas/manager/mod.rs" \
    "self.preserved_drawing_buffer = pending.drawing_buffer.take()" \
    "partial onscreen cleanup does not restore the preserved context/DB pair"
require_literal "$CRATES/graphics/src/canvas/manager/types.rs" \
    "Window," \
    "window SurfaceKind still carries a platform-native integer"
require_literal "$CRATES/graphics/src/canvas/manager/egl_ops.rs" \
    "struct InitializedDisplayGuard" \
    "initialized EGL display has no early-return teardown guard"
require_literal "$CRATES/graphics/src/canvas/manager/egl_ops.rs" \
    "struct ContextCleanupGuard" \
    "partial pbuffer creation has no EGLContext teardown guard"
require_literal "$CRATES/graphics/src/canvas/manager/egl_ops.rs" \
    "pub(super) struct EglRuntime" \
    "CanvasManager construction has no owned EGL display fallback"
require_literal "$CRATES/graphics/src/upload_thread.rs" \
    "provider.backend_id() != expected_backend" \
    "upload EGL dispatch does not fail closed on provider identity mismatch"

ANDROID_PRESENTER="$CRATES/platform/src/android/presenter.rs"
require_literal "$ANDROID_PRESENTER" \
    "pub struct AndroidEglProvider" \
    "Android system-EGL provider is missing"
require_literal "$ANDROID_PRESENTER" \
    "pub struct AndroidEglSurfaceFactory" \
    "Android EGL surface factory is missing"
require_literal "$ANDROID_PRESENTER" \
    "pub struct AndroidPreparedSurface" \
    "Android prepared surface target is missing"
require_literal "$ANDROID_PRESENTER" \
    'const ANDROID_EGL_LIBRARY: &str = "libEGL.so";' \
    "Android provider must select the system EGL library explicitly"
require_literal "$ANDROID_PRESENTER" \
    "GraphicsPlatform::try_new" \
    "Android presenter bundle is not validated"
require_multiline_regex "$ANDROID_PRESENTER" \
    'as_any\(\)[[:space:]]*\.[[:space:]]*downcast_ref::<AndroidSurfaceWrapper>\(\)' \
    "Android presenter does not fail-closed downcast the platform Surface"
if [[ -f "$ANDROID_PRESENTER" ]] && rg -n 'ANativeWindow_(acquire|release)' "$ANDROID_PRESENTER"; then
    fail "prepared Android presenter target must remain non-owning ($ANDROID_PRESENTER)"
fi
require_literal "$CRATES/platform/src/android/jni/inbound.rs" \
    "android_graphics_platform()" \
    "Android bootstrap does not inject its matched graphics platform"

OHOS_PRESENTER="$CRATES/platform/src/ohos/presenter.rs"
require_literal "$OHOS_PRESENTER" \
    "EglConcurrency::SharedContexts" \
    "OpenHarmony system EGL does not declare its cross-thread context policy"
require_literal "$OHOS_PRESENTER" \
    "PlatformIdentity::new::<OhosProcessEglDomain>" \
    "OpenHarmony provider/factory have no stable process-EGL identity"
require_literal "$OHOS_PRESENTER" \
    "platform_identity_is_stable_for_ohos_process_egl" \
    "OpenHarmony process-EGL identity has no regression test"

OHOS_CAPI_PLATFORM="$CRATES/capi/src/platform/ohos.rs"
require_literal "$OHOS_CAPI_PLATFORM" \
    "pub(crate) enum PlatformContext" \
    "OpenHarmony C ABI does not retain its graphics platform context"
require_literal "$OHOS_CAPI_PLATFORM" \
    "Some(PlatformContext::Graphics(graphics_platform)) => graphics_platform.clone()" \
    "OpenHarmony C ABI rebuilds rather than reuses its graphics platform context"

# The ABI is no longer compile-only: desktop Linux and Android each ship a
# linkable runtime. What must hold now is that the macro answers per target
# rather than for "Linux" as a whole -- Android and OpenHarmony are Linux
# kernels too, so a classifier written on __linux__ alone would claim a runtime
# on OpenHarmony, where none is built.
require_literal "$ROOT/include/migo/types.h" \
    "#if defined(__ANDROID__)" \
    "C ABI runtime macro no longer distinguishes Android from desktop Linux"
require_literal "$ROOT/include/migo/types.h" \
    "MIGO_PLATFORM_IS_OPENHARMONY" \
    "C ABI runtime macro no longer distinguishes OpenHarmony from desktop Linux"
require_literal "$ROOT/include/migo/types.h" \
    "#define MIGO_C_ABI_CANDIDATE 1" \
    "C ABI must remain a candidate until the README blockers are closed"
require_literal "$CRATES/platform/src/android/jni/profile_contract.rs" \
    '("updateSurface", "(ILjava/lang/Object;IIF)V")' \
    "Android updateSurface JNI descriptor changed"
require_literal "$CRATES/platform/src/android/jni/profile_contract.rs" \
    '("onSurfaceDestroyed", "(I)V")' \
    "Android onSurfaceDestroyed JNI descriptor changed"

require_active_run .github/workflows/pr-ci.yml \
    'bash scripts/test-surface-attachment-contract.sh'
require_active_run .github/workflows/release.yml \
    'bash scripts/test-surface-attachment-contract.sh'

(
    cd "$ROOT/engine"
    cargo test -p migo-shared surface::attachment --lib --locked --offline
)

echo "SurfaceAttachment lifecycle contract: PASS"
