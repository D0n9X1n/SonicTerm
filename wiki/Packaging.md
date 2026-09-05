# Packaging / 打包

## English

This page owns local packaging commands and installed/portable layouts. Running
these commands writes under `dist/`; it does not publish a release. The complete
tag, CI, asset-validation, and publication flow belongs on
[Development and Release](Development-and-Release). Native behavior inside each
package belongs on [Platform Integration](Platform-Integration).

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

## macOS package

### Requirements and command

Build on the target macOS architecture. The current bundle declares macOS 14.0
as its minimum. Install Cairo/pkg-config for the Rust build and `create-dmg` plus
ImageMagick for packaging:

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
└── Resources/
    ├── assets/{fonts,themes,keymaps,icons,i18n}/
    ├── Fonts/*.ttf
    └── sonic.icns
```

`Info.plist` records the supplied version, bundle id
`com.d0n9x1n.sonicterm`, `ATSApplicationFontsPath=Fonts`, and alternate handlers
for `public.shell-script` and `com.apple.terminal.shell-script`. The script
verifies all four Rec Mono faces.

After assembling all resources, the script applies and verifies an ad-hoc
signature. It does not use an Apple Developer ID and does not notarize. A
downloaded package can therefore show the standard unidentified-developer
warning; Finder's **Open** context-menu action allows the first launch.

CI and Release run each architecture's just-built `sonicterm-mac
--runtime-smoke` before its binary can enter DMG packaging. The bounded wrapper
uses separate scratch config/log roots, preserves `HOME`, removes inherited
`NO_COLOR`, and requires native window, renderer/device, live-grid PTY marker,
later presentation, and the complete default warm-renderer lifecycle.

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
workflow version, both language halves here, and the consistency test together;
then run the mutation tests and validate a newly built MSI before merging. Do not
float a tool first and document the selected version afterwards.

### Installed layout and registration

`cargo wix` consumes `crates/sonicterm-windows/wix/main.wxs`. The per-machine
MSI installs this core layout under `Program Files\SonicTerm`:

```text
SonicTerm/
├── sonicterm-windows.exe
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
└── README.md
```

The Debian package is `SonicTerm-<tag>-linux-x86_64.deb` and installs:

```text
/usr/bin/sonicterm
/usr/share/sonicterm/assets/{fonts,themes,keymaps,icons,i18n}/
/usr/share/applications/com.d0n9x1n.SonicTerm.desktop
/usr/share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
/usr/share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
/usr/share/doc/sonicterm/{copyright,LICENSE-Rec-Mono-OFL-1.1,README.md}
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
Wayland/Weston. Each layout is launched through `native-smoke-runner.py`, so its
45-second bound kills the complete process group, removes inherited `NO_COLOR`,
and preserves failure logs. `--runtime-smoke` must create a native window and
renderer/device, observe a `/bin/sh` marker in the live grid, present a later
frame, and complete the default warm-renderer lifecycle. The script refuses to
replace an existing SonicTerm Debian installation.

## Release handoff

A pushed tag matching `v[0-9]+.[0-9]+.[0-9]+*` starts the release workflow.
Its version validator then requires a supported semantic-version tag matching
all workspace packages. The workflow builds two macOS DMGs, one Windows MSI,
and the Linux Debian and tar packages. Package jobs register typed
asset fragments; publication accepts only the validated asset set and also
uploads `release-assets.json` and `SHA256SUMS.txt`. See
[Development and Release](Development-and-Release) for the blocking graph and
verification steps.

## 中文

本页负责本地打包命令和安装/便携布局。运行这些命令只会在 `dist/` 下生成文件，不会发布
release。完整的 tag、CI、资产校验与发布流程见[开发与发布](Development-and-Release)；
各安装包中的原生行为见[平台集成](Platform-Integration)。

## 版本与输出边界

根 `Cargo.toml [workspace.package].version` 是所有第一方 crate 的版本来源。本地命令需要
版本时，应通过 Cargo metadata 读取所有 workspace member，并确认只有一个共同版本：

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

Release tag 会增加 `v` 前缀。`scripts/prepare-release-assets.py check-version` 会拒绝
不能匹配每个 workspace package 的 tag。第一方打包可执行脚本都直接位于 `scripts/`。

## macOS 安装包

### 要求与命令

请在目标 macOS 架构上构建。当前 bundle 声明最低 macOS 14.0。Rust 构建需要
Cairo/pkg-config，打包需要 `create-dmg` 和 ImageMagick：

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

输出为 `dist/SonicTerm-<version>-<suffix>.dmg`。脚本优先使用 `create-dmg`，失败时回退到
`hdiutil`。

### Bundle 布局与信任

DMG 内包含 `SonicTerm.app`：

```text
SonicTerm.app/Contents/
├── MacOS/sonicterm-mac
├── Info.plist
└── Resources/
    ├── assets/{fonts,themes,keymaps,icons,i18n}/
    ├── Fonts/*.ttf
    └── sonic.icns
```

`Info.plist` 写入传入的版本、bundle id `com.d0n9x1n.sonicterm`、
`ATSApplicationFontsPath=Fonts`，并为 `public.shell-script` 和
`com.apple.terminal.shell-script` 声明 alternate handler。脚本会检查四个 Rec Mono 字体。

所有资源组装完成后，脚本会施加并校验 ad-hoc 签名。它不使用 Apple Developer ID，也不
做 notarize。下载的安装包可能显示标准的“无法验证开发者”提示；首次启动可使用 Finder
右键菜单中的**打开**。

CI 与 Release 会在二进制进入 DMG 打包前，对每个架构刚构建的
`sonicterm-mac --runtime-smoke` 运行必需原生 smoke。有界 wrapper 使用分开的临时
config/log 根目录，保留 `HOME`，移除继承的 `NO_COLOR`，并要求原生窗口、渲染器/设备、
实时 grid 中的 PTY marker、之后的呈现和完整默认预热渲染器生命周期。

## Windows 安装包

### 要求与命令

请使用 Windows x64 主机，并安装 MSVC target、vcpkg、`cargo-wix` 0.3.9 和 WiX
Toolset 3.14.1.20250415。CI 逻辑覆盖率使用 `cargo-llvm-cov` 0.9.0。
`scripts/setup-windows-cairo.ps1` 会通过 `VCPKG_ROOT`、
`VCPKG_INSTALLATION_ROOT` 或 `C:\vcpkg` 查找 `vcpkg.exe`，再安装静态 Cairo 和 pkgconf。

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

必须在执行 build 的同一个 PowerShell 进程中 dot-source Cairo 脚本。它会为当前进程设置
`PKG_CONFIG`、`PKG_CONFIG_PATH` 和 `SYSTEM_DEPS_CAIRO_LINK=static`；换一个 shell
就会丢失。如果刚安装 WiX，请重启 shell 或把其 `bin` 目录加入 `PATH`。

非管理员 shell 可能输出 `LGHT1105: Validation could not run due to system policy`。
这表示没有执行 ICE validation，本身不改变 MSI 内容。独立 COM 验证器仍会检查 MSI 的
Property、Component、Feature、FeatureComponents 和 SummaryInformation 数据。它要求
ProductVersion 等于数字 SemVer 核心、UpgradeCode 稳定、ProductCode 非空、template 为
`x64;1033`，且 `Binaries` 精确引用十个 64 位 component。预发布/构建后缀仍属于 tag
来源证明，但不能进入 MSI ProductVersion。

CI 与 Release 会在 MSI artifact 继续流转前运行刚构建的
`sonicterm-windows.exe --runtime-smoke`。普通 CI 另行要求 GDI capability 探针给出唯一
`EXERCISED` verdict；`HOST_INCAPABLE` 只提供信息，不能通过 gate。运行 smoke 使用真实
ConPTY/`cmd.exe`、分开的临时 config/log 根目录，以及与 macOS/Linux 相同的窗口、渲染器、
marker、呈现和预热渲染器生命周期契约。

工具更新通过独立的 `tooling` pull request 完成。中央 workflow 版本、本文两个语言半区和
一致性测试必须一起修改；合并前运行 mutation 测试并验证新构建的 MSI。不要先浮动工具，
再事后记录碰巧选中的版本。

### 安装布局与注册

`cargo wix` 使用 `crates/sonicterm-windows/wix/main.wxs`。Per-machine MSI 的核心布局位于
`Program Files\SonicTerm`：

```text
SonicTerm/
├── sonicterm-windows.exe
└── assets/
    ├── themes/*.toml
    ├── keymaps/*.toml
    ├── fonts/*.ttf
    └── icons/exports/{sonic.ico,sonic.icns}
```

它创建开始菜单快捷方式，并把 `INSTALLDESKTOPSHORTCUT` property 的默认值设为 `1`。
它为 `.ps1`、`.cmd`、`.bat` 和 `.sh` 注册 SonicTerm ProgID、Default Apps capabilities
与 `OpenWithProgids`，并在安装
或卸载后广播 `SHCNE_ASSOCCHANGED`。它不会写扩展名默认值或 `UserChoice`；卸载只删除
SonicTerm 自己的值。MSI 未签名。

## Linux 安装包

### 要求与命令

Linux 安装包面向 x86_64，GLIBC symbol version 上限为 2.35。Release builder 使用
Ubuntu 22.04。本地完整打包需要 Rust/Cairo、Fontconfig、X11、Wayland 开发依赖，以及
`tar`、`gzip`、`dpkg-deb`、`dpkg-shlibdeps`、`readelf`、`file`、Perl 和 Python 3。

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

非 Linux 主机可用 `scripts/make-linux-packages.sh --stage-only` 组装共同 payload，
但不能生成或校验 ELF 安装包。

### 便携与 Debian 布局

两个 artifact 来自同一个规范化 staged payload。时间戳取自 `SOURCE_DATE_EPOCH`；tar
中的 owner/group 是数值形式的 root/root。

可重定位归档为 `SonicTerm-<tag>-linux-x86_64.tar.gz`：

```text
SonicTerm-<tag>-linux-x86_64/
├── sonicterm
├── assets/{fonts,themes,keymaps,icons,i18n}/
├── share/applications/com.d0n9x1n.SonicTerm.desktop
├── share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
├── share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
├── LICENSE
├── LICENSE-Rec-Mono-OFL-1.1
└── README.md
```

Debian package 为 `SonicTerm-<tag>-linux-x86_64.deb`，安装到：

```text
/usr/bin/sonicterm
/usr/share/sonicterm/assets/{fonts,themes,keymaps,icons,i18n}/
/usr/share/applications/com.d0n9x1n.SonicTerm.desktop
/usr/share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
/usr/share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
/usr/share/doc/sonicterm/{copyright,LICENSE-Rec-Mono-OFL-1.1,README.md}
```

Builder 会检查 x86_64 ELF，并拒绝高于 2.35 的 GLIBC requirement。`dpkg-shlibdeps`
推导已链接的 `Depends`；脚本还会加入 `libxkbcommon-x11-0`，因为 winit 在 X11 下动态
加载它。便携归档的主机在使用 X11 时必须提供 `libxkbcommon-x11.so.0`。打包脚本会验证
四个 Rec Mono 字体，以及主题、键位、图标、英文和简体中文 catalog。

### 安装包校验与运行证明

`scripts/test-linux-packages.sh` 会检查源码契约；传入路径时还会验证两种已构建布局。
CI 另行验证 desktop entry、AppStream metadata 和 Debian dependency field；`lintian`
结果在 CI 中只作提示。

`scripts/smoke-linux-packages.sh` 要求在临时 Linux container 中以 root 运行。它解压 tarball、
安装 Debian package、强制 Vulkan 使用 Mesa lavapipe，然后先在 X11/Xvfb、再在 headless
Wayland/Weston 上运行两种布局。每种布局都通过 `native-smoke-runner.py` 启动，因此 45 秒
期限会终止完整进程组、移除继承的 `NO_COLOR` 并保存失败日志。`--runtime-smoke` 必须创建
原生窗口与渲染器/设备，在实时 grid 中观察到 `/bin/sh` marker，呈现之后的一帧，并完成默认
预热渲染器生命周期。脚本会拒绝替换已有的 SonicTerm Debian 安装。

## 发布交接

推送匹配 `v[0-9]+.[0-9]+.[0-9]+*` 的 tag 会启动 release workflow。版本校验器随后要求
tag 是受支持的语义版本，并与所有 workspace package 一致。工作流会构建两个 macOS DMG、
一个 Windows MSI，以及 Linux Debian 与 tar 包。各 package job 会登记类型化 asset
fragment；发布只接受通过校验的资产集，并一同上传 `release-assets.json` 和
`SHA256SUMS.txt`。阻断关系和验证步骤见
[开发与发布](Development-and-Release)。
