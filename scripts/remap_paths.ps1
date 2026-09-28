<#
.SYNOPSIS
Returns rustc flags that keep the build machine's paths out of the binary.

.DESCRIPTION
Rust embeds the source paths of every crate it compiles (for panic
messages), so a plain build puts paths like C:\Users\<name>\.cargo\... into
aktar.exe, and with them the build machine's user name. These flags map the
home folder, Rust's folders, and the repository to neutral names.

The result is for CARGO_ENCODED_RUSTFLAGS (flags separated by 0x1F, so
paths with spaces survive), including any flags already set there.
scripts/build.ps1 and the release workflows use it; scripts/check_binary.ps1
verifies the result.
#>
$root = Split-Path -Parent $PSScriptRoot
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }

# When several prefixes match, rustc applies the last one, so the most
# specific goes last.
$remaps = [ordered]@{
    $env:USERPROFILE = 'C:\home'
    $rustupHome = 'C:\rustup'
    $cargoHome = 'C:\cargo'
    $root = 'C:\aktar'
}
$flags = foreach ($from in $remaps.Keys) { "--remap-path-prefix=$from=$($remaps[$from])" }
if ($env:CARGO_ENCODED_RUSTFLAGS) { $flags = @($env:CARGO_ENCODED_RUSTFLAGS -split [char]0x1F) + $flags }
$flags -join [char]0x1F
