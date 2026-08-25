# Usage / 用法

## English

### Install and first launch

Download the package for your platform from
[GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases):

- macOS Apple Silicon: `SonicTerm-<tag>-mac-aarch64.dmg`
- macOS Intel: `SonicTerm-<tag>-mac-x86_64.dmg`
- Windows x64: `SonicTerm-<tag>-windows-x86_64.msi`
- Linux x86_64: `SonicTerm-<tag>-linux-x86_64.deb` or
  `SonicTerm-<tag>-linux-x86_64.tar.gz`

On macOS, open the DMG and move `SonicTerm.app` to Applications. Release builds
are ad-hoc signed but do not have an Apple Developer ID signature or
notarization. If macOS blocks the first launch, use Finder’s **Open** context-menu
action. The minimum packaged macOS version is 14.0.

On Windows, run the MSI. It installs SonicTerm per machine under Program Files
and adds a Start-menu shortcut. It also registers SonicTerm as an available
handler for supported script files without changing the current default app.

Install the Debian package with:

```sh
sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb
```

Linux packages require x86_64 and glibc 2.35 or newer. The
`.deb` installs the required linked libraries and desktop metadata. For the
portable archive, extract it and run `sonicterm` from the extracted payload so
its adjacent `assets/` directory remains available. The host must supply the
runtime libraries; X11 needs `libxkbcommon-x11.so.0`. Both X11 and Wayland are
supported.

The first normal launch creates `~/.sonicterm/`, writes `sonicterm.toml`, and
seeds editable theme and keymap examples. For local package building and release
asset details, see [Packaging](Packaging).

### Common workflows

The command palette is the quickest way to discover actions:

- macOS: `Cmd+Shift+P`
- Windows and Linux: `Alt+Shift+P`

Common defaults are:

| Action | macOS | Windows and Linux |
| --- | --- | --- |
| New tab | `Cmd+T` | `Alt+T` or `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` |
| Split right / down | `Cmd+D` / `Cmd+Shift+D` | `Alt+D` / `Alt+Shift+D` |
| Focus panes | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Search | `Cmd+F` | `Alt+F` |
| READONLY mode | `Cmd+[` | `Alt+[` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` |
| Reload config | `Cmd+R` | `Alt+R` |

Each pane owns a separate child PTY. A tab can contain a split-pane tree. You can
reorder tabs, drag them between SonicTerm windows, or drag a tab away to create
a window. The live pane and PTY move with the tab; the shell does not restart.
Closing a split closes its PTY. Closing the final pane closes the tab.

Broadcast mode mirrors source-pane input to the other panes in the current tab
or in all tabs. Receiver panes are marked. The source is excluded, so it does
not receive the input twice. Use broadcast carefully because each receiver PTY
gets the same bytes.

For the complete default map, action names, and customization syntax, see
[Keybindings](Keybindings).

### Select and copy text

Drag to select cells. Double-click to select a word. Triple-click to select a
line. Continue dragging after a double- or triple-click to extend by whole words
or lines. SonicTerm does not auto-copy when the button is released.

Mouse-aware terminal applications can request the left button and drag motion.
In such a TUI, start with **Shift-drag** to bypass mouse reporting and make a
local SonicTerm selection. The choice is made on the initial button press and
lasts until release.

Use the platform copy shortcut after selecting. A successful explicit copy on
the alternate screen clears that selection and removes its highlight. A failed
clipboard write leaves a still-valid selection in place so you can retry. A
primary-screen selection remains after a successful copy. Repainting selected
cells to the same complete character/style/hyperlink/wide/combining identity
keeps the selection; an actual selected-cell change clears it before copy.
Terminal applications may also write UTF-8 text through OSC 52 target `c` up to
512 KiB. Clipboard reads/queries, malformed Base64, other selection targets, and
oversized writes are ignored.

#### Copilot CLI inside rmux

Copilot CLI uses mouse tracking for transcript selection and for scrolling while
a drag reaches an edge. When it runs inside rmux, leave rmux's standard
conditional pane bindings in place so a mouse-aware nested application receives
the complete press, drag, release, and wheel stream:

```tmux
set -g mouse on
bind -n MouseDown1Pane { select-pane -t=; send -M }
bind -n MouseDrag1Pane { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
set -s set-clipboard on
```

With these settings, Copilot owns selection and can extend it through its virtual
scrolling transcript. rmux relays Copilot's OSC 52 clipboard write to SonicTerm,
which writes it to the native clipboard. `set-clipboard on` trusts programs in
rmux panes to replace that clipboard; keep rmux's safer default if pane output is
not trusted.

Do not force every `MouseDrag1Pane` into rmux `copy-mode -M` for this workflow.
That makes rmux, rather than Copilot, own the drag and wheel; scrolling then walks
rmux history outside Copilot's live alternate screen. A Shift-drag remains the
SonicTerm-local fallback, but it can select only the cells currently rendered by
SonicTerm and cannot drive Copilot's virtual transcript scrolling.

READONLY mode blocks terminal input while you inspect history. Arrow keys or
`h/j/k/l` move its reading cursor; `w/b`, `0/$`, and `g` / `G` move by word, line, and buffer. Press `Escape` to exit. READONLY does not create a text
selection. Search, tab switching, pane focus, update checks, and saving current
font settings remain available. See [Keybindings](Keybindings) for the exact
controls and whitelist.

### Open URLs and local targets

Hold `Cmd` on macOS or `Ctrl` on Windows and Linux while pointing at a target.
A valid target becomes underlined; click it to open. OSC 8 links and plain-text
`http://`, `https://`, `mailto:`, and `file://` URIs take priority over raw
filesystem detection. Unrelated terminal output and same-value repaints do not
blink an unchanged target; changing the pointed row, target, CWD, viewport, or
openability identity revokes authorization and requires a fresh probe.

Raw local targets include:

- native absolute paths such as `/usr/local/etc`, `C:/Users/name`, and
  `C:\\Users\\name`;
- current-user home paths such as `~/notes` and Windows `~\\notes`;
- separator-relative paths such as `src/main.rs` and Windows `src\\main.rs`;
- explicit relative paths such as `./file`, `../file`, and `../../file`;
- contextual names such as `sonicterm`, `.DS_Store`, or `My Folder`.

These forms can contain ordinary spaces. Relative and contextual forms require
the exact pane to report a trustworthy absolute local working directory through
OSC 7. A missing, malformed, or foreign-host OSC 7 value fails closed. SonicTerm
never substitutes the process working directory, another pane’s directory, or a
named user’s home.

The background probe checks at most 37 candidates, and each candidate spans at
most eight non-space parts. SonicTerm chooses the longest unambiguous openable
candidate containing the pointed cell. A blocked candidate or equal-length
ambiguity fails closed instead of falling back to a shorter name. A complete
standalone single-quoted contextual name, such as `'My Folder'` from `ll`, is
treated as `My Folder`. Other quoted or escaped names, `ls -F` suffixes (`*`,
`@`, `=`, `|`), editor `:line:column` suffixes, wrapped paths, and tokens with
wide or combining cells are not raw clickable targets.

Only regular files and directories are eligible. Missing, inaccessible,
symlink/reparse-point, socket, device, executable, launcher, script, shortcut,
installer, network, UNC, WSL, and remote targets remain ordinary text. Each
platform revalidates the target immediately before opening it. Windows uses
`ShellExecuteExW` without a shell. macOS uses `/usr/bin/open -- <target>`.
Linux prefers the desktop portal with an open file descriptor and otherwise
uses a fixed `/usr/bin/xdg-open` or `/bin/xdg-open` path. The macOS and Linux
path-based openers still have the normal pathname race after revalidation.

Set `terminal.clickable_bare_names = false` to disable contextual names. Set
`terminal.clickable_local_targets = false` to disable every raw local target.
Neither setting disables URI or OSC 8 links. For exact defaults and reload
behavior, see [Configuration](Configuration).

### Open script files as drafts

Installed macOS builds can appear in Finder’s **Open With** menu for `.sh`,
`.command`, and `.tool`. The Windows MSI registers SonicTerm as an available
handler for `.ps1`, `.cmd`, `.bat`, and `.sh`. Installation does not replace the
current default handler.

Opening a supported file creates a tab whose working directory is the file’s
parent. SonicTerm safely quotes an absolute path and inserts a command at the
prompt **without Enter or another control byte**:

- POSIX `sh`, `bash`, `zsh`, `dash`, or `ksh`: `.sh`, `.command`, `.tool`;
- PowerShell or `pwsh`: `.ps1`, `.cmd`, `.bat`;
- Command Prompt: `.cmd`, `.bat`, provided the path contains none of `%`, `!`,
  or `"`.

Review, edit, submit, or clear the draft yourself. An unknown shell, unsupported
shell/script pair, relative or non-Unicode path, control character, or unsafe
Command Prompt path still opens the tab but shows a warning and inserts no
command.

This is a draft-input feature, not a sandbox. Shell startup files run first and
can read PTY input. A startup profile that reads and evaluates input can execute
or consume the draft without Enter. Do not select SonicTerm as a script handler
when your shell startup code does that. SonicTerm itself does not run the script
or interpreter for the open action.

On Windows, each file-association invocation starts a new SonicTerm process. On
macOS, later open requests go to the running app and add tabs.

### Configure and troubleshoot

Use these canonical pages instead of duplicating their detailed rules here:

- Preferences, defaults, reload, and save: [Configuration](Configuration)
- Shortcuts, actions, and READONLY controls: [Keybindings](Keybindings)
- Theme schema and colors: [Themes](Themes)
- Logs, crash files, and diagnostics: [Logging](Logging)

## 中文

### 安装与首次启动

从 [GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases) 下载当前平台的安装包：

- macOS Apple Silicon：`SonicTerm-<tag>-mac-aarch64.dmg`
- macOS Intel：`SonicTerm-<tag>-mac-x86_64.dmg`
- Windows x64：`SonicTerm-<tag>-windows-x86_64.msi`
- Linux x86_64：`SonicTerm-<tag>-linux-x86_64.deb` 或
  `SonicTerm-<tag>-linux-x86_64.tar.gz`

macOS 上打开 DMG，把 `SonicTerm.app` 移到 Applications。发布构建使用 ad-hoc
签名，但没有 Apple Developer ID 签名，也没有 notarize。如果首次启动被 macOS
阻止，请在 Finder 右键菜单中选择 **Open**。安装包要求 macOS 14.0 或更高版本。

Windows 上运行 MSI。它会按机器安装到 Program Files，并添加开始菜单快捷方式。
安装程序还会把 SonicTerm 注册为支持脚本文件的可选 handler，但不会修改当前默认应用。

用以下命令安装 Debian package：

```sh
sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb
```

Linux package 面向 x86_64，并保证最多需要 glibc 2.35 ABI。`.deb` 会安装已链接的
依赖和桌面 metadata。使用便携归档时，请先解压，再从 payload 目录运行
`sonicterm`，确保相邻的 `assets/` 目录仍可读取。主机需要自行提供运行库；X11
需要 `libxkbcommon-x11.so.0`。X11 与 Wayland 都受支持。

首次正常启动会创建 `~/.sonicterm/`、写入 `sonicterm.toml`，并生成可编辑的主题
和 keymap 示例。本地打包与发布资产的详细说明见 [打包](Packaging)。

### 常用工作流

命令面板是查找 action 最快的方法：

- macOS：`Cmd+Shift+P`
- Windows 和 Linux：`Alt+Shift+P`

常用默认快捷键如下：

| 功能 | macOS | Windows 和 Linux |
| --- | --- | --- |
| 新建标签页 | `Cmd+T` | `Alt+T` 或 `Ctrl+T` |
| 关闭当前 pane 或标签页 | `Cmd+W` | `Alt+W` |
| 向右 / 向下分屏 | `Cmd+D` / `Cmd+Shift+D` | `Alt+D` / `Alt+Shift+D` |
| 切换 pane 焦点 | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 搜索 | `Cmd+F` | `Alt+F` |
| READONLY 模式 | `Cmd+[` | `Alt+[` |
| 快速选择 URL | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| 广播到当前标签页 | `Cmd+Shift+B` | `Alt+Shift+B` |
| 重载配置 | `Cmd+R` | `Alt+R` |

每个 pane 都有独立的子 PTY。一个标签页可以包含分屏树。标签页可以排序、在
SonicTerm 窗口之间拖动，也可以拖出成为新窗口。现有 pane 与 PTY 会一起移动，
shell 不会重启。关闭分屏会关闭对应 PTY；关闭最后一个 pane 会关闭标签页。

广播模式会把源 pane 的输入复制到当前标签页或所有标签页中的其它 pane，并标记
接收 pane。源 pane 不在接收集合中，因此不会收到两份输入。请谨慎使用，因为每个
接收 PTY 都会得到相同字节。

完整默认快捷键、action 名称和自定义格式见 [快捷键](Keybindings)。

### 选择与复制文字

拖动可以按 cell 选择。双击选择单词，三击选择整行。双击或三击后继续拖动时，会按
完整单词或整行扩展。松开鼠标不会自动复制。

支持鼠标的终端程序可以请求左键和 drag motion。在这类 TUI 中，请从
**Shift-drag** 开始，以绕过 mouse reporting，让 SonicTerm 在本地选择文字。
Gesture owner 在第一次按下鼠标时确定，并保持到松开。

选好后使用当前平台的复制快捷键。在 alternate screen 中，显式复制成功后会清除
该选区并移除高亮。剪贴板写入失败时，只要选区仍有效，它就会保留，方便重试。
Primary screen 中复制成功后，选区仍保留。若选中 cell 只是按完全相同的字符、style、
hyperlink、宽字符结构和组合字符重新绘制，选区会保留；实际 cell identity 改变时，
SonicTerm 会在复制前清除它。终端程序也可通过 OSC 52 的 `c` target 写入最多 512 KiB
的 UTF-8 文字。剪贴板读取/查询、格式错误的 Base64、其它 selection target 和超限写入
都会被忽略。

#### 在 rmux 中运行 Copilot CLI

Copilot CLI 使用 mouse tracking 选择会话文字，并在拖动到边缘时滚动内容。它在 rmux
中运行时，应保留 rmux 的标准条件式 pane 绑定，让支持鼠标的内层程序收到完整的按下、
drag、松开与 wheel 事件流：

```tmux
set -g mouse on
bind -n MouseDown1Pane { select-pane -t=; send -M }
bind -n MouseDrag1Pane { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
set -s set-clipboard on
```

这些设置让 Copilot 持有选区，并可跨其虚拟滚动会话继续扩展。rmux 会把 Copilot 的
OSC 52 剪贴板写入转发给 SonicTerm，再由 SonicTerm 写入原生剪贴板。
`set-clipboard on` 也表示信任 rmux pane 内的程序改写剪贴板；若不信任 pane 输出，
应保留 rmux 更安全的默认值。

此工作流不要强制所有 `MouseDrag1Pane` 进入 rmux `copy-mode -M`。否则 drag 与 wheel
会归 rmux，而不是 Copilot；滚动会进入 Copilot live alternate screen 之外的 rmux
history。Shift-drag 仍可作为 SonicTerm 本地选区后备，但它只能选择 SonicTerm 当前已经
绘制的 cell，不能驱动 Copilot 的虚拟会话滚动。

READONLY 模式会在查看历史记录时阻止终端输入。方向键或 `h/j/k/l` 移动阅读光标；
`w/b`、`0/$`、`g` / `G` 分别按单词、行和 buffer 移动。按 `Escape`
退出。READONLY 不创建文字选区。搜索、切换标签页、切换 pane 焦点、检查更新和保存
当前字体设置仍可使用。完整控制与允许列表见 [快捷键](Keybindings)。

### 打开 URL 与本地目标

鼠标指向目标时，macOS 按住 `Cmd`，Windows 和 Linux 按住 `Ctrl`。有效目标会显示
下划线；点击即可打开。OSC 8 link 和普通文字中的 `http://`、`https://`、
`mailto:`、`file://` URI 优先于原始文件系统检测。无关终端输出和同值重绘不会让
未变化的目标闪烁；pointed row、target、CWD、viewport 或可打开 identity 改变时，
授权会被撤销并重新 probe。

原始本地目标包括：

- `/usr/local/etc`、`C:/Users/name`、`C:\\Users\\name` 等原生绝对路径；
- `~/notes` 和 Windows `~\\notes` 等当前用户 home 路径；
- `src/main.rs` 和 Windows `src\\main.rs` 等带分隔符的相对路径；
- `./file`、`../file`、`../../file` 等显式相对路径；
- `sonicterm`、`.DS_Store`、`My Folder` 等上下文名称。

这些形式可以包含普通空格。相对形式和上下文名称要求准确 pane 通过 OSC 7 报告可信
本机绝对工作目录。OSC 7 缺失、格式错误或来自远端 host 时会 fail closed。
SonicTerm 不会改用进程工作目录、其它 pane 的目录或命名用户的 home。

后台 probe 最多检查 37 个候选，每个候选最多跨 8 个非空格部分。SonicTerm 会选择
包含鼠标 cell 的最长、无歧义且可打开候选。遇到 blocked 候选或同长度歧义时会
fail closed，不会退回较短名称。`ll` 输出的完整独立单引号上下文名称，例如
`'My Folder'`，会按 `My Folder` 处理。其它带引号或转义的名称、`ls -F` 后缀
（`*`、`@`、`=`、`|`）、editor `:line:column` 后缀、跨行路径，以及含宽字符或
组合 cell 的 token 都不会成为原始可点击目标。

只有普通文件和目录可以打开。不存在、不可访问、symlink/reparse point、socket、
device、executable、launcher、script、shortcut、installer、network、UNC、WSL 和
远端目标都会保持普通文字。每个平台都会在打开前立即重新验证。Windows 使用
`ShellExecuteExW`，不经过 shell。macOS 使用 `/usr/bin/open -- <target>`。Linux
优先把已打开的 file descriptor 交给 desktop portal；否则使用固定的
`/usr/bin/xdg-open` 或 `/bin/xdg-open`。macOS 和 Linux 的路径 opener 在重新验证
之后仍有通常的 pathname race。

设置 `terminal.clickable_bare_names = false` 可以关闭上下文名称。设置
`terminal.clickable_local_targets = false` 可以关闭所有原始本地目标。两者都不影响
URI 或 OSC 8 link。准确默认值和重载行为见 [配置](Configuration)。

### 以草稿方式打开脚本

安装后的 macOS build 可以出现在 Finder 为 `.sh`、`.command`、`.tool` 提供的
**Open With** 菜单中。Windows MSI 会把 SonicTerm 注册为 `.ps1`、`.cmd`、`.bat`、
`.sh` 的可选 handler。安装不会替换当前默认 handler。

打开受支持文件时，SonicTerm 会在文件父目录创建标签页，安全引用绝对路径，并把命令
放到提示符中，**不会发送 Enter 或其它 control byte**：

- POSIX `sh`、`bash`、`zsh`、`dash`、`ksh`：`.sh`、`.command`、`.tool`；
- PowerShell 或 `pwsh`：`.ps1`、`.cmd`、`.bat`；
- Command Prompt：`.cmd`、`.bat`，且路径不能包含 `%`、`!`、`"`。

请自行检查、修改、提交或清空草稿。未知 shell、不支持的 shell/script 组合、相对路径、
非 Unicode 路径、control character 或不安全的 Command Prompt 路径，仍会打开标签页，
但只显示 warning，不插入命令。

这是输入草稿功能，不是 sandbox。Shell startup file 会先运行，也可以读取 PTY 输入。
如果启动配置会读取并执行输入，即使没有 Enter，也可能执行或吞掉草稿。使用这类配置时，
不要把 SonicTerm 设为脚本 handler。对于这个 open action，SonicTerm 自身不会运行脚本
或解释器。

Windows 中，每次文件关联调用都会启动新的 SonicTerm 进程。macOS 会把后续 open request
发送到当前运行的应用，并添加标签页。

### 配置与排障

详细规则由以下页面维护，这里不重复：

- 偏好、默认值、重载与保存：[配置](Configuration)
- 快捷键、action 与 READONLY 控制：[快捷键](Keybindings)
- 主题 schema 与颜色：[主题](Themes)
- 日志、crash 文件与诊断：[日志](Logging)
