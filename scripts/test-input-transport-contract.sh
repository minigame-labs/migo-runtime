#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES="$ROOT/engine/crates"

fail() {
    echo "Input transport contract failed: $*" >&2
    exit 1
}

if ! command -v rg >/dev/null 2>&1; then
    echo "Input transport contract could not run: ripgrep (rg) is not installed." >&2
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

CHANNEL="$CRATES/shared/src/host_channel.rs"
REGISTRY="$CRATES/core/src/runtime/registry.rs"
HOST="$CRATES/core/src/runtime/host.rs"
CAPI="$CRATES/capi/src"
ANDROID_JNI="$CRATES/platform/src/android/jni"
ANDROID_JAVA="$ROOT/platforms/android/library/src/main/java/com/migo/runtime"
STATS="$CRATES/shared/src/stats.rs"

require_literal "$CHANNEL" "pub fn try_send_coalescible(" \
    "host channel has no explicit coalescible input operation"
require_literal "$CHANNEL" "pub fn try_send_reliable(" \
    "host channel has no reliable-transition operation"
require_literal "$CHANNEL" "pub fn try_send_terminal(" \
    "host channel has no terminal supersession operation"
require_literal "$CHANNEL" "VecDeque::with_capacity(" \
    "ordered host queue is not preallocated"
if rg -n 'unbounded_channel|Semaphore' "$CHANNEL"; then
    fail "legacy unbounded/semaphore host transport remains"
fi

# Everything above reads the source for properties the code was *written* to have,
# which cannot fail when an allocation appears. These two require the tests that
# can: `cargo test -p migo-shared --lib` counts real allocations across a burst on
# both halves of the transport and fails on a non-zero count. Deleting a gate is
# the one failure mode those tests cannot report about themselves, so it is checked
# here.
require_literal "$CHANNEL" "assert_no_steady_state_allocation(" \
    "the ordered host queue has no allocation-count gate, only structural checks"
require_literal "$CRATES/shared/src/payload_pool.rs" "assert_no_steady_state_allocation(" \
    "the input payload pool has no allocation-count gate, only structural checks"

require_literal "$REGISTRY" "pub(crate) const HOST_RELIABLE_INPUT_RESERVE: usize = 64;" \
    "reliable input reserve is missing or no longer non-zero"
require_multiline_regex "$REGISTRY" \
    'HOST_PAYLOAD_POOL_CAPACITY:[^=]*=[[:space:]]*HOST_NORMAL_COMMAND_CAPACITY[[:space:]]*\+[[:space:]]*HOST_RELIABLE_INPUT_RESERVE[[:space:]]*\+[[:space:]]*2' \
    "fixed payload pools do not cover both input lanes, the receiver, and a candidate"
require_literal "$REGISTRY" "claim_input_saturation_notification" \
    "Android adapters have no shared once-per-episode notification gate"
require_literal "$REGISTRY" "pub fn send_reliable_command_to_host(" \
    "host registry has no reliable lane for trusted asynchronous results"
require_multiline_regex "$ANDROID_JNI/inbound.rs" \
    'fn forward_json_result_to_js\([\s\S]*?send_reliable_command_to_host\(host_id,[[:space:]]*cmd\)' \
    "Android asynchronous JSON results still use the saturating normal lane"

require_literal "$CAPI/input.rs" "ingress.try_send_touch(touch_data)" \
    "C touch input bypasses semantic ingress"
require_literal "$CAPI/input.rs" "ingress.try_send_pointer(command)" \
    "C pointer input bypasses semantic ingress"
require_literal "$CAPI/keyboard.rs" "ingress.try_send_composition(command)" \
    "C composition input bypasses semantic ingress"
require_literal "$CAPI/keyboard.rs" "ingress.try_send_key(command)" \
    "C physical-key input bypasses semantic ingress"
require_literal "$CAPI/keyboard.rs" "ingress.try_send_keyboard(command)" \
    "C soft-keyboard input bypasses semantic ingress"
require_literal "$CAPI/gamepad.rs" "ingress.try_send_gamepad_connection(command)" \
    "C gamepad topology bypasses semantic ingress"
require_literal "$CAPI/gamepad.rs" "ingress.try_send_gamepad_state(state)" \
    "C gamepad samples bypass semantic ingress"
require_literal "$CAPI/lib.rs" "input_saturation_reported" \
    "C sessions do not rate-limit saturation callbacks"

# Asserted in the routing, not in the embedded host: both executions deliver
# input through `input_route::route`, so the retraction lives there and an
# external session gets it too. Held to the ORDER, which is the property --
# every release reaches content before `focusChanged(false)` does, or a game
# hears the loss with a finger still down.
require_multiline_regex "$CRATES/core/src/runtime/input_route.rs" \
    'HostCommand::OnFocusChanged[[:space:]]*\{[[:space:]]*focused[[:space:]]*\}[[:space:]]*=>[[:space:]]*\{[[:space:]]*if[[:space:]]*!focused[[:space:]]*\{[[:space:]]*state\.retract_for_focus_loss\([\s\S]*?\}[[:space:]]*sink\.focus_changed\(focused\);' \
    "focus loss no longer retracts accepted input before the content callback"

require_literal "$ANDROID_JNI/profile_contract.rs" \
    '("onTouchEvent", "(IIJILjava/nio/ByteBuffer;)Z")' \
    "Rust JNI profile does not declare boolean touch acceptance"
require_multiline_regex "$ANDROID_JNI/inbound.rs" \
    'fn onTouch\([^)]*buffer:[[:space:]]*JObject,[[:space:]]*\)[[:space:]]*->[[:space:]]*jboolean' \
    "Rust JNI touch entry does not return jboolean"
require_literal "$ANDROID_JAVA/internal/NativeBridge.java" \
    "static native boolean onTouchEvent" \
    "Java native bridge does not return touch acceptance"
require_literal "$ANDROID_JAVA/GameSession.java" \
    "return touchHandler.dispatch(sessionId, event);" \
    "GameSession still reports touch handled unconditionally"
require_literal "$ANDROID_JAVA/GameSession.java" \
    "notifyError(errorCode, fullMessage, ErrorCode.isRecoverable(errorCode));" \
    "GameSession does not preserve native recoverability classification"

require_literal "$STATS" "pub const VERSION: u16 = 6;" \
    "debug stats version is not v6"
require_literal "$STATS" "pub input_coalesced: u32," \
    "v6 metrics omit input coalescing"
require_literal "$STATS" "pub input_reliable_reserve_uses: u32," \
    "v6 metrics omit reliable-reserve use"
require_literal "$STATS" "pub input_saturation_events: u32," \
    "v6 metrics omit actual saturation"

require_literal "$ROOT/include/migo/input.h" \
    "MIGO_OK means the event" \
    "public C input acceptance semantics are undocumented"
require_literal "$ROOT/include/migo/session.h" \
    "Migo retracts every previously accepted active touch" \
    "public focus-loss convergence contract is undocumented"

require_active_run ".github/workflows/pr-ci.yml" \
    "bash scripts/test-input-transport-contract.sh"
require_active_run ".github/workflows/release.yml" \
    "bash scripts/test-input-transport-contract.sh"

echo "Input transport contract passed."
