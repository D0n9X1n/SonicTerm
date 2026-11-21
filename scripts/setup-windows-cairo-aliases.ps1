# Give every library a static link names with a bare `-l<name>` a `<name>.lib` the MSVC linker can find on LIB.
#
# The names do not always match the files vcpkg installs: zlib 1.4 installs `libzs.lib` and its pkg-config file
# says `-llibzs`, but the Rust side strips the `lib` prefix and asks the linker for `zs.lib`. Each name, with and
# without a `lib` prefix, that the library directory lacks gets a copy of whichever spelling it has, in an alias
# directory this function empties and rebuilds on every call, so a copy never outlives a vcpkg upgrade. The caller
# puts the library directory before the alias directory on LIB, so a real library always wins over a copy. A
# library none of whose spellings exist throws, naming it; Windows system libraries come from the SDK.

Set-StrictMode -Version Latest

function Update-CairoLinkAliases {
    param(
        [Parameter(Mandatory)] [string]$LinkFlags,
        [Parameter(Mandatory)] [string]$LibraryDir,
        [Parameter(Mandatory)] [string]$AliasDir
    )
    if (Test-Path -LiteralPath $AliasDir) {
        Remove-Item -LiteralPath $AliasDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $AliasDir | Out-Null
    $systemLibs = @("gdi32", "msimg32", "user32", "kernel32", "advapi32", "ole32", "opengl32", "dwrite", "d2d1",
                    "windowscodecs", "bcrypt", "ws2_32", "shell32", "uuid", "comdlg32")
    # `-clike` is case-sensitive, so a `-L` search directory is not read as a library.
    foreach ($flag in ($LinkFlags -split '\s+' | Where-Object { $_ -clike '-l*' })) {
        $name = $flag.Substring(2)
        if ($name -in $systemLibs) {
            continue
        }
        $stem = if ($name.StartsWith("lib")) { $name.Substring(3) } else { $name }
        $spellings = @("$name.lib", "lib$stem.lib", "$stem.lib", "${stem}static.lib", "${stem}MT.lib") |
            Select-Object -Unique
        $existing = $spellings | ForEach-Object { Join-Path -Path $LibraryDir -ChildPath $_ } |
            Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $existing) {
            throw "the static link asks for $name, but $LibraryDir has none of: $($spellings -join ', ')."
        }
        foreach ($wanted in @("$name.lib", "$stem.lib") | Select-Object -Unique) {
            if (-not (Test-Path -LiteralPath (Join-Path -Path $LibraryDir -ChildPath $wanted))) {
                Copy-Item -LiteralPath $existing -Destination (Join-Path -Path $AliasDir -ChildPath $wanted)
                Write-Host "linked $wanted from $(Split-Path -Leaf $existing)"
            }
        }
    }
}
