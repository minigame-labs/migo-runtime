# Materialising a verified V8 archive under a content-addressed path, for the PowerShell
# entry points.
# Location: scripts/lib/V8Materialise.psm1
#
# The counterpart of scripts/lib/v8-materialise.sh, and it exists for the same reason
# AndroidNdk.psm1 does: the rule was enforced on the shell path only.
# `build-android-so.ps1` selected its archive with `Test-Path` -- existence, not identity --
# and `build-aar.ps1` invokes it, so once the PowerShell release path started working that
# was the shipping Windows-host route to an AAR linked against unverified bytes.
#
# The path names both hashes for the same reason the shell version does: cargo reruns the v8
# crate's build script when the *value* of RUSTY_V8_ARCHIVE changes, not when the file at
# that path is replaced, so content in the path is what makes its staleness rule correct.

Set-StrictMode -Version Latest
$MigoV8MaterialiseLibDir = $PSScriptRoot

function Get-MigoFileSha256 {
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

<#
.SYNOPSIS
Verifies a V8 archive and binding against the component manifest beside them, then returns
their paths under a directory named by both hashes.

.DESCRIPTION
A target with no committed manifest is refused rather than materialised: without one there is
nothing to say what the right bytes are, which is the rule scripts/fetch-v8-archives.sh
states for downloading them and scripts/lib/v8-materialise.sh states for using them.
#>
function Resolve-MigoMaterialisedV8 {
    param(
        [Parameter(Mandatory)][string]$V8Dir,
        [Parameter(Mandatory)][string]$Root
    )

    $archive = Join-Path $V8Dir "librusty_v8.a"
    $binding = Join-Path $V8Dir "src_binding.rs"
    $manifest = Join-Path $V8Dir "component-manifest.json"

    foreach ($required in $archive, $binding) {
        if (-not (Test-Path -LiteralPath $required)) {
            throw "missing V8 input: $required"
        }
    }
    if (-not (Test-Path -LiteralPath $manifest)) {
        throw ("no component manifest beside $archive; a target with no committed manifest " +
               "cannot be verified, so it is not built against")
    }

    $python = if (Get-Command python3 -ErrorAction SilentlyContinue) { "python3" } else { "python" }
    & $python (Join-Path $MigoV8MaterialiseLibDir "v8_cohort.py") `
        (Join-Path $MigoV8MaterialiseLibDir "../..") $manifest
    if ($LASTEXITCODE -ne 0) { throw "V8 component cohort rejected before materialisation" }

    $recorded = (Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).hashes
    $gotArchive = Get-MigoFileSha256 -Path $archive
    $gotBinding = Get-MigoFileSha256 -Path $binding
    if ($gotArchive -ne $recorded.archive) {
        throw ("$archive does not match its manifest`n  recorded $($recorded.archive)`n" +
               "  actual   $gotArchive")
    }
    if ($gotBinding -ne $recorded.rust_binding) {
        throw ("$binding does not match its manifest`n  recorded $($recorded.rust_binding)`n" +
               "  actual   $gotBinding")
    }

    $dest = Join-Path $Root "$gotArchive-$gotBinding"
    New-Item -ItemType Directory -Path $dest -Force | Out-Null

    foreach ($pair in @(@{ Name = "librusty_v8.a"; Hash = $gotArchive },
                        @{ Name = "src_binding.rs"; Hash = $gotBinding })) {
        $target = Join-Path $dest $pair.Name
        if (Test-Path -LiteralPath $target) {
            # Re-checked rather than trusted: the path asserts both hashes, so a file that no
            # longer matches the directory naming it is the one thing this must not pass on.
            if ((Get-MigoFileSha256 -Path $target) -ne $pair.Hash) {
                throw "$target does not hash to the directory that names it; refusing to reuse it"
            }
            continue
        }
        # A hard link, because the archive is ~120 MB per architecture. Copy is the fallback
        # for the case a link cannot serve -- a different volume, or a filesystem without them.
        $source = Join-Path $V8Dir $pair.Name
        try {
            New-Item -ItemType HardLink -Path $target -Target $source -ErrorAction Stop | Out-Null
        }
        catch {
            Copy-Item -LiteralPath $source -Destination $target -Force
        }
    }

    return [pscustomobject]@{
        Archive = Join-Path $dest "librusty_v8.a"
        Binding = Join-Path $dest "src_binding.rs"
    }
}

Export-ModuleMember -Function Get-MigoFileSha256, Resolve-MigoMaterialisedV8
