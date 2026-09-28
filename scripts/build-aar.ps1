#Requires -Version 7.0
# PowerShell 7 or newer, and this is a refusal rather than a preference: the host checks
# below use `$IsWindows`, which Windows PowerShell 5.1 does not define. There it would
# evaluate false and the script would silently choose the Unix `gradlew` wrapper and an
# extensionless manifest tool -- on Windows, the one host this entry point exists for. A
# version requirement makes that unrepresentable; handling it would leave two
# host-detection rules to keep in agreement.

param(
    [ValidateSet("release", "debug")]
    [string]$BuildType = "release",
    [ValidateSet("full", "slim")]
    [string]$ProductProfile = "full",
    [ValidateSet("z", "2", "3")]
    [string]$CodegenProfile = "z",
    [ValidateSet("required", "optional", "off", "")]
    [string]$ArtifactManifest = "",
    [string]$OutputDir = "dist",
    [string[]]$Architectures = @("all"),
    [switch]$SkipRustBuild = $false,
    [switch]$UnverifiedNativeLibs = $false,
    [switch]$WorkerSnapshot = $false,
    [switch]$Help = $false
)

# =========================
# Help
# =========================
if ($Help) {
    Write-Host "MiniGame Android AAR Builder"
    Write-Host ""
    Write-Host "Usage:"
    Write-Host "  .\build-aar.ps1 [-BuildType release|debug] [-CodegenProfile z|2|3]"
    Write-Host "                    [-Architectures all|arm64-v8a,...]"
    Write-Host "                    [-ArtifactManifest required|optional|off]"
    Write-Host "                    [-SkipRustBuild] [-UnverifiedNativeLibs] [-WorkerSnapshot]"
    Write-Host "  -ArtifactManifest Manifest policy (release requires required; debug defaults optional)"
    Write-Host "  -UnverifiedNativeLibs  Package .so files this invocation did not build."
    Write-Host "                    Only meaningful with -SkipRustBuild. The result is not"
    Write-Host "                    a release artifact and must not be published."
    exit 0
}

# =========================
# Path Resolution
# =========================
$ScriptDir  = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot   = Resolve-Path (Join-Path $ScriptDir "..")
$AndroidDir = Join-Path $RepoRoot "platforms/android"
$LibraryDir = Join-Path $AndroidDir "library"

if ($BuildType -eq "debug" -and $CodegenProfile -ne "z") {
    throw "Codegen profile $CodegenProfile requires a release build"
}
if ($WorkerSnapshot.IsPresent -and ($BuildType -ne "release" -or $ProductProfile -ne "full")) {
    throw "Worker snapshot requires a full release build"
}

# A release AAR must carry native libraries built from this source. -SkipRustBuild
# packages whatever .so files are on disk and the validation step only checks that
# they exist, so a release built this way ships natives from another commit with
# nothing in the artifact saying so. The bash twin carries the full reasoning.
if ($BuildType -eq "release" -and $SkipRustBuild -and -not $UnverifiedNativeLibs) {
    throw "Release AARs cannot be built with -SkipRustBuild: the packaged native libraries would not be built from this source"
}
if ($UnverifiedNativeLibs -and -not $SkipRustBuild) {
    throw "-UnverifiedNativeLibs is only meaningful with -SkipRustBuild"
}

# The Gradle release gate refuses to package unless verified inputs have been
# staged, and its refusal names `scripts/build-aar.sh` -- so before this, a release
# built through the PowerShell entry point could not succeed at all, and failed
# deep inside Gradle after the Rust build rather than at argument time. The policy
# is the bash twin's: release means required, debug defaults to optional.
if ([string]::IsNullOrEmpty($ArtifactManifest)) {
    $ArtifactManifest = if ($BuildType -eq "release") { "required" } else { "optional" }
}
if ($BuildType -eq "release" -and $ArtifactManifest -ne "required") {
    throw "Release AARs require -ArtifactManifest required"
}

$SourceDateEpoch = $env:SOURCE_DATE_EPOCH
$SourceDateEpochMetadata = $null
if (-not [string]::IsNullOrEmpty($SourceDateEpoch)) {
    [long]$SourceDateEpochSeconds = 0
    if ($SourceDateEpoch -notmatch '^[0-9]+$' -or
            -not [long]::TryParse($SourceDateEpoch, [ref]$SourceDateEpochSeconds) -or
            $SourceDateEpochSeconds -gt 9223372036854775) {
        throw "Invalid SOURCE_DATE_EPOCH: expected non-negative Unix seconds that fit in milliseconds"
    }
    $SourceDateEpochMetadata = $SourceDateEpoch
}

# The timestamp a shipped artifact may carry: SOURCE_DATE_EPOCH if set, else now,
# and UTC either way. The bash twin's `scripts/lib/reproducible-timestamp.sh`
# carries the reasoning. This wrote a *local* wall clock beside the epoch it had
# just recorded, so the same source produced different bytes in two timezones and
# in two minutes.
# Self-contained on purpose: it reads the environment rather than an outer-scope
# variable, so it does not depend on where in the script it is called from, and
# every expression is a single line because PowerShell ends a statement at a
# newline -- a leading-dot continuation is a C# habit that does not parse here.
# This file cannot be run on the machine that wrote it, so it assumes as little as
# possible.
function Get-ReproducibleTimestamp {
    $epoch = $env:SOURCE_DATE_EPOCH
    if ([string]::IsNullOrEmpty($epoch)) {
        $nowUtc = (Get-Date).ToUniversalTime()
        return $nowUtc.ToString("yyyy-MM-ddTHH:mm:ssZ")
    }
    [long]$seconds = 0
    if ($epoch -notmatch '^[0-9]+$' -or -not [long]::TryParse($epoch, [ref]$seconds)) {
        throw "SOURCE_DATE_EPOCH must be non-negative Unix seconds, got: $epoch"
    }
    $stamp = [DateTimeOffset]::FromUnixTimeSeconds($seconds)
    $utc = $stamp.UtcDateTime
    return $utc.ToString("yyyy-MM-ddTHH:mm:ssZ")
}

$CodegenSuffix = ""
$CargoProfile = "debug"
if ($BuildType -eq "release") {
    switch ($CodegenProfile) {
        "z" { $CargoProfile = "release" }
        "2" {
            $CargoProfile = "release-hot2"
            $CodegenSuffix = "-opt2"
        }
        "3" {
            $CargoProfile = "release-hot3"
            $CodegenSuffix = "-opt3"
        }
    }
}
$WorkerSnapshotSuffix = if ($WorkerSnapshot.IsPresent) { "-worker-snapshot" } else { "" }
$ArtifactSuffix = "$CodegenSuffix$WorkerSnapshotSuffix"

$RustBuildScript     = Join-Path $ScriptDir "build-android-so.ps1"
$ExternalJniLibs     = Join-Path $RepoRoot "engine/jniLibs/$ProductProfile$ArtifactSuffix"

# The staged-identity inputs. Every path here is the one build-aar.sh uses, because
# the Gradle gate reads them by location: a second opinion about where the package
# index lives is a release that gates on nothing.
$ManifestGenerator    = Join-Path $ScriptDir "generate-android-artifact-manifests.py"
$BuildMetadataWriter  = Join-Path $ScriptDir "write-android-build-metadata.py"
$AarManifestVerifier  = Join-Path $ScriptDir "verify-android-aar-manifests.py"
$ManifestToolManifest = Join-Path $RepoRoot "tools/artifact-manifest/Cargo.toml"
$ManifestBuildRoot    = Join-Path $LibraryDir "build/generated/migoArtifactManifest"
$ManifestAssetRoot    = Join-Path $ManifestBuildRoot "assets/migo/artifacts"
$ManifestIndex        = Join-Path $ManifestAssetRoot "package-index.json"
$NdkLock              = Join-Path $RepoRoot "contracts/artifact-manifest/android-v8.lock.json"
$ManifestTool         = $null

Write-Host "========================================"
Write-Host "MiniGame Android AAR Builder"
Write-Host "========================================"
Write-Host "RepoRoot:   $RepoRoot"
Write-Host "AndroidDir: $AndroidDir"
Write-Host "LibraryDir: $LibraryDir"
Write-Host ""

# =========================
# Sanity Checks
# =========================
if (-not (Test-Path $AndroidDir)) {
    throw "Android directory not found: $AndroidDir"
}
if (-not (Test-Path $LibraryDir)) {
    throw "Android library module not found: $LibraryDir"
}
if (-not (Test-Path $RustBuildScript)) {
    throw "Rust build script not found: $RustBuildScript"
}

# =========================
# Build Rust (.so)
# =========================
function Build-RustLibrary {
    param([string[]]$TargetArchitectures)

    if ($SkipRustBuild) {
        Write-Host "Skipping Rust build"
        return
    }

    Write-Host "Building Rust Android .so..."

    Push-Location $ScriptDir
    try {
        foreach ($arch in $TargetArchitectures) {
            Write-Host "→ Rust build: $arch ($BuildType, codegen=$CodegenProfile)"
            $rustArgs = @(
                $arch,
                $BuildType,
                "--product-profile=$ProductProfile",
                "--codegen-profile=$CodegenProfile"
            )
            if ($WorkerSnapshot.IsPresent) {
                $rustArgs += "--worker-snapshot"
            }
            & $RustBuildScript @rustArgs

            if ($LASTEXITCODE -ne 0) {
                throw "Rust build failed for $arch"
            }
        }
    }
    finally {
        Pop-Location
    }

    Write-Host "✓ Rust build done"
}

# =========================
# Validate JNI Libraries
# =========================
function Test-NativeLibraries {
    param([string[]]$TargetArchitectures)

    Write-Host "Validating $ProductProfile JNI libraries..."

    if (-not (Test-Path $ExternalJniLibs)) {
        throw "jniLibs directory not found: $ExternalJniLibs"
    }

    foreach ($arch in $TargetArchitectures) {
        $src = Join-Path $ExternalJniLibs $arch
        if (-not (Test-Path $src)) {
            throw "Missing native libs for $ProductProfile/$arch"
        }
        foreach ($library in @("libmigo.so", "libc++_shared.so")) {
            if (-not (Test-Path (Join-Path $src $library))) {
                throw "Missing $ProductProfile/$arch/$library"
            }
        }
        Write-Host "✓ $ProductProfile/$arch ready"
    }
}

# =========================
# Verified artifact identity
# =========================
# A full commit is required rather than derived loosely: the identity is what the
# artifact is stamped with, and an abbreviated or absent revision would make two
# different sources indistinguishable.
function Resolve-SourceRevision {
    $revision = $env:MIGO_SOURCE_REVISION
    if ([string]::IsNullOrWhiteSpace($revision)) { $revision = $env:GITHUB_SHA }
    if ([string]::IsNullOrWhiteSpace($revision)) {
        $revision = (& git -C $RepoRoot rev-parse HEAD 2>$null)
        if ($LASTEXITCODE -ne 0) {
            throw "Cannot read the source revision; set MIGO_SOURCE_REVISION to the full commit"
        }
    }
    $revision = $revision.Trim()
    if ($revision -notmatch '^[0-9a-fA-F]{40}$') {
        throw "A full MIGO_SOURCE_REVISION/GITHUB_SHA is required for artifact identity"
    }
    return $revision
}

# Generates the per-slice identities Gradle's release gate requires. The work is
# done by the same generator, metadata writer and Rust tool the shell entry point
# drives -- this function only arranges their inputs, so there is one implementation
# of what a manifest says and two ways to ask for it.
function Stage-ArtifactManifests {
    param([Parameter(Mandatory)][string[]]$TargetArchitectures)

    if ($ArtifactManifest -eq "off") {
        Write-Host "Artifact manifest generation is disabled for this non-release build"
        return
    }

    try {
        foreach ($required in $ManifestGenerator, $BuildMetadataWriter,
                              $AarManifestVerifier, $ManifestToolManifest) {
            if (-not (Test-Path -LiteralPath $required)) {
                throw "Missing artifact manifest input: $required"
            }
        }
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
            throw "cargo is required for artifact manifests"
        }
        if (-not (Get-Command python3 -ErrorAction SilentlyContinue)) {
            throw "python3 is required for artifact manifests"
        }

        Import-Module (Join-Path $ScriptDir "lib/AndroidNdk.psm1") -Force
        $ndkHome = Resolve-MigoPinnedNdk -Lock $NdkLock

        $toolTarget = $env:MIGO_ARTIFACT_MANIFEST_TARGET_DIR
        if ([string]::IsNullOrWhiteSpace($toolTarget)) {
            $toolTarget = Join-Path $RepoRoot "tools/artifact-manifest/target"
        }
        $previousTargetDir = $env:CARGO_TARGET_DIR
        try {
            $env:CARGO_TARGET_DIR = $toolTarget
            & cargo build --manifest-path $ManifestToolManifest --locked --release
            if ($LASTEXITCODE -ne 0) { throw "Building migo-artifact-manifest failed" }
        }
        finally {
            $env:CARGO_TARGET_DIR = $previousTargetDir
        }

        $toolName = if ($IsWindows) { "migo-artifact-manifest.exe" } else { "migo-artifact-manifest" }
        $tool = Join-Path $toolTarget "release/$toolName"
        if (-not (Test-Path -LiteralPath $tool)) { throw "Manifest tool was not produced: $tool" }

        $metadata = Join-Path $ManifestBuildRoot "build-metadata.json"
        & python3 $BuildMetadataWriter `
            --repo-root $RepoRoot `
            --output $metadata `
            --ndk-home $ndkHome `
            --source-revision (Resolve-SourceRevision)
        if ($LASTEXITCODE -ne 0) { throw "Writing build metadata failed" }

        $generatorArgs = [System.Collections.Generic.List[string]]::new()
        $generatorArgs.AddRange([string[]]@(
            "--repo-root", $RepoRoot,
            "--tool", $tool,
            "--output-root", $ManifestAssetRoot,
            "--build-metadata", $metadata,
            "--product-profile", $ProductProfile,
            "--build-type", $BuildType,
            "--codegen-profile", $CodegenProfile
        ))
        if ($WorkerSnapshot.IsPresent) { $generatorArgs.Add("--worker-snapshot") }
        foreach ($arch in $TargetArchitectures) {
            $generatorArgs.AddRange([string[]]@("--arch", $arch))
        }
        & python3 $ManifestGenerator @generatorArgs
        if ($LASTEXITCODE -ne 0) { throw "Generating artifact manifests failed" }

        $script:ManifestTool = $tool
        Write-Host "✓ Verified artifact manifests staged"
    }
    catch {
        # Partial identity is worse than none: Gradle would read whatever landed.
        if (Test-Path -LiteralPath $ManifestBuildRoot) {
            Remove-Item -LiteralPath $ManifestBuildRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
        if ($ArtifactManifest -eq "required") { throw }
        Write-Warning "Verified artifact manifests unavailable; continuing debug-only build: $($_.Exception.Message)"
    }
}

# =========================
# Build AAR
# =========================
function Build-AAR {
    param([string[]]$TargetArchitectures)

    Write-Host "Building AAR..."

    Push-Location $AndroidDir
    try {
        # PowerShell 7 is cross-platform, and the wrapper's name is not: probing only
        # for gradlew.bat made this entry point unrunnable anywhere except Windows,
        # which is why its packaging defects went unobserved for so long. The
        # repository ships both wrappers, so prefer the one for this host.
        $wrapper = if ($IsWindows) { ".\gradlew.bat" } else { "./gradlew" }
        if (Test-Path -LiteralPath $wrapper) {
            $gradle = $wrapper
        } elseif (Get-Command gradle -ErrorAction SilentlyContinue) {
            $gradle = "gradle"
        } else {
            throw "Gradle not found"
        }

        & $gradle clean
        if ($LASTEXITCODE -ne 0) { throw "Gradle clean failed" }

        # clean removes generated assets, so identity is staged only after it
        # succeeds -- the same ordering the shell entry point depends on.
        Stage-ArtifactManifests -TargetArchitectures $TargetArchitectures

        $profileTask = (Get-Culture).TextInfo.ToTitleCase($ProductProfile)
        $typeTask = (Get-Culture).TextInfo.ToTitleCase($BuildType)
        $abiProperty = "-PmigoAbis=" + ($TargetArchitectures -join ",")
        $codegenProperty = "-PmigoCodegenProfile=$CodegenProfile"
        $workerSnapshotProperty = "-PmigoWorkerSnapshot=$($WorkerSnapshot.IsPresent.ToString().ToLowerInvariant())"
        $gradleArgs = [System.Collections.Generic.List[string]]::new()
        $gradleArgs.AddRange([string[]]@($abiProperty, $codegenProperty, $workerSnapshotProperty))
        if ($BuildType -eq "release") {
            if (-not $ManifestTool) {
                throw "Release packaging requires the staged artifact manifest tool"
            }
            $gradleArgs.Add("-PmigoVerifiedReleasePackaging=true")
            $gradleArgs.Add("-PmigoArtifactManifestTool=$ManifestTool")
        }
        $gradleArgs.Add("assemble$profileTask$typeTask")
        & $gradle @gradleArgs

        if ($LASTEXITCODE -ne 0) {
            throw "AAR build failed"
        }
    }
    finally {
        Pop-Location
    }

    Write-Host "✓ AAR build success"
}

# =========================
# Collect Outputs
# =========================
function Collect-Outputs {
    param([Parameter(Mandatory)][string[]]$TargetArchitectures)

    Write-Host "Collecting outputs..."

    $outDir = Join-Path $AndroidDir $OutputDir
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null

    # A single-ABI build is a distinct product, not a variant spelling of the
    # multi-ABI one: per-ABI size is what a host weighs against its APK budget. Without
    # this suffix the two overwrite each other under one name, and the shell twin
    # already distinguishes them.
    $abiSuffix = if ($TargetArchitectures.Count -eq 1) { "-$($TargetArchitectures[0])" } else { "" }

    $aarDir = Join-Path $LibraryDir "build/outputs/aar"
    $aar = Join-Path $aarDir "migo-$ProductProfile-$BuildType.aar"
    if (-not (Test-Path $aar)) { throw "Expected AAR not found: $aar" }

    $artifactName = "migo-$ProductProfile-$BuildType$ArtifactSuffix$abiSuffix.aar"
    $outputAar = Join-Path $outDir $artifactName
    $attestation = "$outputAar.attestation.json"
    $versionMetadata = Join-Path $outDir "version-$ProductProfile$ArtifactSuffix$abiSuffix.json"
    # Removed before anything is written, the way the shell twin does it: an
    # attestation left from an earlier run would otherwise sit beside an AAR it does
    # not describe, and a sidecar that names the wrong bytes is worse than none.
    foreach ($stale in $outputAar, $attestation, $versionMetadata) {
        if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force }
    }

    # The attestation is checked against the AAR that was actually produced, not
    # inferred from the fact that staging succeeded before Gradle ran.
    if (Test-Path -LiteralPath $ManifestIndex) {
        & python3 $AarManifestVerifier --aar $aar --index $ManifestIndex --tool $ManifestTool
        if ($LASTEXITCODE -ne 0) { throw "AAR manifest verification failed" }
    }
    elseif ($ArtifactManifest -eq "required") {
        throw "Required package index was not generated: $ManifestIndex"
    }

    Copy-Item $aar $outputAar -Force

    # The external sidecar, which the embedded-index check does not produce. Without
    # it a PowerShell release is not the same package as a shell one: release.yml and
    # the consumers expect `<aar>.attestation.json` beside the archive.
    if (Test-Path -LiteralPath $ManifestIndex) {
        & $ManifestTool attest $outputAar $ManifestIndex $attestation | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Producing the release attestation failed" }
        & $ManifestTool verify-attestation $attestation $outputAar $ManifestIndex | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Verifying the release attestation failed" }
        Write-Host "✓ Release attestation -> $attestation"
    }

    @{
        productProfile = $ProductProfile
        buildType = $BuildType
        codegenProfile = $CodegenProfile
        cargoProfile = $CargoProfile
        workerSnapshot = $WorkerSnapshot.IsPresent
        sourceDateEpoch = $SourceDateEpochMetadata
        buildTime = (Get-ReproducibleTimestamp)
    } | ConvertTo-Json | Out-File $versionMetadata -Encoding utf8

    Write-Host "✓ Outputs ready: $outDir"
}

# =========================
# Main
# =========================
$SupportedArchitectures = @("arm64-v8a", "x86_64")
if ($Architectures -contains "all") {
    $ResolvedArchitectures = $SupportedArchitectures
}
else {
    $ResolvedArchitectures = @($Architectures | Select-Object -Unique)
    foreach ($arch in $ResolvedArchitectures) {
        if ($SupportedArchitectures -notcontains $arch) {
            throw "Unsupported architecture: $arch"
        }
    }
}

Build-RustLibrary -TargetArchitectures $ResolvedArchitectures
Test-NativeLibraries -TargetArchitectures $ResolvedArchitectures
Build-AAR -TargetArchitectures $ResolvedArchitectures
Collect-Outputs -TargetArchitectures $ResolvedArchitectures

Write-Host ""
Write-Host "========================================"
Write-Host "✅ Android AAR build completed"
Write-Host "========================================"
