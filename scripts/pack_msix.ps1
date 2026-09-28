<#
.SYNOPSIS
Packages Aktar as an MSIX for the Microsoft Store.

.DESCRIPTION
Puts the release build of aktar.exe, the logos in packaging/msix/Assets, and
packaging/msix/AppxManifest.xml (filled in from packaging/msix/identity.json
and the version in src-tauri/tauri.conf.json) into
src-tauri/target/msix/Aktar_<version>_x64.msix.

The package is left unsigned: the Store signs it when it's submitted. To
install it on this PC first, pass -SignForTesting, which signs it with a
self-signed certificate made for the purpose (see the instructions printed
at the end).

Needs the Windows SDK (makeappx, makepri, signtool), which comes with the
Visual Studio Build Tools' C++ workload and is on GitHub's Windows runners.

.EXAMPLE
pwsh scripts/pack_msix.ps1 -Build
Builds the release exe (no NSIS installer, with scripts/build.ps1, which
keeps the build machine's paths out of it) and packages it.

.EXAMPLE
pwsh scripts/pack_msix.ps1 -SignForTesting
Packages an exe built earlier and signs it for a local install.
#>
[CmdletBinding()]
param(
    # Runs `scripts/build.ps1 --no-bundle` first.
    [switch]$Build,
    # Signs with a self-signed certificate so the package installs locally.
    [switch]$SignForTesting,
    [string]$ExePath,
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $ExePath) { $ExePath = Join-Path $root 'src-tauri\target\release\aktar.exe' }
if (-not $OutDir) { $OutDir = Join-Path $root 'src-tauri\target\msix' }
$packaging = Join-Path $root 'packaging\msix'

function Find-SdkTool([string]$name) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $tool = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "x64\$name" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    if (-not $tool) { throw "$name not found. Install the Windows SDK (Visual Studio Build Tools, C++ workload)." }
    $tool
}

function Invoke-Tool([string]$tool, [string[]]$arguments) {
    & $tool @arguments | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "$(Split-Path -Leaf $tool) failed with exit code $LASTEXITCODE" }
}

if ($Build) {
    & (Join-Path $PSScriptRoot 'build.ps1') --no-bundle
}
if (-not (Test-Path $ExePath)) { throw "No build at $ExePath. Run with -Build, or scripts/build.ps1 first." }
# Whatever built it, nothing goes into a package with the build machine's
# user name in it.
& (Join-Path $PSScriptRoot 'check_binary.ps1') -Path $ExePath

# The Store wants four-part versions ending in .0: 0.1.1 -> 0.1.1.0.
$config = Get-Content (Join-Path $root 'src-tauri\tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$version = "$($config.version -replace '[-+].*$', '').0"
$identity = Get-Content (Join-Path $packaging 'identity.json') -Raw -Encoding UTF8 | ConvertFrom-Json

function ConvertTo-XmlText([string]$text) { [System.Security.SecurityElement]::Escape($text) }

# Staging: exactly what goes into the package.
$stage = Join-Path $OutDir 'stage'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $ExePath (Join-Path $stage 'aktar.exe')
Copy-Item (Join-Path $packaging 'Assets') (Join-Path $stage 'Assets') -Recurse
$manifest = Get-Content (Join-Path $packaging 'AppxManifest.xml') -Raw -Encoding UTF8
$values = [ordered]@{
    '{{IDENTITY_NAME}}' = ConvertTo-XmlText $identity.name
    '{{PUBLISHER}}' = ConvertTo-XmlText $identity.publisher
    '{{PUBLISHER_DISPLAY_NAME}}' = ConvertTo-XmlText $identity.publisherDisplayName
    '{{VERSION}}' = $version
}
foreach ($placeholder in $values.Keys) { $manifest = $manifest.Replace($placeholder, $values[$placeholder]) }
[IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, (New-Object Text.UTF8Encoding $false))

# resources.pri lets Windows pick the logo for each size and display scale.
$makepri = Find-SdkTool 'makepri.exe'
$priConfig = Join-Path $OutDir 'priconfig.xml'
Invoke-Tool $makepri @('createconfig', '/cf', $priConfig, '/dq', 'en-US', '/pv', '10.0.0', '/o')
Invoke-Tool $makepri @('new', '/pr', $stage, '/cf', $priConfig, '/mn', (Join-Path $stage 'AppxManifest.xml'), '/of', (Join-Path $stage 'resources.pri'), '/o')

$package = Join-Path $OutDir "Aktar_${version}_x64.msix"
Invoke-Tool (Find-SdkTool 'makeappx.exe') @('pack', '/d', $stage, '/p', $package, '/o')

if ($SignForTesting) {
    # The certificate's subject has to match the manifest's Publisher.
    $certificate = Get-ChildItem Cert:\CurrentUser\My |
        Where-Object { $_.Subject -eq $identity.publisher -and $_.FriendlyName -eq 'Aktar MSIX test signing' } |
        Select-Object -First 1
    if (-not $certificate) {
        $certificate = New-SelfSignedCertificate -Type Custom -Subject $identity.publisher `
            -KeyUsage DigitalSignature -FriendlyName 'Aktar MSIX test signing' `
            -CertStoreLocation Cert:\CurrentUser\My `
            -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
    }
    Invoke-Tool (Find-SdkTool 'signtool.exe') @('sign', '/fd', 'SHA256', '/sha1', $certificate.Thumbprint, $package)
    $cer = Join-Path $OutDir 'Aktar-test-signing.cer'
    Export-Certificate -Cert $certificate -FilePath $cer | Out-Null
    Write-Host ''
    Write-Host 'To install it on this PC, trust the test certificate once (as administrator):'
    Write-Host "  Import-Certificate -FilePath `"$cer`" -CertStoreLocation Cert:\LocalMachine\TrustedPeople"
    Write-Host 'then open the .msix, or run:'
    Write-Host "  Add-AppxPackage `"$package`""
}

Write-Host ''
Write-Host "Package: $package"
