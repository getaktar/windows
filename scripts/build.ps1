<#
.SYNOPSIS
Builds a release of Aktar that doesn't carry the build machine's paths.

.DESCRIPTION
Runs `pnpm tauri build` with the arguments given, with the path remapping
from scripts/remap_paths.ps1, then checks aktar.exe with
scripts/check_binary.ps1: if the home folder or user name is still in it,
the build fails. The Mac app does the same for its C dependencies
(scripts/release.sh).

.EXAMPLE
pwsh scripts/build.ps1                # the NSIS installer
pwsh scripts/build.ps1 --no-bundle    # just aktar.exe
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$env:CARGO_ENCODED_RUSTFLAGS = & (Join-Path $PSScriptRoot 'remap_paths.ps1')
# RUSTFLAGS would be ignored next to CARGO_ENCODED_RUSTFLAGS anyway.
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue

Push-Location $root
try {
    pnpm tauri build @args
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
} finally { Pop-Location }

& (Join-Path $PSScriptRoot 'check_binary.ps1') -Path (Join-Path $root 'src-tauri\target\release\aktar.exe')
