<#
.SYNOPSIS
Fails if a built binary contains the build machine's user name or home
folder.

.DESCRIPTION
A last check before anything is shipped (see scripts/build.ps1). Looks for
the home folder path and the user name, in both 8-bit and UTF-16 text, and
prints the first matches so the source can be found.
#>
param([Parameter(Mandatory)][string]$Path)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Path)) { throw "No binary at $Path" }
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Path))
$texts = @([Text.Encoding]::GetEncoding(28591).GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes))

$needles = @($env:USERPROFILE, $env:USERNAME) | Where-Object { $_ -and $_.Length -ge 3 } | Select-Object -Unique
# A generic account name (CI runners use "runneradmin") says nothing about
# anyone; only look for it as part of a path.
$needles = $needles | ForEach-Object { if ($_ -eq $env:USERNAME) { "\$_\" } else { $_ } }

$found = @()
foreach ($needle in $needles) {
    foreach ($text in $texts) {
        $index = $text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase)
        if ($index -ge 0) {
            $found += $text.Substring([Math]::Max(0, $index - 20), [Math]::Min(140, $text.Length - [Math]::Max(0, $index - 20)))
        }
    }
}
if ($found) {
    Write-Host "The binary contains the build machine's home folder or user name:"
    $found | Select-Object -First 5 | ForEach-Object { Write-Host "  ...$($_ -replace '[^\x20-\x7E]', '.')..." }
    throw "$Path is not safe to ship."
}
Write-Host "Checked $Path`: no home folder or user name inside."
