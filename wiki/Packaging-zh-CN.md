# 打包

[English](Packaging)

按主机平台选择下方命令，在 `dist/` 生成本地包，不会自动发布。发布授权与流程见
[开发与发布](Development-and-Release-zh-CN)，包内原生行为见[平台集成](Platform-Integration-zh-CN)。

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

每种安装包都将固定源码并静态链接的 winit 依赖的 `crates/sonicterm-winit/LICENSE`
以 `LICENSE-winit-Apache-2.0` 文件名附带。

## macOS 安装包

### 要求与命令

请在目标 macOS 架构上构建和打包，并使用链接该程序时提供 Homebrew 库的同一主机。
Bundle 的最低 macOS 版本取 14.0、可执行文件及全部内嵌 dylib 的部署目标中的最大值。
正式 Apple Silicon 安装包支持 macOS 14+，Intel 安装包支持 macOS 15+；CI 强制检查
这些显式上限，依赖要求更新系统时打包失败。请检查 `LSMinimumSystemVersion` 和
`Resources/native-libraries.json` 获取产物的实际要求。正式发布包由对应架构的 CI runner 组装。Rust 构建需要 Cairo/pkg-config，打包需要
`create-dmg` 和 ImageMagick：

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

输出为 `dist/SonicTerm-<version>-<suffix>.dmg`。脚本的 `macos-bundle.py dmg` 镜像阶段在
`create-dmg` 可用时优先使用它；只有命令正常以非零状态退出且进程组没有残留成员时，才回退到
`hdiutil`。未安装 `create-dmg` 时直接使用 `hdiutil`；启动失败、超时、信号终止、中断、无法
取得退出状态，以及残留进程数量为正数或未知，都会使打包失败，而不是触发回退。

镜像命令和重试等待共享 300 秒的单调时钟预算，其中为进程组检查、回收和输出排空预留 60 秒。
每条命令最多运行 120 秒，且只有预留时间之前还剩至少 30 秒时才会启动。只有已正常失败且进程组
清空的命令输出精确诊断 `hdiutil: create failed - Resource busy`，才允许重试：最多尝试三次，
间隔 10 秒，并且等待与下一次尝试的最短预算必须都能容纳。延迟唤醒后会重新检查预算。这是受监督
的命令预算，不是操作系统调度、文件系统或整个 bundle 组装阶段的硬截止时间。

每次尝试前都会移除上一次尝试留下的私有不完整镜像。只有成功且进程组已清空的命令生成的非空普通
`.dmg` 文件（不能是符号链接），才会原子替换目标；失败时保留原有目标。原始命令日志及
`result.json` 保留在新建的同级 `<output>.creation-*` 目录中。CI 与 Release 的失败上传仅包含
这些日志和 JSON，不上传暂存镜像。监督器检查并终止命令所在的进程组；脱离该组的后代不在此保证
范围内。不会终止共享磁盘镜像服务，进程组为空也不证明没有镜像仍处于挂载状态。

### Bundle 布局与信任

DMG 内包含 `SonicTerm.app`：

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

`Info.plist` 写入传入的版本、bundle id `com.d0n9x1n.sonicterm`、
`ATSApplicationFontsPath=assets/fonts`，并为 `public.shell-script` 和
`com.apple.terminal.shell-script` 声明 alternate handler。四个未经修改的 Rec Mono 字体
仅保存一次，运行时和 AppKit/CoreText 共用同一路径。

`scripts/macos-bundle.py` 从已安装的 Homebrew keg 递归收集可执行文件依赖的非系统 dylib，
核对架构，将加载路径改为 bundle 内相对路径，并移除构建主机的 rpath。安装包附带必需的
许可证、安装收据、源文件/打包文件摘要和实际库版本；来源信息缺失或依赖无法解析会使
打包失败。系统库仍由 macOS 提供。各 dylib 先独立签名，再封装整个 app 的签名；验证器
检查最终依赖集合和清单。这不改变源码固定的 FreeType/HarfBuzz 栈，也不要求用户安装
Homebrew。

原生依赖收集完成后，脚本将 `LICENSE-winit-Apache-2.0` 加入
`Contents/Resources/licenses`。所有资源组装完成后，再施加并校验 ad-hoc 签名。
它不使用 Apple Developer ID，也不做 notarize。下载的安装包可能显示标准的
“无法验证开发者”提示；首次启动可使用 Finder
右键菜单中的**打开**。

CI 与 Release 会在二进制进入 DMG 打包前，对每个架构刚构建的
`sonicterm-mac --runtime-smoke` 运行必需原生 smoke。有界 wrapper 使用分开的临时
config/log 根目录，保留 `HOME`，移除继承的 `NO_COLOR`，并要求原生窗口、渲染器/设备、
实时 grid 中的 PTY marker、之后的呈现和完整默认预热渲染器生命周期。

Apple Silicon 与 Intel CI lane 还会在各自原生主机生成 DMG，然后运行
`python3 scripts/test-macos-package.py --dmg <image> --state-dir <new-directory>`。
验证器将只读挂载镜像中的 app 复制到临时安装路径，检查签名和依赖清单，再针对子进程
拒绝 Homebrew 文件读取，运行应用和 Cairo 绘制。原生 LaunchServices 探针核对四个字体
URL 均指向 bundle，避免误用系统中已安装的同名字体。它验证基本 Cairo 渐变像素，不是
完整的 COLR 字形测试。只有 `open -W` 在建立等待前进程已退出时产生的精确
`kevent`/“No such process” 错误，且本次新报告已以唯一、完整的通过判定结束，才会被接受；
其他启动失败和超时仍使验证失败。同一可执行文件分别使用单份/重复字体生成 UDZO 镜像，报告实际
压缩后节省量，并与逻辑文件大小和新增 Cairo 库大小区分。不会移动或重命名主机库。
日志及 `package-evidence.json` 保留检查和尺寸证据。
验证器的命令共享自启动起 420 秒的截止时间，并为最后的卸载预留时间；只有
`hdiutil create` 的临时性 `Resource busy` 失败会重试，最多两次。

`SONICTERM_PACKAGE_DIR` 可选择独立输出目录。第四个可选参数 `--bundle-only` 只组装和
验证 app，不生成 DMG。`SONICTERM_MAX_MACOS_MINIMUM` 默认为 `14.0`，正式 Intel 策略
使用 `15.0`。开发者可为使用较新 Homebrew bottle 的本地实验显式指定更高上限，但该产物
不是受支持的正式发布包。验证器通过 `--max-minimum-macos` 接收相同上限，不会修改 Mach-O
部署目标来伪装兼容性。

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

工具更新通过独立的 `tooling` pull request 完成。中央 workflow 版本、Packaging 的两个语言文件和
一致性测试必须一起修改；合并前运行 mutation 测试并验证新构建的 MSI。不要先浮动工具，
再事后记录碰巧选中的版本。

### 安装布局与注册

`cargo wix` 使用 `crates/sonicterm-windows/wix/main.wxs`。Per-machine MSI 的核心布局位于
`Program Files\SonicTerm`：

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
├── LICENSE-winit-Apache-2.0
└── README.md
```

Debian package 为 `SonicTerm-<tag>-linux-x86_64.deb`，安装到：

```text
/usr/bin/sonicterm
/usr/share/sonicterm/assets/{fonts,themes,keymaps,icons,i18n}/
/usr/share/applications/com.d0n9x1n.SonicTerm.desktop
/usr/share/metainfo/com.d0n9x1n.SonicTerm.metainfo.xml
/usr/share/icons/hicolor/256x256/apps/com.d0n9x1n.SonicTerm.png
/usr/share/doc/sonicterm/{copyright,LICENSE-Rec-Mono-OFL-1.1,LICENSE-winit-Apache-2.0,README.md}
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
Wayland/Weston 上运行两种布局。可选的第三个参数为 `default`、`frame-validation` 或
`device-recovery`；省略时选择 `default`，空值或未知名称会在安装包或启动显示服务前失败。
每种布局都在 `--` 之前把
场景传给 `native-smoke-runner.py`，并使用独立的场景/显示后端/包布局状态目录和日志。包装器
移除继承的 `NO_COLOR`、保留 `HOME`，并把每个子进程限制为 45 秒。在 POSIX 上它终止该
进程组；离开此组的后代不在这个期限的约束内。

CI 与 Release 用三个独立的五分钟步骤运行默认、frame-validation 和 device-recovery 矩阵。
默认冒烟要求原生窗口与渲染器/设备、实时 grid 中的 `/bin/sh` marker、后续呈现、预热渲染器
生命周期，以及隔离故障、保留资源故障和设备丢失检查。帧验证从新进程开始，在初次呈现后注入
持续故障，并要求呈现停止且新执行的 PTY marker 到达。设备恢复使用另一个进程，先建立两个
可见窗口和一个预热渲染器，再销毁它们的共享设备，要求只重建一次、两个窗口在替换代次上呈现
包含新 marker 的帧，并在释放后恢复原有渲染器计数。原 PTY 身份必须保留，旧代次回调不能触发
第二次重建。故障隔离失败返回 `17`，设备丢失失败返回 `18`，恢复失败返回 `19`，其它阶段成功
但 PTY 清理未完成返回 `20`；更早的失败优先。首个失败用例停止对应矩阵并保留其退出码。
失败日志名为 `sonicterm-<场景>-<显示后端>-<包布局>-smoke.log`，匹配上传 glob。
脚本会拒绝替换已有的 SonicTerm Debian 安装。

## 发布交接

本地包不会自动发布。Tag 驱动的 release 先验证全部 workspace 版本与类型化包片段，
再上传五个包、`release-assets.json` 和 `SHA256SUMS.txt`。准确 tag 规则与发布步骤见
[开发与发布](Development-and-Release-zh-CN)。
