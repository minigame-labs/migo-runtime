# ============================================================
# Android dynamic library build script
# Location: scripts/build-android-so.ps1
#
# Usage:
#   ./build-android-so.ps1
#   ./build-android-so.ps1 arm64-v8a
#   ./build-android-so.ps1 arm64-v8a x86_64 release
#   ./build-android-so.ps1 all release
#   ./build-android-so.ps1 arm64-v8a release --codegen-profile=2
# ============================================================

param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Args
)

# ------------------------------------------------------------
# Constants
# ------------------------------------------------------------
# Android minimum supported API level.
#
# Raised from 21 to 26 in lock-step with build-android-so.sh because
# skia-bindings 0.93 hard-codes API 26 in
# `build_support/platform/android.rs` (the first Oreo API; needed for
# full Vulkan and a number of modern NDK headers Skia depends on).
# Linking Skia against an older runtime would be ABI-unsafe, so we
# promote minSdk for the whole engine.
$ANDROID_API = 26

# Kept in step with the shell script, which is the lane that is actually
# exercised; this one had drifted two changes behind (the cdylib moved out of
# `platform` into its own crate, and every package later gained a `migo-`
# prefix), so it pointed at a crate and an output file that no longer exist.
# Package name and directory name are separate variables on purpose: packages
# carry the prefix, directories do not.
$CRATE_NAME = "migo-android-jni"
$CRATE_DIR  = "crates/android-jni"

# Not `lib$CRATE_NAME.so`: the crate sets `[lib] name = "migo"`, so cargo emits
# the shipping file name directly.
$CRATE_SO_NAME = "libmigo.so"

$OUTPUT_SO_NAME = "libmigo.so"

$PLATFORM_MAP = @{
    "arm64-v8a" = "aarch64-linux-android"
    "x86_64"    = "x86_64-linux-android"
    "all"       = "all"
}

# ------------------------------------------------------------
# Logging helpers
# ------------------------------------------------------------
function Print-Info    { param($m) Write-Host "[INFO] $m"    -ForegroundColor Cyan }
function Print-Success { param($m) Write-Host "[SUCCESS] $m" -ForegroundColor Green }
function Print-Warning { param($m) Write-Host "[WARNING] $m" -ForegroundColor Yellow }
function Print-Error   { param($m) Write-Host "[ERROR] $m"   -ForegroundColor Red }

function Resolve-Paths {
    $projectRoot = Split-Path $PSScriptRoot -Parent

    $engineRoot = Join-Path $projectRoot "engine"

    if (-not (Test-Path $engineRoot)) {
        Print-Error "engine directory not found at $engineRoot"
        exit 1
    }

    return @{
        Project   = $projectRoot
        Engine    = $engineRoot
        Crate     = Join-Path $engineRoot $CRATE_DIR
        Target    = Join-Path $engineRoot "target"
        JniLibs   = Join-Path $engineRoot "jniLibs"
        v8Libs    = Join-Path $engineRoot "third_party/rusty_v8"
        Platforms = Join-Path $engineRoot "platforms"
    }
}

# ------------------------------------------------------------
# Dependency check
# ------------------------------------------------------------
function Check-Dependencies {
    Print-Info "Checking dependencies..."

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Print-Error "cargo not found"
        exit 1
    }

    if (-not (Get-Command cargo-ndk -ErrorAction SilentlyContinue)) {
        Print-Error "cargo-ndk not found (install with: cargo install cargo-ndk)"
        exit 1
    }

    # Which NDK this is has to be asserted, not assumed: the NDK supplies the
    # compiler, sysroot and linker that the component manifest records, so it is
    # part of the artifact's identity. Testing only that ANDROID_NDK_HOME was
    # non-empty let a Windows build link the pinned V8 archive with any NDK at all,
    # which is the defect the shell path closed in task 1.1a and this path never
    # did. The variable is no longer required either, for the same reason
    # build-android-so.sh does not require it: the NDK is found in the standard SDK
    # layouts, and an override is checked like any other candidate.
    Import-Module (Join-Path $PSScriptRoot "lib/AndroidNdk.psm1") -Force
    $ndkLock = Join-Path (Split-Path $PSScriptRoot -Parent) "contracts/artifact-manifest/android-v8.lock.json"
    try {
        $ndkHome = Resolve-MigoPinnedNdk -Lock $ndkLock
    }
    catch {
        Print-Error $_.Exception.Message
        exit 1
    }
    Print-Success "Pinned Android NDK: $ndkHome"

    Print-Success "All dependencies are ready"
}

# ------------------------------------------------------------
# ABI mapping
# ------------------------------------------------------------
function Get-AbiName($platform) {
    switch ($platform) {
        "arm64-v8a" { "arm64-v8a" }
        "x86_64"    { "x86_64" }
        default     { "unknown" }
    }
}

# ------------------------------------------------------------
# Locate arm64 clang builtins
# ------------------------------------------------------------
function Find-Arm64Builtins {
    $prebuilt = Join-Path $env:ANDROID_NDK_HOME "toolchains\llvm\prebuilt"
    if (-not (Test-Path $prebuilt)) {
        return $null
    }

    Get-ChildItem $prebuilt -Directory | ForEach-Object {
        $clangRoot = Join-Path $_.FullName "lib64\clang"
        if (Test-Path $clangRoot) {
            Get-ChildItem $clangRoot -Directory | ForEach-Object {
                Get-ChildItem $_.FullName -Recurse `
                    -Filter "libclang_rt.builtins-aarch64-android.a" `
                    -ErrorAction SilentlyContinue |
                    Select-Object -First 1
            }
        }
    } | Select-Object -First 1
}

# ------------------------------------------------------------
# Build one platform
# ------------------------------------------------------------
function Build-Platform {
    param(
        [string]$platform,
        [string]$buildType,
        [string]$productProfile,
        [string]$codegenProfile,
        [bool]$workerSnapshot,
        [hashtable]$paths
    )

    if ($platform -in @("armv7", "x86")) {
        Print-Error "$platform is not supported"
        return $false
    }

    $targetTriple = $PLATFORM_MAP[$platform]
    $profileArgs = @()
    $outDir = "debug"
    $destinationSuffix = ""
    if ($buildType -eq "release") {
        switch ($codegenProfile) {
            "z" {
                $profileArgs = @("--release")
                $outDir = "release"
            }
            "2" {
                $profileArgs = @("--profile", "release-hot2")
                $outDir = "release-hot2"
                $destinationSuffix = "-opt2"
            }
            "3" {
                $profileArgs = @("--profile", "release-hot3")
                $outDir = "release-hot3"
                $destinationSuffix = "-opt3"
            }
        }
    }
    $cargoFeatures = "profile-$productProfile"
    if ($workerSnapshot) {
        $destinationSuffix += "-worker-snapshot"
        $cargoFeatures += ",worker-snapshot"
    }

    # --------------------------------------------------------
    # Rusty V8 config
    # --------------------------------------------------------
    # Verified against its component manifest and used from a path that is its own hash.
    # This was `Test-Path` -- existence, not identity -- so the AAR this script's caller
    # produces was linked against whatever bytes sat there, exactly as the shell path was
    # before it grew the same check.
    Import-Module (Join-Path $PSScriptRoot "lib/V8Materialise.psm1") -Force
    $v8 = Resolve-MigoMaterialisedV8 `
        -V8Dir (Join-Path $paths.v8Libs $targetTriple) `
        -Root (Join-Path (Split-Path $PSScriptRoot -Parent) "engine/target/v8-materialised")
    $env:RUSTY_V8_ARCHIVE = $v8.Archive
    Print-Info "RUSTY_V8_ARCHIVE = $($v8.Archive)"
    $env:RUSTY_V8_SRC_BINDING_PATH = $v8.Binding
    Print-Info "RUSTY_V8_SRC_BINDING_PATH = $($v8.Binding)"

    # --------------------------------------------------------
    # RUSTFLAGS (arm64 builtins)
    # --------------------------------------------------------
    $origRUSTFLAGS = $env:RUSTFLAGS

    # `-Wl,--allow-multiple-definition` is needed, and the reason this comment used to
    # give was wrong twice over: it is not that skia-bindings redefines symbols the NDK's
    # *shared* libc++ provides. Measured on the shell path by removing the flag: exactly six
    # symbols, the ones libc++ explicitly instantiates in stdexcept.cpp, defined by two
    # different static libc++ implementations -- Chromium's inside V8's rlib and the NDK
    # sysroot's libc++_static.a. Safe only because no std exception object crosses the
    # V8/Skia boundary. See ledger item 1.4 and the shell twin's comment.
    # Without this the final link fails with
    # "multiple definition of std::…" on some symbols.
    $commonRustflags = "-Clink-arg=-Wl,--allow-multiple-definition"

    if ($platform -eq "arm64-v8a") {
        $builtins = Find-Arm64Builtins
        if (-not $builtins) {
            Print-Error "libclang_rt.builtins-aarch64-android.a not found"
            return $false
        }

        $builtinsDir = Split-Path $builtins -Parent
        # --exclude-libs,ALL prevents re-exporting symbols from static
        # libs (V8, ring, Skia), shaving ~430 KB off .dynsym/.rela.dyn.
        # Not usable on x86_64 due to relocation model differences.
        $env:RUSTFLAGS = "$origRUSTFLAGS $commonRustflags -L $builtinsDir -l static=clang_rt.builtins-aarch64-android -Clink-arg=-Wl,--exclude-libs,ALL"

        Print-Info "Using arm64 clang builtins + --exclude-libs,ALL + --allow-multiple-definition"
    } else {
        $env:RUSTFLAGS = "$origRUSTFLAGS $commonRustflags"
    }

    # --------------------------------------------------------
    # SQLite bundled compile flags — trim features we never use.
    # libsqlite3-sys' build.rs reads $env:LIBSQLITE3_FLAGS and passes
    # them verbatim to the amalgamation compile. Flags here are a
    # pure *subtraction* on top of the default ENABLE_* matrix; any
    # attempt to OMIT a feature that sqlite3-sys defaults to
    # ENABLE_ would fail at cc_compile.  Keep this list in lock-step
    # with build-android-so.sh to avoid target-specific binary drift.
    $env:LIBSQLITE3_FLAGS = (@(
        "-DSQLITE_OMIT_LOAD_EXTENSION",
        "-DSQLITE_OMIT_DEPRECATED",
        "-DSQLITE_OMIT_AUTHORIZATION",
        "-DSQLITE_OMIT_SHARED_CACHE",
        "-DSQLITE_DQS=0",
        "-DSQLITE_DEFAULT_MEMSTATUS=0",
        "-DSQLITE_LIKE_DOESNT_MATCH_BLOBS",
        "-DSQLITE_MAX_EXPR_DEPTH=0"
    ) -join " ")

    # --------------------------------------------------------
    # Build
    # --------------------------------------------------------
    Print-Info "Building $platform ($targetTriple) [$buildType, codegen=$codegenProfile, worker-snapshot=$workerSnapshot]"

    $cargoArgs = @(
        "ndk",
        "--target", $targetTriple,
        "--platform", $ANDROID_API,
        "--",
        "build",
        "--target-dir", $paths.Target
    )
    $cargoArgs += $profileArgs
    $cargoArgs += @(
        "--no-default-features",
        "--features", $cargoFeatures
    )

    $locationPushed = $false
    try {
        Push-Location $paths.Crate
        $locationPushed = $true
        $proc = Start-Process `
            -FilePath "cargo" `
            -ArgumentList $cargoArgs `
            -Wait `
            -NoNewWindow `
            -PassThru
    }
    catch {
        Print-Error "Unable to start cargo build for $platform`: $_"
        $env:RUSTFLAGS = $origRUSTFLAGS
        return $false
    }
    finally {
        if ($locationPushed) {
            Pop-Location
        }
    }

    if ($proc.ExitCode -ne 0) {
        Print-Error "Build failed for $platform"
        $env:RUSTFLAGS = $origRUSTFLAGS
        return $false
    }

    # --------------------------------------------------------
    # Copy output .so
    # --------------------------------------------------------
    $abi     = Get-AbiName $platform
    $dstDir = Join-Path $paths.JniLibs "$productProfile$destinationSuffix\$abi"

    if (-not (Test-Path $dstDir)) {
        New-Item -ItemType Directory -Path $dstDir | Out-Null
    }

    $srcSo = Join-Path $paths.Target "$targetTriple\$outDir\$CRATE_SO_NAME"
    $dstSo = Join-Path $dstDir $OUTPUT_SO_NAME

    if (-not (Test-Path $srcSo)) {
        Print-Error "Output .so not found: $srcSo"
        $env:RUSTFLAGS = $origRUSTFLAGS
        return $false
    }
    try {
        Copy-Item $srcSo $dstSo -Force -ErrorAction Stop
    }
    catch {
        Print-Error "Unable to copy $srcSo to $dstSo`: $_"
        $env:RUSTFLAGS = $origRUSTFLAGS
        return $false
    }
    Print-Success "Copied -> $dstSo"

    # --------------------------------------------------------
    # Copy libc++_shared.so (required by cpal/oboe)
    # --------------------------------------------------------
    $libcppSrc = Join-Path $env:ANDROID_NDK_HOME "toolchains\llvm\prebuilt\windows-x86_64\sysroot\usr\lib\$targetTriple\libc++_shared.so"
    $libcppDst = Join-Path $dstDir "libc++_shared.so"

    if (Test-Path $libcppSrc) {
        try {
            Copy-Item $libcppSrc $libcppDst -Force -ErrorAction Stop
        }
        catch {
            Print-Error "Unable to copy $libcppSrc to $libcppDst`: $_"
            $env:RUSTFLAGS = $origRUSTFLAGS
            return $false
        }
        # Strip debug symbols from libc++_shared.so (NDK ships unstripped, ~6.6MB -> ~800KB)
        $llvmStrip = Get-Command "llvm-strip" -ErrorAction SilentlyContinue
        if (-not $llvmStrip) {
            $llvmStrip = Get-Command (Join-Path $env:ANDROID_NDK_HOME "toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe") -ErrorAction SilentlyContinue
        }
        if ($llvmStrip) {
            & $llvmStrip.Source --strip-all $libcppDst
            if ($LASTEXITCODE -ne 0) {
                Print-Error "Unable to strip $libcppDst"
                $env:RUSTFLAGS = $origRUSTFLAGS
                return $false
            }
            Print-Success "Copied + stripped -> $libcppDst"
        } else {
            Print-Success "Copied -> $libcppDst (llvm-strip not found, skipped stripping)"
        }
    } else {
        Print-Error "libc++_shared.so not found: $libcppSrc"
        $env:RUSTFLAGS = $origRUSTFLAGS
        return $false
    }

    $env:RUSTFLAGS = $origRUSTFLAGS
    return $true
}

# ------------------------------------------------------------
# Main
# ------------------------------------------------------------
$buildType = "release"
$productProfile = "full"
$codegenProfile = "z"
$workerSnapshot = $false
$platforms = @()

for ($i = 0; $i -lt $Args.Count; $i++) {
    $arg = $Args[$i]
    if ($arg -eq "release") {
        $buildType = "release"
    } elseif ($arg -eq "debug") {
        $buildType = "debug"
    } elseif ($arg -eq "--product-profile") {
        $i++
        if ($i -ge $Args.Count) { throw "--product-profile requires full|slim" }
        $productProfile = $Args[$i]
    } elseif ($arg -like "--product-profile=*") {
        $productProfile = $arg.Substring("--product-profile=".Length)
    } elseif ($arg -eq "--codegen-profile") {
        $i++
        if ($i -ge $Args.Count) { throw "--codegen-profile requires z|2|3" }
        $codegenProfile = $Args[$i]
    } elseif ($arg -like "--codegen-profile=*") {
        $codegenProfile = $arg.Substring("--codegen-profile=".Length)
    } elseif ($arg -eq "--worker-snapshot") {
        $workerSnapshot = $true
    } elseif ($PLATFORM_MAP.ContainsKey($arg)) {
        if ($arg -eq "all") {
            $platforms = @("arm64-v8a", "x86_64")
        } else {
            $platforms += $arg
        }
    } else {
        throw "Unknown argument: $arg"
    }
}

if ($productProfile -notin @("full", "slim")) {
    throw "Invalid product profile '$productProfile' (expected full|slim)"
}
if ($codegenProfile -notin @("z", "2", "3")) {
    throw "Invalid codegen profile '$codegenProfile' (expected z|2|3)"
}
if ($buildType -eq "debug" -and $codegenProfile -ne "z") {
    throw "Codegen profile $codegenProfile requires a release build"
}
if ($workerSnapshot -and ($buildType -ne "release" -or $productProfile -ne "full")) {
    throw "Worker snapshot requires a full release build"
}

Check-Dependencies
$paths = Resolve-Paths

if ($platforms.Count -eq 0) {
    $platforms = @("arm64-v8a", "x86_64")
    Print-Info "No platform specified, building default ABIs"
}

Print-Info "Build type : $buildType"
Print-Info "Product    : $productProfile"
Print-Info "Codegen    : $codegenProfile"
Print-Info "Worker snap: $workerSnapshot"
Print-Info "Platforms  : $($platforms -join ', ')"

$failed = @()
foreach ($p in $platforms) {
    if (-not (Build-Platform $p $buildType $productProfile $codegenProfile $workerSnapshot $paths)) {
        $failed += $p
    }
}

if ($failed.Count -eq 0) {
    Print-Success "All Android builds succeeded"
    exit 0
} else {
    Print-Error "Failed platforms: $($failed -join ', ')"
    exit 1
}
