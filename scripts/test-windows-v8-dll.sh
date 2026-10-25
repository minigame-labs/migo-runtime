#!/usr/bin/env bash
# ============================================================
# Prove the Windows rusty_v8 DLL is usable, not merely linkable.
# Location: scripts/test-windows-v8-dll.sh
#
# scripts/build-v8-windows.sh producing a .dll and an import library only shows
# the link succeeded. This links a plain C consumer against that import library
# and runs it, so a DLL that builds but cannot load -- a missing dependency, a
# botched export table, an entry point that faults -- fails here instead of much
# later inside a Rust test binary where the cause is far from the symptom.
#
# The probe is C on purpose: the export surface is C, and a C consumer proves it
# without dragging in Rust, cargo or any V8 header.
#
# Usage: bash scripts/test-windows-v8-dll.sh
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
ART_DIR="$PROJECT_ROOT/engine/third_party/rusty_v8/$TARGET"

WORK_WIN="${MIGO_WIN_DLL_PROBE_DIR:-C:\\migo-win-spike-tmp\\v8dllprobe}"
win_to_unix() { printf '/mnt/%s' "$(printf '%s' "$1" | sed 's|\\|/|g; s|^\(.\):|\L\1|')"; }
WORK_UNIX="$(win_to_unix "$WORK_WIN")"

info() { printf '\033[0;36m[v8-dll] %s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m[v8-dll] %s\033[0m\n' "$*"; }
err()  { printf '\033[0;31m[v8-dll] %s\033[0m\n' "$*" >&2; }

DLL="$ART_DIR/rusty_v8.dll"
IMPLIB="$ART_DIR/rusty_v8.dll.lib"
for f in "$DLL" "$IMPLIB"; do
    [[ -f "$f" ]] || { err "missing $f -- run scripts/build-v8-windows.sh first"; exit 1; }
done

VSWHERE="/mnt/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
[[ -f "$VSWHERE" ]] || { err "vswhere not found -- install VS Build Tools with the C++ workload"; exit 1; }
VS_ROOT="$("$VSWHERE" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
    -latest -property installationPath | tr -d '\r')"
[[ -n "$VS_ROOT" ]] || { err "no Visual Studio with the C++ workload"; exit 1; }

mkdir -p "$WORK_UNIX"
cp "$DLL" "$WORK_UNIX/rusty_v8.dll"
cp "$IMPLIB" "$WORK_UNIX/rusty_v8.dll.lib"
MSVC_WARNINGS_AS_ERRORS='/W''X'

# Declared locally rather than included: this is exactly what a third-party C
# consumer of the DLL has to be able to do.
cat > "$WORK_UNIX/probe.c" <<'PROBE'
#include <stdio.h>

extern const char* v8__V8__GetVersion(void);
extern int v8__register_host_callback(const char* name, void* fn);
extern int v8__host_callbacks_ready(void);

/* The callbacks the DLL expects the host to supply. Listed explicitly rather
   than derived from the DLL, because this is the contract: if upstream adds one
   and the host side is not updated, registering these leaves a slot empty and
   v8__host_callbacks_ready stays 0, which is exactly the failure this catches
   -- and it catches it here rather than at the first serialization. */
static const char* const kExpected[] = {
    "crdtp__FrontendChannel__BASE__sendProtocolResponse",
    "crdtp__FrontendChannel__BASE__sendProtocolNotification",
    "crdtp__FrontendChannel__BASE__fallThrough",
    "crdtp__FrontendChannel__BASE__flushProtocolNotifications",
    "crdtp__DomainDispatcher__BASE__Dispatch",
    "crdtp__DomainDispatcher__BASE__Drop",
    "v8__Platform__CustomPlatform__BASE__PostTask",
    "v8__Platform__CustomPlatform__BASE__PostNonNestableTask",
    "v8__Platform__CustomPlatform__BASE__PostDelayedTask",
    "v8__Platform__CustomPlatform__BASE__PostNonNestableDelayedTask",
    "v8__Platform__CustomPlatform__BASE__PostIdleTask",
    "v8__Platform__CustomPlatform__BASE__DROP",
    "v8__ValueSerializer__Delegate__ThrowDataCloneError",
    "v8__ValueSerializer__Delegate__HasCustomHostObject",
    "v8__ValueSerializer__Delegate__IsHostObject",
    "v8__ValueSerializer__Delegate__WriteHostObject",
    "v8__ValueSerializer__Delegate__GetSharedArrayBufferId",
    "v8__ValueSerializer__Delegate__GetWasmModuleTransferId",
    "v8__ValueSerializer__Delegate__ReallocateBufferMemory",
    "v8__ValueSerializer__Delegate__FreeBufferMemory",
    "v8__ValueDeserializer__Delegate__ReadHostObject",
    "v8__ValueDeserializer__Delegate__GetSharedArrayBufferFromId",
    "v8__ValueDeserializer__Delegate__GetWasmModuleFromId",
    "rusty_v8_RustObj_trace",
    "rusty_v8_RustObj_get_name",
    "rusty_v8_RustObj_drop",
    "v8_inspector__V8Inspector__Channel__BASE__sendResponse",
    "v8_inspector__V8Inspector__Channel__BASE__sendNotification",
    "v8_inspector__V8Inspector__Channel__BASE__flushProtocolNotifications",
    "v8_inspector__V8InspectorClient__BASE__generateUniqueId",
    "v8_inspector__V8InspectorClient__BASE__runMessageLoopOnPause",
    "v8_inspector__V8InspectorClient__BASE__quitMessageLoopOnPause",
    "v8_inspector__V8InspectorClient__BASE__runIfWaitingForDebugger",
    "v8_inspector__V8InspectorClient__BASE__consoleAPIMessage",
    "v8_inspector__V8InspectorClient__BASE__ensureDefaultContextInGroup",
    "v8_inspector__V8InspectorClient__BASE__resourceNameToUrl",
};
static const int kExpectedCount = (int)(sizeof kExpected / sizeof kExpected[0]);

/* Only ever used as a distinct non-null address; never called. */
static void placeholder(void) {}

int main(void) {
    const char* version = v8__V8__GetVersion();
    int i;

    if (version == NULL || version[0] == '\0') {
        printf("PROBE-FAIL empty version\n");
        return 1;
    }

    /* Nothing registered yet, so the DLL must not claim to be ready. A ready
       DLL at this point would mean the check is not looking at anything. */
    if (v8__host_callbacks_ready() != 0) {
        printf("PROBE-FAIL ready before anything was registered\n");
        return 1;
    }

    /* A name the DLL does not know must be refused, not silently accepted --
       otherwise a renamed callback would bind nowhere and fail much later. */
    if (v8__register_host_callback("v8__no_such_callback", (void*)placeholder) != 0) {
        printf("PROBE-FAIL unknown callback name was accepted\n");
        return 1;
    }

    for (i = 0; i < kExpectedCount; i++) {
        if (v8__register_host_callback(kExpected[i], (void*)placeholder) != 1) {
            printf("PROBE-FAIL DLL rejected known callback: %s\n", kExpected[i]);
            return 1;
        }
        /* Every registration before the last must leave the set incomplete;
           otherwise the DLL knows fewer callbacks than this list does. */
        if (i < kExpectedCount - 1 && v8__host_callbacks_ready() != 0) {
            printf("PROBE-FAIL ready after only %d of %d registrations\n",
                   i + 1, kExpectedCount);
            return 1;
        }
    }

    if (v8__host_callbacks_ready() != 1) {
        printf("PROBE-FAIL not ready after registering all %d\n", kExpectedCount);
        return 1;
    }

    printf("PROBE-OK %s callbacks=%d\n", version, kExpectedCount);
    return 0;
}
PROBE

cat > "$WORK_UNIX/probe.bat" <<BAT
@echo off
setlocal
set "ERRORLEVEL="
call "${VS_ROOT}\\VC\\Auxiliary\\Build\\vcvars64.bat" >nul
cd /d "${WORK_WIN}"
cl /nologo /W4 ${MSVC_WARNINGS_AS_ERRORS} probe.c rusty_v8.dll.lib /Fe:probe.exe >compile.log 2>&1
rem A bare cl failure must not be masked: capture its code before anything else
rem runs, because the next command replaces %errorlevel%.
set CL_EXIT=%errorlevel%
if not "%CL_EXIT%"=="0" (
  type compile.log
  exit /b %CL_EXIT%
)
probe.exe >run.log 2>&1
set RUN_EXIT=%errorlevel%
type run.log
exit /b %RUN_EXIT%
BAT

info "linking and running a C consumer against rusty_v8.dll"
if ! ( cd "$WORK_UNIX" && cmd.exe /c "probe.bat" ); then
    err "probe failed -- the DLL links but does not load or run"
    [[ -f "$WORK_UNIX/compile.log" ]] && sed -n '1,20p' "$WORK_UNIX/compile.log" >&2
    exit 1
fi

RESULT="$(tr -d '\r' < "$WORK_UNIX/run.log")"
case "$RESULT" in
    PROBE-OK\ *) ;;
    *) err "unexpected probe output: $RESULT"; exit 1 ;;
esac

VERSION="${RESULT#PROBE-OK }"
VERSION="${VERSION%% *}"
# Shape check only. Pinning the exact version here would make a routine V8 bump
# fail in a place that has nothing to say about it; an empty or garbage string
# is what this is guarding against.
if ! printf '%s' "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.'; then
    err "version does not look like a V8 version: '$VERSION'"
    exit 1
fi

COUNT="${RESULT##*callbacks=}"

ok "C consumer linked the import library, loaded rusty_v8.dll and called into V8"
ok "v8 version reported: $VERSION"
ok "host-callback contract holds: unknown name refused, all $COUNT known names bound, ready only once complete"
