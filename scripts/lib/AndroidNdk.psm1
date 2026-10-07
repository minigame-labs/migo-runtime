# Resolving the pinned Android NDK on the PowerShell entry points.
# Location: scripts/lib/AndroidNdk.psm1
#
# The counterpart of scripts/lib/android-ndk.sh, and it exists because the
# pinned NDK requirement was enforced on the shell path only.
# The PowerShell path accepted `$env:ANDROID_NDK_HOME` without checking it, so a
# Windows build could link the pinned V8 archive with any NDK -- and the NDK's
# compiler, sysroot and linker are all recorded in the component manifest as
# part of the artifact's identity. The enumeration meant to prevent exactly
# this globbed `*.sh`, so no gate could see the entry points that are not shell
# scripts.
#
# Selection is by the NDK's own Pkg.Revision rather than by directory name, for the
# same reason as the shell version: a directory called `ndk/23.2.8568313` is just a
# name, while Pkg.Revision is the fact the manifest stamps into the artifact. An
# explicit ANDROID_NDK_HOME is honoured but checked like any other candidate, so an
# override cannot substitute a different toolchain silently.

Set-StrictMode -Version Latest

# The NDK's own record of what it is.
function Get-MigoNdkRevision {
    param([Parameter(Mandatory)][string]$NdkHome)

    $properties = Join-Path $NdkHome "source.properties"
    if (-not (Test-Path -LiteralPath $properties)) { return $null }
    foreach ($line in Get-Content -LiteralPath $properties) {
        if ($line -match '^Pkg\.Revision\s*=\s*(\S+)') { return $Matches[1] }
    }
    return $null
}

# The pinned version, read from the same build lock the shell resolver reads.
function Read-MigoNdkPin {
    param([Parameter(Mandatory)][string]$Lock)

    if (-not (Test-Path -LiteralPath $Lock)) { throw "missing build lock: $Lock" }
    $pin = (Get-Content -LiteralPath $Lock -Raw | ConvertFrom-Json).ndk.version
    if ([string]::IsNullOrWhiteSpace($pin)) { throw "build lock has no ndk.version: $Lock" }
    return $pin
}

# Sets $env:ANDROID_NDK_HOME to an NDK whose own Pkg.Revision equals the pin, and
# returns it. $env:ANDROID_NDK is set too: skia-bindings' build script reads that
# name rather than ANDROID_NDK_HOME, which the shell path also has to do.
function Resolve-MigoPinnedNdk {
    param([Parameter(Mandatory)][string]$Lock)

    $pin = Read-MigoNdkPin -Lock $Lock

    $candidates = [System.Collections.Generic.List[string]]::new()
    foreach ($direct in $env:ANDROID_NDK_HOME, $env:ANDROID_NDK_ROOT) {
        if (-not [string]::IsNullOrWhiteSpace($direct)) { $candidates.Add($direct) }
    }
    foreach ($root in $env:ANDROID_HOME, $env:ANDROID_SDK_ROOT,
                      $(if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA "Android/Sdk" }),
                      (Join-Path $HOME "Android/Sdk"),
                      (Join-Path $HOME "Library/Android/sdk")) {
        if (-not [string]::IsNullOrWhiteSpace($root)) {
            $candidates.Add((Join-Path $root "ndk/$pin"))
        }
    }

    $mismatches = [System.Collections.Generic.List[string]]::new()
    foreach ($candidate in $candidates) {
        if (-not (Test-Path -LiteralPath $candidate)) { continue }
        $revision = Get-MigoNdkRevision -NdkHome $candidate
        if ($null -eq $revision) { continue }
        if ($revision -eq $pin) {
            $env:ANDROID_NDK_HOME = $candidate
            $env:ANDROID_NDK = $candidate
            return $candidate
        }
        # Reported as it happens, not only when nothing matches. A caller who set
        # ANDROID_NDK_HOME and got a different NDK has to be told; falling through
        # in silence is the substitution the pin exists to prevent, even though
        # what it falls through to is the right toolchain.
        $mismatch = "$candidate is NDK $revision, the lock pins $pin"
        Write-Warning $mismatch
        $mismatches.Add($mismatch)
    }

    $looked = if ($candidates.Count -gt 0) { $candidates -join ", " } else { "(nothing)" }
    $detail = if ($mismatches.Count -gt 0) { "`n" + ($mismatches -join "`n") } else { "" }
    throw ("no Android NDK $pin found$detail`nlooked at: $looked`n" +
           "install it with: sdkmanager 'ndk;$pin'`nor set ANDROID_NDK_HOME to that NDK")
}

Export-ModuleMember -Function Get-MigoNdkRevision, Read-MigoNdkPin, Resolve-MigoPinnedNdk
