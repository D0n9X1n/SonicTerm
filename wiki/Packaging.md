# Packaging / 打包

How to build distributable macOS, Windows, and Linux packages locally, and what
the release workflow does differently.

如何在本地构建可分发的 macOS、Windows 与 Linux 安装包，以及发布工作流的不同之处。

Packaging executables live with the other first-party entry points in
`scripts/`. Pushing a `v*` tag runs the release workflow and publishes assets;
local packaging commands only create files under `dist/`.

Related: [Development and Release](Development-and-Release) · [Architecture Internals](Architecture-Internals)

## English

### macOS

The release workflow builds separate Apple Silicon and Intel binaries, then runs
`scripts/bake-icons.sh` and `scripts/make-macos-dmg.sh` from the repository
root. The packaging script assembles `SonicTerm.app`, copies the runtime assets
and bundled fonts, applies an ad-hoc signature, and creates the
architecture-specific DMG in `dist/`.

For a local package, install the build and packaging tools, build the native
release binary, and use a suffix that identifies the host architecture:

```bash
brew install cairo pkg-config create-dmg imagemagick
cargo build --release -p sonicterm-mac
bash scripts/bake-icons.sh

case "$(uname -m)" in
  arm64)  artifact_suffix=mac-aarch64 ;;
  x86_64) artifact_suffix=mac-x86_64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

version="$(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
bash scripts/make-macos-dmg.sh \
  target/release/sonicterm-mac \
  "$version" \
  "$artifact_suffix"
```

The generated `Info.plist` also declares `.sh`, `.command`, and `.tool`
shell-document types at `Alternate` handler rank. This makes the installed app
eligible for Finder's **Open With** without changing the user's current
association.

The app bundle is ad-hoc signed for internal consistency, but it is **not**
signed with an Apple Developer ID and **not** notarized. A downloaded build can
therefore show the normal unidentified-developer warning; use Finder's **Open**
context-menu action if macOS blocks the first launch.

### Windows

The MSI is built by `cargo wix` from
`crates/sonicterm-windows/wix/main.wxs`.

Install Rust's MSVC target, vcpkg, cargo-wix, and WiX Toolset 3. Make
`vcpkg.exe` reachable through `VCPKG_ROOT`, `VCPKG_INSTALLATION_ROOT`, or
`C:\vcpkg` before running the setup script:

```powershell
rustup target add x86_64-pc-windows-msvc
cargo install cargo-wix --locked
choco install wixtoolset --no-progress -y
```

Restart the shell after installing WiX if its `bin` directory is not yet on
`PATH`. Then run the same package sequence as the release workflow, from the
repository root:

```powershell
. .\scripts\setup-windows-cairo.ps1
cargo build --release --target x86_64-pc-windows-msvc -p sonicterm-windows
New-Item -ItemType Directory -Force -Path dist | Out-Null
Push-Location .\crates\sonicterm-windows
cargo wix --package sonicterm-windows --no-build --nocapture --output ..\..\dist\
Pop-Location
```

`setup-windows-cairo.ps1` exports `PKG_CONFIG` and `PKG_CONFIG_PATH` into the
**current process**, so it must be sourced in the same shell that runs the
build. A separate shell will fail in `cairo-sys-rs` with "The pkg-config command
could not be found."

The MSI registers application-specific ProgIDs, Default Apps capabilities, and
per-extension `OpenWithProgids` entries for `.ps1`, `.cmd`, `.bat`, and `.sh`.
It does not set extension defaults or `UserChoice`. Install and uninstall each
run the installed executable with `--refresh-shell-associations` after their
registry mutation; that mode only broadcasts `SHCNE_ASSOCCHANGED` and opens no
window. Uninstall removes SonicTerm's own values while leaving other handlers
and user choices intact.

`light.exe` may warn `LGHT1105: Validation could not run due to system policy`
when the shell is not elevated. ICE validation is skipped; the MSI contents and
installability are unaffected.

### Linux

Linux packages target x86_64 and a glibc 2.35 baseline. The release job builds
inside `ubuntu:22.04`, then creates both artifacts from one staged payload:

- `SonicTerm-<tag>-linux-x86_64.tar.gz` — relocatable `sonicterm` plus an
  adjacent `assets/` tree;
- `SonicTerm-<tag>-linux-x86_64.deb` — `/usr/bin/sonicterm`, FHS assets,
  desktop entry, AppStream metadata, hicolor icon, licenses, and README.

On an x86_64 Linux host with the native build and Debian package tools installed:

```bash
cargo build --release -p sonicterm-linux
tag="v$(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); m=set(d["workspace_members"]); v={p["version"] for p in d["packages"] if p["id"] in m}; assert len(v)==1; print(v.pop())')"
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)" \
  bash scripts/make-linux-packages.sh target/release/sonicterm "$tag" dist
bash scripts/test-linux-packages.sh \
  "dist/SonicTerm-${tag}-linux-x86_64.tar.gz" \
  "dist/SonicTerm-${tag}-linux-x86_64.deb"
```

The builder checks ELF architecture and glibc symbol versions, derives Debian
`Depends` with `dpkg-shlibdeps`, normalizes timestamps and ownership, and verifies
all four Rec Mono faces plus themes, keymaps, icons, and i18n. The Debian package
uses `com.d0n9x1n.SonicTerm` consistently for its desktop ID, Wayland application
ID, X11 class, AppStream component, and hicolor icon.

CI validates both package layouts, then runs each one under X11/Xvfb and headless
Wayland/Weston with Vulkan forced to Mesa lavapipe. A smoke passes only after a
window and GPU surface exist, `/bin/sh` starts in a PTY, a non-literal marker
round-trips into the grid, and a later frame reaches native presentation.

### Releases

The release workflow performs these steps automatically when a `v*` tag is
pushed. Each package job registers its files in typed asset fragments. The
publish job revalidates their hashes and required platform/architecture/kind
tuples, then creates `release-assets.json`, deterministic `SHA256SUMS.txt`, and an
exact upload-path list. Missing, altered, duplicate, or unregistered release
files block publication. Local packaging only writes files under `dist/`.
Pushing the tag is a separate, owner-approved action — running a packaging
script locally does not publish anything.

## 中文

### macOS

发布工作流会分别构建 Apple Silicon 与 Intel 两个二进制，然后在仓库根目录运行
`scripts/bake-icons.sh` 与 `scripts/make-macos-dmg.sh`。打包脚本会组装
`SonicTerm.app`、复制运行时资源与内置字体、施加 ad-hoc 签名，
并在 `dist/` 中生成对应架构的 DMG。

如需本地打包，请先安装构建与打包工具，构建原生 release 二进制，
并使用能标识主机架构的后缀：

```bash
brew install cairo pkg-config create-dmg imagemagick
cargo build --release -p sonicterm-mac
bash scripts/bake-icons.sh

case "$(uname -m)" in
  arm64)  artifact_suffix=mac-aarch64 ;;
  x86_64) artifact_suffix=mac-x86_64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

version="$(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
bash scripts/make-macos-dmg.sh \
  target/release/sonicterm-mac \
  "$version" \
  "$artifact_suffix"
```

生成的 `Info.plist` 还以 `Alternate` handler rank 声明 `.sh`、`.command` 和
`.tool` shell document type，使安装后的应用出现在 Finder **打开方式**中，但不会修改用户当前关联。

该 app bundle 会施加 ad-hoc 签名以保证内部一致性，但**没有**使用
Apple Developer ID 签名，也**没有**经过公证（notarize）。因此下载得到的构建
可能会出现常见的「来自身份不明的开发者」提示；若 macOS 阻止首次启动，
请使用 Finder 右键菜单中的**打开**。

### Windows

MSI 由 `cargo wix` 基于 `crates/sonicterm-windows/wix/main.wxs` 构建。

请安装 Rust 的 MSVC 目标、vcpkg、cargo-wix 以及 WiX Toolset 3。
在运行安装脚本之前，请确保可通过 `VCPKG_ROOT`、`VCPKG_INSTALLATION_ROOT`
或 `C:\vcpkg` 找到 `vcpkg.exe`：

```powershell
rustup target add x86_64-pc-windows-msvc
cargo install cargo-wix --locked
choco install wixtoolset --no-progress -y
```

如果安装 WiX 后其 `bin` 目录尚未加入 `PATH`，请重启终端。
随后在仓库根目录运行与发布工作流相同的打包序列：

```powershell
. .\scripts\setup-windows-cairo.ps1
cargo build --release --target x86_64-pc-windows-msvc -p sonicterm-windows
New-Item -ItemType Directory -Force -Path dist | Out-Null
Push-Location .\crates\sonicterm-windows
cargo wix --package sonicterm-windows --no-build --nocapture --output ..\..\dist\
Pop-Location
```

`setup-windows-cairo.ps1` 会把 `PKG_CONFIG` 与 `PKG_CONFIG_PATH` 导出到
**当前进程**，因此它必须在运行构建的同一个 shell 中被 source。
换一个 shell 会导致 `cairo-sys-rs` 报错：
「The pkg-config command could not be found.」

MSI 为 `.ps1`、`.cmd`、`.bat`、`.sh` 注册应用专属 ProgID、Default Apps
capabilities 和各扩展名的 `OpenWithProgids`，不会设置扩展名默认值或 `UserChoice`。
安装与卸载会在 registry 修改完成后，以 `--refresh-shell-associations` 调用已安装 executable；
该模式只广播 `SHCNE_ASSOCCHANGED`，不会打开窗口。卸载仅移除 SonicTerm 自己的值，
保留其它 handler 和用户选择。

当 shell 未以管理员权限运行时，`light.exe` 可能会警告
`LGHT1105: Validation could not run due to system policy`。
这只是跳过了 ICE 验证，不影响 MSI 的内容与可安装性。

### Linux

Linux 安装包面向 x86_64，并以 glibc 2.35 为基线。发布 job 在
`ubuntu:22.04` 中构建，再从同一个 staged payload 生成两个 artifact：

- `SonicTerm-<tag>-linux-x86_64.tar.gz` —— 可重定位的 `sonicterm` 与相邻
  `assets/` 目录；
- `SonicTerm-<tag>-linux-x86_64.deb` —— `/usr/bin/sonicterm`、FHS 资产、
  desktop entry、AppStream metadata、hicolor icon、license 与 README。

在已安装原生构建工具和 Debian 打包工具的 x86_64 Linux 主机上运行：

```bash
cargo build --release -p sonicterm-linux
tag="v$(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); m=set(d["workspace_members"]); v={p["version"] for p in d["packages"] if p["id"] in m}; assert len(v)==1; print(v.pop())')"
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)" \
  bash scripts/make-linux-packages.sh target/release/sonicterm "$tag" dist
bash scripts/test-linux-packages.sh \
  "dist/SonicTerm-${tag}-linux-x86_64.tar.gz" \
  "dist/SonicTerm-${tag}-linux-x86_64.deb"
```

builder 会检查 ELF 架构与 glibc symbol version，用 `dpkg-shlibdeps` 推导
Debian `Depends`，规范化 timestamp 与 owner，并验证四个 Rec Mono 字体、theme、
keymap、icon 与 i18n。Debian package 的 desktop ID、Wayland application ID、
X11 class、AppStream component 与 hicolor icon 都使用
`com.d0n9x1n.SonicTerm`。

CI 会验证两种 package layout，再让每一种分别在 X11/Xvfb 与 headless
Wayland/Weston 下运行，并强制 Vulkan 使用 Mesa lavapipe。只有原生窗口与 GPU
surface 已创建、`/bin/sh` 已在 PTY 中启动、非 literal marker 已往返进入 grid，
且随后一帧完成原生呈现，smoke 才会通过。

### 发布

推送 `v*` 标签时，发布工作流会自动执行上述步骤。每个平台打包 job 会把文件登记到
类型化 asset fragment；publish job 会重新验证 hash 与必需的
platform/architecture/kind tuple，再生成 `release-assets.json`、确定性的
`SHA256SUMS.txt` 和精确 upload-path list。缺失、被修改、重复或未登记的 release
文件都会阻止发布。本地打包只会在 `dist/` 下写入文件。推送标签是一个独立的、
需所有者批准的动作——在本地运行打包脚本不会发布任何东西。
