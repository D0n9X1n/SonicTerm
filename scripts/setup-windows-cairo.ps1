[CmdletBinding()]
param(
    [string]$Triplet = "x64-windows-static",
    [string]$HostTriplet = "x64-windows"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$runningOnWindows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)
if (-not $runningOnWindows) {
    throw "scripts\setup-windows-cairo.ps1 only supports Windows runners."
}

$vcpkgRoot = @(
    $env:VCPKG_ROOT,
    $env:VCPKG_INSTALLATION_ROOT,
    "C:\vcpkg"
) | Where-Object {
    $_ -and (Test-Path -LiteralPath (Join-Path -Path $_ -ChildPath "vcpkg.exe"))
} | Select-Object -First 1

if (-not $vcpkgRoot) {
    throw "Could not find vcpkg.exe. Set VCPKG_ROOT or VCPKG_INSTALLATION_ROOT."
}

$vcpkg = Join-Path -Path $vcpkgRoot -ChildPath "vcpkg.exe"
& $vcpkg install "cairo:$Triplet" "pkgconf:$HostTriplet"
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg failed to install cairo:$Triplet and pkgconf:$HostTriplet."
}

$targetRoot = Join-Path -Path $vcpkgRoot -ChildPath "installed\$Triplet"
$hostRoot = Join-Path -Path $vcpkgRoot -ChildPath "installed\$HostTriplet"
$pkgConfig = Join-Path -Path $hostRoot -ChildPath "tools\pkgconf\pkg-config.exe"
if (-not (Test-Path -LiteralPath $pkgConfig)) {
    $pkgConfig = Join-Path -Path $targetRoot -ChildPath "tools\pkgconf\pkg-config.exe"
}

$pkgConfigPath = Join-Path -Path $targetRoot -ChildPath "lib\pkgconfig"
$cairoPc = Join-Path -Path $pkgConfigPath -ChildPath "cairo.pc"

if (-not (Test-Path -LiteralPath $pkgConfig)) {
    throw "Could not find pkg-config.exe after vcpkg install."
}
if (-not (Test-Path -LiteralPath $cairoPc)) {
    throw "Could not find cairo.pc at $cairoPc."
}

$toolDir = Split-Path -Parent $pkgConfig
$exports = [ordered]@{
    PKG_CONFIG = $pkgConfig
    PKG_CONFIG_PATH = $pkgConfigPath
    SYSTEM_DEPS_CAIRO_LINK = "static"
}

foreach ($entry in $exports.GetEnumerator()) {
    [System.Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    if ($env:GITHUB_ENV) {
        Add-Content -LiteralPath $env:GITHUB_ENV -Value "$($entry.Key)=$($entry.Value)"
    }
}

$env:PATH = "$toolDir;$env:PATH"
if ($env:GITHUB_PATH) {
    Add-Content -LiteralPath $env:GITHUB_PATH -Value $toolDir
}

Write-Host "Configured cairo:$Triplet for cairo-sys-rs via $pkgConfig"
