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

READONLY mode blocks terminal input while you inspect history. Arrow keys or
`h/j/k/l` move its reading cursor; `w/b`, `0/$`, and `g` / `G` move by word, line, and buffer. Press `Escape` to exit. READONLY does not create a text
selection. Search, tab switching, pane focus, update checks, and saving current
font settings remain available. See [Keybindings](Keybindings) for the exact
controls and whitelist.

### rmux and tmux integration

SonicTerm starts child PTYs with `TERM=xterm-256color` and
`COLORTERM=truecolor`. Configure rmux/tmux to advertise `tmux-256color` to
programs inside panes; do not change SonicTerm itself to `TERM=tmux-256color`:

```tmux
set -g default-terminal "tmux-256color"
set -as terminal-features ",tmux-256color:RGB"
```

rmux needs a separate outer-terminal capability to relay the active pane's
working directory to SonicTerm. Enable title/path updates and advertise OSC 7
for the `xterm-256color` terminal that SonicTerm exposes to rmux:

```tmux
set -g set-titles on
set -as terminal-features ",xterm-256color:RGB:osc7"
```

The shell inside each pane must emit OSC 7 when its working directory changes.
rmux records that report and, with both settings above, emits the active pane's
path to SonicTerm. `#{pane_current_path}` is process-inspection metadata for rmux
formats; it is not substituted for a missing shell report. After changing
`terminal-features`, reload the configuration and detach/reattach so the outer
client capabilities are resolved again, then render a fresh prompt.

This relay lets SonicTerm resolve `src/main.rs`, `./file`, and bare names against
the exact pane. On Windows and Linux, hold `Ctrl` while pointing at the text; an
eligible target becomes underlined and can be clicked. SonicTerm still fails
closed when OSC 7 is absent, malformed, or names a foreign host: it never guesses
from process CWD, rmux status metadata, another pane, or a named user's home.
Absolute paths do not require OSC 7.

The outer terminal, multiplexer, and nested TUI form three independent input and
clipboard layers. The layer that owns the initial mouse press owns the complete
gesture until release:

| Gesture or copy path | Owner | Result |
| --- | --- | --- |
| Unmodified drag while the nested app requests mouse tracking | Nested app through rmux/tmux | App selection and app-controlled edge scrolling |
| Unmodified drag without nested mouse tracking, with multiplexer mouse mode on | rmux/tmux | Multiplexer copy-mode selection |
| `Shift` held before mouse-down | SonicTerm | Local terminal selection of currently rendered cells |
| Multiplexer copy command | rmux/tmux | Multiplexer buffer plus configured system/OSC 52 copy |
| Nested app OSC 52 write | Nested app, relayed by the multiplexer | SonicTerm native clipboard write |

For tmux-compatible rmux behavior, keep the standard conditional pane bindings
instead of forcing every drag into copy mode:

```tmux
set -g mouse on
bind -n MouseDown1Pane { select-pane -t=; send -M }
bind -n MouseDrag1Pane { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
```

The equivalent tmux defaults select the pane, forward mouse reports when the
inner program requests them, and enter copy mode otherwise. These semantics are
important for TUIs with virtual transcripts, such as Copilot CLI: only the
nested app can reveal additional transcript rows while a drag reaches an edge.
If every `MouseDrag1Pane` is rebound to `copy-mode -M`, the wheel and drag belong
to the multiplexer and can scroll into multiplexer history outside the app's live
alternate screen.

There are two clipboard paths:

```tmux
# Allow trusted pane applications and multiplexer copies to reach SonicTerm by OSC 52.
set -s set-clipboard on

# Optional alternative for rmux/tmux copy mode on Windows.
set -s copy-command 'powershell -NoProfile -NonInteractive -Command "[Console]::InputEncoding=[Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())"'
```

`set-clipboard on` lets programs in panes replace the outer native clipboard;
use it only for trusted pane output. The external `copy-command` path applies to
multiplexer-owned copy mode. On Windows, it must declare UTF-8 input; `clip.exe`
or a bare `$input | Set-Clipboard` can corrupt box drawing, CJK, accents, and
emoji through the console code page.

Troubleshooting:

- If a drag highlights only while the button is held and disappears on release,
  inspect which layer owns the press. A nested mouse-aware TUI may be drawing its
  own transient selection.
- If copy mode scrolls outside the nested TUI, restore the conditional
  `MouseDrag1Pane` binding so the nested app owns mouse tracking and edge scroll.
- If selection works but the native clipboard does not change, enable trusted
  OSC 52 relay with `set-clipboard on`, or configure a UTF-8 `copy-command` for
  multiplexer-owned copies.
- Hold `Shift` before mouse-down for a SonicTerm-local fallback. It cannot drive
  a nested application's virtual scrolling because SonicTerm sees only rendered
  cells.

See [Terminal IO and VT](Terminal-IO-and-VT) for the pointer-protocol and OSC 52
boundaries.

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
most eight non-space parts. Logical display-line reconstruction is also capped
at 4 KiB and eight consecutive rows. SonicTerm joins path fragments only across
recorded automatic margin wraps and only while the complete chain remains
visible. Every fragment then shares one authorization and underline. A hard
line break is never joined; a ninth row, an offscreen edge, or an evicted
predecessor leaves the chain inert.

SonicTerm chooses the longest unambiguous actionable candidate containing the
pointed cell. For a path ending in prose punctuation such as `src/main.rs,`, the
legal literal filename is probed first. Only when that literal is missing can a
shorter candidate without trailing comma, semicolon, period, colon, exclamation
mark, or question mark win; the underline then excludes the prose punctuation.
A blocked literal or equal-length ambiguity fails closed instead of falling
back. A complete standalone single-quoted contextual name, such as `'My Folder'`
from `ll`, is treated as `My Folder`. Other quoted or escaped names, `ls -F`
suffixes (`*`, `@`, `=`, `|`), editor `:line:column` suffixes, and targets with
wide, continuation, combining, or OSC 8-owned cells remain inert.

Only regular files and directories are eligible. Missing, inaccessible,
symlink/reparse-point, socket, device, executable, launcher, shortcut, installer,
network, UNC, WSL, and remote targets remain ordinary text. On macOS, an
ordinary non-executable source or script file is reveal-only: click selects it
in Finder through fixed `/usr/bin/open -R -- <target>` arguments and never opens
or executes it. App bundles, installers, `.command`, AppleScript, executable
mode, shebangs, and executable file magic remain blocked. Every platform
revalidates the exact target kind and action immediately before dispatch.
Windows uses `ShellExecuteExW` without a shell. Ordinary macOS files and
directories use `/usr/bin/open -- <target>`. Linux prefers the desktop portal
with an open file descriptor and otherwise uses a fixed `/usr/bin/xdg-open` or
`/bin/xdg-open` path. The macOS and Linux path-based openers still have the
normal pathname race after revalidation.

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

READONLY 模式会在查看历史记录时阻止终端输入。方向键或 `h/j/k/l` 移动阅读光标；
`w/b`、`0/$`、`g` / `G` 分别按单词、行和 buffer 移动。按 `Escape`
退出。READONLY 不创建文字选区。搜索、切换标签页、切换 pane 焦点、检查更新和保存
当前字体设置仍可使用。完整控制与允许列表见 [快捷键](Keybindings)。

### rmux 与 tmux 集成

SonicTerm 启动子 PTY 时设置 `TERM=xterm-256color` 与
`COLORTERM=truecolor`。rmux/tmux 应向 pane 内程序报告 `tmux-256color`；不要把
SonicTerm 自身的 `TERM` 改成 `tmux-256color`：

```tmux
set -g default-terminal "tmux-256color"
set -as terminal-features ",tmux-256color:RGB"
```

rmux 还需要单独声明外层终端能力，才能把活动 pane 的工作目录转发给 SonicTerm。
请启用 title/path 更新，并为 SonicTerm 向 rmux 暴露的 `xterm-256color` 声明 OSC 7：

```tmux
set -g set-titles on
set -as terminal-features ",xterm-256color:RGB:osc7"
```

每个 pane 内的 shell 必须在工作目录变化时发出 OSC 7。rmux 会记录该报告，并在上述
两项设置都生效时把活动 pane 的路径发给 SonicTerm。`#{pane_current_path}` 是 rmux
format 使用的进程检查元数据；shell 没有报告时，rmux 不会用它代替 OSC 7。修改
`terminal-features` 后，请重新加载配置并 detach/reattach，让外层 client 重新解析能力，
然后显示一次新 prompt。

完成转发后，SonicTerm 才能相对于准确 pane 解析 `src/main.rs`、`./file` 和 bare name。
Windows 与 Linux 上，指向文字时按住 `Ctrl`；可打开目标会显示下划线，随后可以点击。
OSC 7 缺失、格式错误或声明远端 host 时，SonicTerm 仍会 fail closed：它不会从进程 CWD、
rmux status 元数据、其它 pane 或命名用户 home 猜测目录。绝对路径不依赖 OSC 7。

外层终端、multiplexer 和内层 TUI 是三个独立的输入与剪贴板层。第一次按下鼠标时
取得所有权的层，会一直持有完整 gesture 直到松开：

| Gesture 或复制路径 | 所有者 | 结果 |
| --- | --- | --- |
| 内层程序请求 mouse tracking 时的无修饰键 drag | 通过 rmux/tmux 交给内层程序 | 程序选区与程序控制的边缘滚动 |
| 内层未请求 mouse tracking 且 multiplexer mouse mode 已开启时的无修饰键 drag | rmux/tmux | Multiplexer copy-mode 选区 |
| mouse-down 前已按住 `Shift` | SonicTerm | 对当前已绘制 cell 建立本地终端选区 |
| Multiplexer copy 命令 | rmux/tmux | Multiplexer buffer 加已配置的系统/OSC 52 复制 |
| 内层程序发出 OSC 52 write | 内层程序，由 multiplexer 转发 | SonicTerm 写入原生剪贴板 |

若要让 rmux 采用兼容 tmux 的行为，应保留标准条件式 pane 绑定，不要强制所有 drag
都进入 copy mode：

```tmux
set -g mouse on
bind -n MouseDown1Pane { select-pane -t=; send -M }
bind -n MouseDrag1Pane { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
```

tmux 的等效默认规则会先选择 pane；内层程序请求 mouse report 时转发，否则进入 copy
mode。对于 Copilot CLI 等具有虚拟会话记录的 TUI，这一点不能改变：drag 到边缘时，
只有内层程序知道如何显示更多会话行。若把所有 `MouseDrag1Pane` 都改绑到
`copy-mode -M`，drag 与 wheel 会归 multiplexer，并可能滚入应用 live alternate
screen 之外的 multiplexer history。

剪贴板有两条路径：

```tmux
# 允许可信 pane 程序和 multiplexer copy 通过 OSC 52 到达 SonicTerm。
set -s set-clipboard on

# Windows 上 rmux/tmux copy mode 的可选外部路径。
set -s copy-command 'powershell -NoProfile -NonInteractive -Command "[Console]::InputEncoding=[Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())"'
```

`set-clipboard on` 允许 pane 内程序替换外层原生剪贴板，只应对可信 pane 输出开启。
外部 `copy-command` 路径用于 multiplexer 自己持有的 copy mode。在 Windows 上，该命令
必须声明 UTF-8 输入；`clip.exe` 或裸 `$input | Set-Clipboard` 可能经过 console code
page 破坏框线字符、CJK、重音字符和 emoji。

排查方法：

- 若高亮只在按住鼠标时出现、松开即消失，先确认 press 归哪一层；支持鼠标的内层 TUI
  可能正在绘制自己的临时选区。
- 若 copy mode 滚出内层 TUI，请恢复条件式 `MouseDrag1Pane` 绑定，让内层程序持有 mouse
  tracking 与边缘滚动。
- 若可以选择但原生剪贴板不变，请用 `set-clipboard on` 开启可信 OSC 52 relay；若复制由
  multiplexer 持有，则配置 UTF-8 `copy-command`。
- mouse-down 前按住 `Shift` 可使用 SonicTerm 本地选区后备。它只能看到已绘制 cell，
  因此不能驱动内层程序的虚拟滚动。

Pointer protocol 与 OSC 52 边界见 [终端 IO 与 VT](Terminal-IO-and-VT)。

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

后台 probe 最多检查 37 个候选，每个候选最多跨 8 个非空格部分。逻辑显示行重建同样有
4 KiB 和连续 8 行上限。SonicTerm 只跨已记录的终端右边界自动换行连接路径片段，并且要求
完整链仍在可见 viewport 内；所有片段共享同一授权与下划线。硬换行绝不会连接；第 9 行、
不可见边界或前驱已从 scrollback 淘汰时，整条链保持不可操作。

SonicTerm 会选择包含鼠标 cell 的最长、无歧义且可操作候选。对于 `src/main.rs,` 这类以正文
标点结尾的路径，会先探测标点属于文件名的合法字面候选。只有该字面文件不存在时，才会尝试
去掉末尾逗号、分号、句点、冒号、感叹号或问号的较短候选；此时下划线不包含正文标点。字面
候选被阻止或同长度候选有歧义时会 fail closed，不会回退。`ll` 输出的完整独立单引号上下文
名称，例如 `'My Folder'`，会按 `My Folder` 处理。其它带引号或转义的名称、`ls -F` 后缀
（`*`、`@`、`=`、`|`）、editor `:line:column` 后缀，以及含宽字符、续格、组合字符或已属于
OSC 8 的 cell 的目标都保持不可操作。

只有普通文件和目录可以操作。不存在、不可访问、symlink/reparse point、socket、device、
executable、launcher、shortcut、installer、network、UNC、WSL 和远端目标都会保持普通文字。
macOS 上，普通且不可执行的源文件或脚本只能在 Finder 中显示：点击会通过固定参数
`/usr/bin/open -R -- <target>` 选中它，不会打开或执行。App bundle、installer、`.command`、
AppleScript、可执行权限、shebang 和可执行文件 magic 仍被阻止。每个平台都会在调用前立即
重新验证完全相同的目标类型与操作。Windows 使用不经过 shell 的 `ShellExecuteExW`；普通
macOS 文件和目录使用 `/usr/bin/open -- <target>`。Linux 优先把已打开的 file descriptor
交给 desktop portal；否则使用固定的 `/usr/bin/xdg-open` 或 `/bin/xdg-open`。macOS 和
Linux 的路径 opener 在重新验证之后仍有通常的 pathname race。

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
