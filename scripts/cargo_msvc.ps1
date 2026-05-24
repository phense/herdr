#!/usr/bin/env pwsh
# Run `cargo` with Visual Studio's `cl.exe`/`link.exe` and the Windows SDK on
# PATH. Without this, invoking `cargo` from a fresh shell on Windows either
# can't find link.exe (when Visual Studio's env vars aren't loaded) or finds
# the wrong `link.exe` from Git Bash / MSYS2, which is a hard-link utility,
# not the MSVC linker.
#
# Usage:
#   pwsh -File scripts/cargo_msvc.ps1 check --target x86_64-pc-windows-msvc
#   pwsh -File scripts/cargo_msvc.ps1 build --release
#
# Environment overrides:
#   VS_INSTALL_PATH     Force a specific VS install (default: vswhere-discovered)
#   VS_ARCH             Host/target arch for vcvars (default: x64)

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArgs
)

$ErrorActionPreference = 'Stop'

function Find-VsInstallPath {
    if ($env:VS_INSTALL_PATH -and (Test-Path $env:VS_INSTALL_PATH)) {
        return $env:VS_INSTALL_PATH
    }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        $vswhere = "${env:ProgramFiles}\Microsoft Visual Studio\Installer\vswhere.exe"
    }
    if (Test-Path $vswhere) {
        $path = & $vswhere -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if ($LASTEXITCODE -eq 0 -and $path) { return $path.Trim() }
    }
    foreach ($candidate in @(
        'C:\BuildTools',
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\BuildTools",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Community",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Professional",
        "${env:ProgramFiles}\Microsoft Visual Studio\2022\Enterprise"
    )) {
        if (Test-Path "$candidate\VC\Auxiliary\Build\vcvars64.bat") { return $candidate }
    }
    throw "could not locate a Visual Studio install with the C++ workload. Install via `winget install Microsoft.VisualStudio.2022.BuildTools` and add the 'Desktop development with C++' workload."
}

$VsInstallPath = Find-VsInstallPath
$Arch = if ($env:VS_ARCH) { $env:VS_ARCH } else { 'x64' }
$VcVarsName = switch ($Arch) {
    'x64'   { 'vcvars64.bat' }
    'x86'   { 'vcvars32.bat' }
    'arm64' { 'vcvarsarm64.bat' }
    default { "vcvars$Arch.bat" }
}
$VcVars = Join-Path $VsInstallPath "VC\Auxiliary\Build\$VcVarsName"
if (-not (Test-Path $VcVars)) {
    throw "$VcVarsName not found under $VsInstallPath. Install the 'Desktop development with C++' workload."
}

# Capture the env that vcvars writes into a cmd.exe child, then mirror it into
# the current PowerShell session before launching cargo.
$tempEnv = New-TemporaryFile
try {
    & cmd.exe /c "call `"$VcVars`" >nul 2>&1 && set" > $tempEnv
    Get-Content $tempEnv | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') {
            $name = $matches[1]
            $value = $matches[2]
            Set-Item -LiteralPath "Env:$name" -Value $value
        }
    }
}
finally {
    Remove-Item $tempEnv -ErrorAction SilentlyContinue
}

# Fix-up: vcvars sometimes ships only the UCRT lib path even when the full
# `um\<arch>` directory exists (happens right after a fresh SDK install
# before VS finalizes registration). Append the missing path so link.exe
# can find kernel32.lib / ws2_32.lib / etc.
$ArchDir = if ($Arch -eq 'x86') { 'x86' } elseif ($Arch -eq 'arm64') { 'arm64' } else { 'x64' }
$sdkRoot = "${env:ProgramFiles(x86)}\Windows Kits\10"
if (Test-Path "$sdkRoot\Lib") {
    $latestSdk = Get-ChildItem "$sdkRoot\Lib" -Directory |
        Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1
    if ($latestSdk) {
        $umLib = Join-Path $latestSdk.FullName "um\$ArchDir"
        $ucrtLib = Join-Path $latestSdk.FullName "ucrt\$ArchDir"
        foreach ($p in @($umLib, $ucrtLib)) {
            if ((Test-Path $p) -and ($env:LIB -notlike "*$p*")) {
                $env:LIB = "$($env:LIB);$p"
            }
        }
        $umInc   = Join-Path $latestSdk.FullName.Replace('\Lib\', '\Include\') 'um'
        $sharedInc = Join-Path $latestSdk.FullName.Replace('\Lib\', '\Include\') 'shared'
        $ucrtInc = Join-Path $latestSdk.FullName.Replace('\Lib\', '\Include\') 'ucrt'
        foreach ($p in @($umInc, $sharedInc, $ucrtInc)) {
            if ((Test-Path $p) -and ($env:INCLUDE -notlike "*$p*")) {
                $env:INCLUDE = "$($env:INCLUDE);$p"
            }
        }
    }
}

# Prepend Zig path so build.rs can find it on a clean dev box.
$ZigDir = "$env:LOCALAPPDATA\zig-x86_64-windows-0.15.2"
if (Test-Path "$ZigDir\zig.exe") { $env:PATH = "$ZigDir;$env:PATH" }

& cargo @CargoArgs
exit $LASTEXITCODE
