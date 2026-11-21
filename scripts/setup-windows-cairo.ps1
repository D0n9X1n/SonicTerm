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

# When a binary cache directory is provided (CI), make sure it exists so vcpkg
# can populate/restore prebuilt packages instead of compiling Cairo from source.
if ($env:VCPKG_DEFAULT_BINARY_CACHE) {
    New-Item -ItemType Directory -Force -Path $env:VCPKG_DEFAULT_BINARY_CACHE | Out-Null
}

# Pass package arguments => classic mode. Force-disable manifest mode so a stray
# vcpkg.json anywhere in the working-directory chain can never flip this command
# into manifest mode (which rejects positional package args and would fail).
& $vcpkg install "cairo:$Triplet" "pkgconf:$HostTriplet" "--feature-flags=-manifests"
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg failed to install cairo:$Triplet and pkgconf:$HostTriplet."
}

$targetRoot = Join-Path -Path $vcpkgRoot -ChildPath "installed\$Triplet"
$hostRoot = Join-Path -Path $vcpkgRoot -ChildPath "installed\$HostTriplet"
$pkgConfigCandidates = @(
    (Join-Path -Path $hostRoot -ChildPath "tools\pkgconf\pkg-config.exe"),
    (Join-Path -Path $hostRoot -ChildPath "tools\pkgconf\pkgconf.exe"),
    (Join-Path -Path $targetRoot -ChildPath "tools\pkgconf\pkg-config.exe"),
    (Join-Path -Path $targetRoot -ChildPath "tools\pkgconf\pkgconf.exe")
)
$pkgConfig = $pkgConfigCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $pkgConfig) {
    $searchRoots = @(
        $hostRoot,
        $targetRoot,
        (Join-Path -Path $vcpkgRoot -ChildPath "downloads\tools")
    ) | Where-Object { Test-Path -LiteralPath $_ }
    $pkgConfig = Get-ChildItem -LiteralPath $searchRoots -Recurse -File -Include "pkg-config.exe", "pkgconf.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}

$pkgConfigPath = Join-Path -Path $targetRoot -ChildPath "lib\pkgconfig"
$cairoPc = Join-Path -Path $pkgConfigPath -ChildPath "cairo.pc"

if (-not (Test-Path -LiteralPath $pkgConfig)) {
    throw "Could not find pkg-config.exe or pkgconf.exe after vcpkg install."
}
if (-not (Test-Path -LiteralPath $cairoPc)) {
    throw "Could not find cairo.pc at $cairoPc."
}

# A static link names each dependency with a bare `-l<name>`, which MSVC's linker resolves as `<name>.lib` in a
# directory on LIB. The names do not always match the files vcpkg installs: zlib 1.4 installs `libzs.lib` and its
# pkg-config file says `-llibzs`, but the Rust side strips the `lib` prefix and asks the linker for `zs.lib`. So
# every name the static Cairo link lists, with and without a `lib` prefix, gets a `<name>.lib` in the target lib
# directory, copied from whichever spelling exists, and that directory goes on LIB. A library none of whose
# spellings exist fails here, named, instead of at link time; Windows system libraries come from the SDK.
$targetLib = Join-Path -Path $targetRoot -ChildPath "lib"
$env:PKG_CONFIG_PATH = $pkgConfigPath
$staticLibs = & $pkgConfig --libs --static cairo
if ($LASTEXITCODE -ne 0) {
    throw "pkg-config could not list cairo's static libraries."
}
Write-Host "cairo static link flags: $staticLibs"
$systemLibs = @("gdi32", "msimg32", "user32", "kernel32", "advapi32", "ole32", "opengl32", "dwrite", "d2d1",
                "windowscodecs", "bcrypt", "ws2_32", "shell32", "uuid", "comdlg32")
# `-clike` is case-sensitive, so the `-L` search directory is not read as a library.
foreach ($flag in ($staticLibs -split '\s+' | Where-Object { $_ -clike '-l*' })) {
    $name = $flag.Substring(2)
    if ($name -in $systemLibs) {
        continue
    }
    $stem = if ($name.StartsWith("lib")) { $name.Substring(3) } else { $name }
    $spellings = @("$name.lib", "lib$stem.lib", "$stem.lib", "${stem}static.lib", "${stem}MT.lib") |
        Select-Object -Unique
    $existing = $spellings | ForEach-Object { Join-Path -Path $targetLib -ChildPath $_ } |
        Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $existing) {
        throw "cairo's static link asks for $name, but $targetLib has none of: $($spellings -join ', ')."
    }
    foreach ($wanted in @("$name.lib", "$stem.lib") | Select-Object -Unique) {
        $destination = Join-Path -Path $targetLib -ChildPath $wanted
        if (-not (Test-Path -LiteralPath $destination)) {
            Copy-Item -LiteralPath $existing -Destination $destination
            Write-Host "linked $wanted from $(Split-Path -Leaf $existing)"
        }
    }
}

$toolDir = Split-Path -Parent $pkgConfig
$linkSearch = if ($env:LIB) { "$targetLib;$env:LIB" } else { $targetLib }
$exports = [ordered]@{
    LIB = $linkSearch
    SONICTERM_CAIRO_BIN_DIR = (Join-Path -Path $targetRoot -ChildPath "bin")
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

# Static Cairo needs no DLL today; a caller-selected dynamic triplet uses this same explicit runtime path.
$cairoBin = $exports.SONICTERM_CAIRO_BIN_DIR
$env:PATH = "$cairoBin;$toolDir;$env:PATH"
if ($env:GITHUB_PATH) {
    Add-Content -LiteralPath $env:GITHUB_PATH -Value $toolDir
    Add-Content -LiteralPath $env:GITHUB_PATH -Value $cairoBin
}

Write-Host "Configured cairo:$Triplet for cairo-sys-rs via $pkgConfig"
