#!/usr/bin/env pwsh
# PowerShell sibling of scripts/build_vendored_libghostty_vt.sh.
#
# Builds the vendored libghostty-vt static library via `zig build -Demit-lib-vt`.
# Used on Windows hosts (and any other PowerShell-only environment) where the
# Bash script cannot run directly. Mirrors build.rs's invocation so the output
# directory layout (zig-out/lib) is the same.

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $ExtraArgs,

    [Alias('h', '?')]
    [switch] $Help
)

$ErrorActionPreference = 'Stop'

function Show-Usage {
    @"
Usage: build_vendored_libghostty_vt.ps1 [-Help] [-- <extra zig build args>]

Environment variables:
  VENDORED_GHOSTTY_DIR     Override the vendored sources path
                           (default: <repo>/vendor/libghostty-vt)
  LIBGHOSTTY_VT_OPTIMIZE   Zig optimize mode (default: ReleaseFast)
  ZIG                      Path to a specific zig binary

Examples:
  pwsh -File scripts/build_vendored_libghostty_vt.ps1
  pwsh -File scripts/build_vendored_libghostty_vt.ps1 -- -Dtarget=x86_64-windows-msvc
  pwsh -File scripts/build_vendored_libghostty_vt.ps1 -Help
"@
}

if ($Help) {
    Show-Usage
    exit 0
}

$RootDir = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$VendoredDir = if ($env:VENDORED_GHOSTTY_DIR) { $env:VENDORED_GHOSTTY_DIR } else { Join-Path $RootDir 'vendor/libghostty-vt' }
$Optimize = if ($env:LIBGHOSTTY_VT_OPTIMIZE) { $env:LIBGHOSTTY_VT_OPTIMIZE } else { 'ReleaseFast' }
$ZigExe = if ($env:ZIG) { $env:ZIG } else { 'zig' }

$BuildZig = Join-Path $VendoredDir 'build.zig'
if (-not (Test-Path -LiteralPath $BuildZig)) {
    Write-Error "vendored libghostty-vt source not found at $VendoredDir"
    exit 1
}

Push-Location -LiteralPath $VendoredDir
try {
    $ZigArgs = @('build', '-Demit-lib-vt', "-Doptimize=$Optimize")
    if ($ExtraArgs) { $ZigArgs += $ExtraArgs }

    & $ZigExe @ZigArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "zig build failed (exit $LASTEXITCODE)"
        exit $LASTEXITCODE
    }
}
finally {
    Pop-Location
}

Write-Host ""
Write-Host "built libghostty-vt in $VendoredDir/zig-out"
