# Packaging

[简体中文](Packaging-zh-CN)

Choose your host platform below to build a package under `dist/`. These commands
do not publish it. Release approval and publication are separate steps in
[Development and Release](Development-and-Release); native behavior is described
in [Platform Integration](Platform-Integration).

## Version and output boundary

The root `Cargo.toml [workspace.package].version` is the version source for every
first-party crate. When a local command needs the version, read all workspace
members from Cargo metadata and require one shared value:

```bash
version="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
versions = {p["version"] for p in metadata["packages"] if p["id"] in members}
assert len(versions) == 1, sorted(versions)
print(versions.pop())
')"
```

Release tags add the `v` prefix. `scripts/prepare-release-assets.py
check-version` rejects a tag that does not match every workspace package.
First-party packaging executables are direct children of `scripts/`.

Every package includes the pinned, statically linked winit dependency's
`crates/sonicterm-winit/LICENSE` as `LICENSE-winit-Apache-2.0`.

## macOS package

### Requirements and command

Build and package on the target macOS architecture, on the same host whose
Homebrew libraries linked the executable. The bundle's minimum macOS version is
the maximum of 14.0 and the deployment targets of its executable and every
bundled dylib. Official Apple Silicon packages support macOS 14+, and Intel
packages support macOS 15+; CI enforces those explicit ceilings and refuses newer
dependency floors. Check `LSMinimumSystemVersion` and
`Resources/native-libraries.json` for the artifact's actual requirement. Official
release packages are assembled on their architecture-specific CI runners. Install Cairo/pkg-config
for the Rust build and `create-dmg` plus ImageMagick for packaging:

```bash
brew install cairo pkg-config create-dmg imagemagick
cargo build --release -p sonicterm-mac
bash scripts/bake-icons.sh

version="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
versions = {p["version"] for p in metadata["packages"] if p["id"] in members}
assert len(versions) == 1, sorted(versions)
print(versions.pop())
')"
case "$(uname -m)" in
  arm64)  suffix=mac-aarch64 ;;
  x86_64) suffix=mac-x86_64 ;;
  *) printf 'unsupported architecture: %s\n' "$(uname -m)" >&2; exit 1 ;;
esac

bash scripts/make-macos-dmg.sh \
  target/release/sonicterm-mac \
  "$version" \
  "$suffix"
```

The output is `dist/SonicTerm-<version>-<suffix>.dmg`. The script uses
`create-dmg`, with `hdiutil` as a fallback.

### Bundle layout and trust

The DMG contains `SonicTerm.app`:

```text
SonicTerm.app/Contents/
├── MacOS/sonicterm-mac
├── Info.plist
├── Frameworks/*.dylib
└── Resources/
    ├── assets/{fonts,themes,keymaps,icons,i18n}/
    ├── licenses/
    ├── native-libraries.json
    └── sonic.icns
```

`Info.plist` records the supplied version, bundle id
`com.d0n9x1n.sonicterm`, `ATSApplicationFontsPath=assets/fonts`, and alternate handlers
for `public.shell-script` and `com.apple.terminal.shell-script`. All four unchanged
Rec Mono faces are stored once: the runtime and AppKit/CoreText share this path.

`scripts/macos-bundle.py` recursively collects the executable's non-system dylib
closure from installed Homebrew kegs, verifies architecture, rewrites loads to
bundle-relative paths, and removes build-host rpaths. Required licenses, receipts,
source/packaged hashes and resolved library versions accompany the bundle;
missing attribution or unresolved dependencies stop packaging. System libraries
remain supplied by macOS. Dylibs are individually signed before the final app
seal, and the verifier checks the resulting closure and manifest. This does not
change the source-pinned FreeType/HarfBuzz stack or require Homebrew on the user's
Mac.

After native dependency collection, the script adds `LICENSE-winit-Apache-2.0`
to `Contents/Resources/licenses`. After assembling all resources, it applies and
verifies an ad-hoc signature. It does not use an Apple Developer ID and does not
notarize. A downloaded package can therefore show the standard
unidentified-developer warning; Finder's **Open** context-menu action allows the first launch.

CI and Release run each architecture's just-built `sonicterm-mac
--runtime-smoke` before its binary can enter DMG packaging. The bounded wrapper
uses separate scratch config/log roots, preserves `HOME`, removes inherited
`NO_COLOR`, and requires native window, renderer/device, live-grid PTY marker,
later presentation, and the complete default warm-renderer lifecycle.

Both Apple Silicon and Intel CI lanes also build a DMG on their native host and
run `python3 scripts/test-macos-package.py --dmg <image> --state-dir <new-directory>`.
The validator installs a read-only-mounted image into a scratch path, checks
signatures and the dependency manifest, then exercises the app and Cairo drawing
with Homebrew filesystem reads denied for those child processes. A native
LaunchServices probe verifies all four font URLs point into the bundle, rather
than accidentally finding installed copies. It checks basic Cairo gradient
pixels; that probe is not an exhaustive COLR glyph test. Only the exact `open -W`
exit-before-wait `kevent`/“No such process” error is accepted when the fresh report
already ends with one complete passing verdict; other launch failures and timeouts
still fail validation. A same-binary UDZO pair
with single versus duplicated fonts reports actual compressed savings separately
from logical file bytes and added Cairo-library bytes. No host libraries are
moved or renamed. Logs and `package-evidence.json` retain the checks and sizes.
The validator's commands share a 420-second deadline from its start, with time
held back for the final unmount. Only a transient `Resource busy` failure from
`hdiutil create` is retried, at most twice.

`SONICTERM_PACKAGE_DIR` chooses an isolated output directory. The optional fourth
argument `--bundle-only` assembles and verifies the app without creating a DMG.
`SONICTERM_MAX_MACOS_MINIMUM` defaults to `14.0`; use `15.0` for the official Intel
policy. A local developer may explicitly choose a higher ceiling for experiments
with newer Homebrew bottles, but that artifact is not a supported release. Pass
the same ceiling to the validator with `--max-minimum-macos`; it never rewrites
Mach-O deployment targets to pretend compatibility.

## Windows package

### Requirements and command

Use a Windows x64 host with the MSVC target, vcpkg, `cargo-wix` 0.3.9, and WiX
Toolset 3.14.1.20250415. CI logic coverage uses `cargo-llvm-cov` 0.9.0.
`scripts/setup-windows-cairo.ps1` looks for `vcpkg.exe` through `VCPKG_ROOT`,
`VCPKG_INSTALLATION_ROOT`, or `C:\vcpkg` and installs static Cairo plus pkgconf.

```powershell
rustup target add x86_64-pc-windows-msvc
cargo install cargo-wix --version 0.3.9 --locked
choco install wixtoolset --version 3.14.1.20250415 --no-progress -y

. .\scripts\setup-windows-cairo.ps1
cargo build --release --target x86_64-pc-windows-msvc -p sonicterm-windows
$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object name -eq sonicterm-windows | Select-Object -ExpandProperty version
$numericVersion = ($version -split '[-+]')[0]
New-Item -ItemType Directory -Force -Path dist | Out-Null
Push-Location .\crates\sonicterm-windows
cargo wix --package sonicterm-windows --target x86_64-pc-windows-msvc `
    --install-version $numericVersion --no-build --nocapture --output ..\..\dist\
Pop-Location
$msi = Get-ChildItem .\dist\*.msi -ErrorAction Stop
.\scripts\validate-windows-msi.ps1 -MsiPath $msi.FullName -ExpectedVersion "v$version"
```

Dot-source the Cairo script in the same PowerShell process as the build. It sets
`PKG_CONFIG`, `PKG_CONFIG_PATH`, and `SYSTEM_DEPS_CAIRO_LINK=static` for that
process. Starting a different shell loses those values. If WiX was just
installed, restart the shell or add its `bin` directory to `PATH`.

An unelevated build may print `LGHT1105: Validation could not run due to system
policy`. That warning means ICE validation did not run; it does not by itself
change the MSI contents. The independent COM validator still checks the MSI
Property, Component, Feature, FeatureComponents, and SummaryInformation data. It
requires the numeric SemVer core as ProductVersion, stable UpgradeCode, nonempty
ProductCode, `x64;1033` template, and the exact ten 64-bit `Binaries` components.
Prerelease/build suffixes remain part of tag provenance but cannot enter MSI
ProductVersion.

CI and Release run the just-built `sonicterm-windows.exe --runtime-smoke` before
the MSI artifact can advance. Normal CI separately requires the GDI capability
probe's unique `EXERCISED` verdict; `HOST_INCAPABLE` is informational only. The
runtime smoke uses real ConPTY/`cmd.exe`, separate scratch config/log roots, and
the same window, renderer, marker, presentation, and warm-renderer lifecycle
contract as macOS and Linux.

Tooling updates use a dedicated `tooling` pull request. Change the central
workflow version, both Packaging language files, and the consistency test together;
then run the mutation tests and validate a newly built MSI before merging. Do not
float a tool first and document the selected version afterwards.

### Installed layout and registration

`cargo wix` consumes `crates/sonicterm-windows/wix/main.wxs`. The per-machine
MSI installs this core layout under `Program Files\SonicTerm`:

```text
SonicTerm/
├── sonicterm-windows.exe
├── LICENSE-winit-Apache-2.0
└── assets/
    ├── themes/*.toml
    ├── keymaps/*.toml
    ├── fonts/*.ttf
    └── icons/exports/{sonic.ico,sonic.icns}
```

It creates a Start-menu shortcut and sets the `INSTALLDESKTOPSHORTCUT` property
to `1` by default. It registers SonicTerm ProgIDs, Default Apps capabilities, and
`OpenWithProgids` for `.ps1`, `.cmd`, `.bat`, and `.sh`, then broadcasts
`SHCNE_ASSOCCHANGED` after install or uninstall. It never writes an extension
default or `UserChoice`, and uninstall removes only SonicTerm's values. The MSI
is unsigned.

## Linux packages

### Requirements and command

Linux packages target x86_64 with a glibc 2.35 maximum symbol baseline. The
release builder is Ubuntu 22.04. A local full build needs the native Rust/Cairo,
Fontconfig, X11, and Wayland development dependencies plus `tar`, `gzip`,
`dpkg-deb`, `dpkg-shlibdeps`, `readelf`, `file`, Perl, and Python 3.

```bash
cargo build --release -p sonicterm-linux
version="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
versions = {p["version"] for p in metadata["packages"] if p["id"] in members}
assert len(versions) == 1, sorted(versions)
print(versions.pop())
')"
tag="v${version}"
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)" \
  bash scripts/make-linux-packages.sh target/release/sonicterm "$tag" dist
bash scripts/test-linux-packages.sh \
  "dist/SonicTerm-${tag}-linux-x86_64.tar.gz" \
  "dist/SonicTerm-${tag}-linux-x86_64.deb"
```

On a non-Linux host, `scripts/make-linux-packages.sh --stage-only` can assemble
the common payload, but it cannot create or validate the ELF packages.

### Portable and Debian layouts

Both artifacts come from one normalized staged payload. Timestamps use
`SOURCE_DATE_EPOCH`; tar ownership is root/root with numeric ids.

The relocatable archive is
`SonicTerm-<tag>-linux-x86_64.tar.gz`:

```text
SonicTerm-<tag>-linux-x86_64/
├── sonicterm
├── assets/{fonts,themes,keymaps,icons,i18n}/
├── share/applications/com.d0n9x1n.SonicTerm.desktop
├── share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
├── share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
├── LICENSE
├── LICENSE-Rec-Mono-OFL-1.1
├── LICENSE-winit-Apache-2.0
└── README.md
```

The Debian package is `SonicTerm-<tag>-linux-x86_64.deb` and installs:

```text
/usr/bin/sonicterm
/usr/share/sonicterm/assets/{fonts,themes,keymaps,icons,i18n}/
/usr/share/applications/com.d0n9x1n.SonicTerm.desktop
/usr/share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
/usr/share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
/usr/share/doc/sonicterm/{copyright,LICENSE-Rec-Mono-OFL-1.1,LICENSE-winit-Apache-2.0,README.md}
```

The builder checks x86_64 ELF identity and rejects GLIBC requirements newer
than 2.35. `dpkg-shlibdeps` derives linked `Depends`; the script also adds
`libxkbcommon-x11-0` because winit loads it dynamically for X11. The portable
archive's host must provide `libxkbcommon-x11.so.0` when using X11. The package
script verifies all four Rec Mono faces plus themes, keymaps, icons, and English
and Simplified Chinese catalogs.

### Package validation and runtime proof

`scripts/test-linux-packages.sh` checks the source contract and, when paths are
provided, both built layouts. CI additionally validates the desktop entry,
AppStream metadata, and Debian dependency field. `lintian` findings are advisory
in CI.

`scripts/smoke-linux-packages.sh` requires root in an ephemeral Linux container.
It extracts the tarball, installs the Debian package, forces Vulkan through Mesa
lavapipe, and runs both layouts first on X11/Xvfb and then on headless
Wayland/Weston. Its optional third argument is `default`, `frame-validation`, or
`device-recovery`; an omitted argument selects `default`, while empty or unknown names fail before
package installation or display startup. Each layout passes the scenario before
`--` to `native-smoke-runner.py`, with a distinct scenario/display/package state
root and log. The wrapper removes inherited `NO_COLOR`, preserves `HOME`, and
bounds each child to 45 seconds. On POSIX it kills that process group; descendants
that leave it are outside that bound.

CI and Release run the default, frame-validation and device-recovery matrices in
separate five-minute steps. Default smoke requires a native window and renderer/device,
a `/bin/sh` marker in the live grid, later presentation, the warm-renderer
lifecycle, and isolated/retained-resource/device-loss fault checks. Frame validation
starts a fresh process, injects a persistent fault after initial presentation, and
requires stopped presentation plus a newly executed PTY marker. Device recovery
starts another process with two live windows and one warm renderer, destroys their
shared device, and requires one rebuild, new marker-bearing presentations in both
windows on the replacement generation, and release back to the original renderer
count. The original PTY identities must survive, and an old-generation callback
must not trigger another rebuild. Fault-containment failures exit `17`, device-loss
failures `18`, recovery failures `19`, and otherwise successful smoke with unsettled
PTY teardown `20`; earlier failures take precedence.
The first failed case stops its matrix and preserves its exit code. Failure logs
use `sonicterm-<scenario>-<display>-<package>-smoke.log`, matching the upload glob.
The script refuses to replace an existing SonicTerm Debian installation.

## Release handoff

Local packages are not published automatically. The tag-driven release validates
all workspace versions and typed package fragments before uploading the five
packages, `release-assets.json`, and `SHA256SUMS.txt`. Exact tag rules and release
steps are in [Development and Release](Development-and-Release).
