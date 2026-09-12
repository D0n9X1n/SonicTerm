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
Splitting a zoomed pane exits zoom, restores the split layout, and focuses the
new pane in both main and torn-out windows. A refused split leaves zoom and
focus unchanged; refusal in a live child window never redirects the split to
the main window.

Broadcast mode mirrors source-pane input to the other panes in the current tab
or in all tabs. Receiver panes are marked. The source is excluded, so it does
not receive the input twice. Use broadcast carefully because each receiver PTY
gets the same bytes.

For the complete default map, action names, and customization syntax, see
[Keybindings](Keybindings).

### Window names and numbers

Terminal windows receive process-local numbers starting at 1: `#1 SonicTerm`.
Use **Rename Window** in the command palette to set a custom name, for example
`#2 Work`. Edit only the name: Enter trims surrounding whitespace
and saves, blank input resets the numbered default, and Escape cancels. Names
support Unicode and IME, with at most 128 Unicode scalar values after trimming;
control characters, line breaks, and overlong input are rejected with feedback.
The configured paste shortcut inserts into this editor, never into the shell.

Numbers are never reused or reassigned within a process. New windows and torn-out
tabs get fresh numbers and blank names; moving tabs into an existing window or
hiding/restoring a retained window preserves its identity. Warm helper windows
remain unnumbered until adopted. Names and numbers are not saved across restarts;
separate processes may each start at 1. Tab names, shell commands, OSC titles,
working directories, focus changes, and config reload do not rename windows.
The editor targets the window where it opened and cancels when that window closes.
Rename Window and the command palette remain available in READONLY mode; terminal
input and unsafe commands remain blocked.

Titles appear in applicable OS window lists, previews, and switchers. Windows
uses taskbar previews and Alt+Tab; macOS uses window lists, Dock window menus,
and Mission Control, not application-level Cmd+Tab. Linux display depends on the
X11/Wayland desktop. The OS may hide or truncate labels; app grouping, icons,
application IDs, and the Dock application label are unchanged.

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
font settings, the command palette, and window renaming remain available. See [Keybindings](Keybindings) for the exact
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

This relay enables exact-pane relative paths and CWD inheritance for ordinary new
tabs and splits in main and child windows. Inheritance accepts only an empty host,
`localhost`, or the exact local hostname and a native absolute path of at most
4,096 decoded UTF-8 bytes. Explicit CWD wins; new windows do not inherit it.
It also lets SonicTerm resolve `src/main.rs`, `./file`, and bare names against
the exact pane. On Windows and Linux, hold `Ctrl` while pointing at the text; an
eligible target becomes underlined and can be clicked. SonicTerm still fails
closed when OSC 7 is absent, malformed, or names a foreign host: it never guesses
from process CWD, rmux status metadata, another pane, or a named user's home.
Absolute paths do not require OSC 7.

For foreground `rmux`, `tmux`, or `screen`, a nonempty raw OSC title is preferred
even when CWD is known; manual tab titles still win. Other processes retain normal
CWD-first automatic titles. OSC 8 preserves URI semicolons; OSC 133 `B` ends the
prompt without timing, `C` starts execution, and `A`/`D` keep their region behavior.
This is bounded shell integration, not full WezTerm parity.

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

These bindings select the pane and forward mouse reports to a requesting TUI;
otherwise they enter copy mode. Only a nested TUI can reveal more of its virtual
transcript during edge dragging. Unconditionally binding `MouseDrag1Pane` to
`copy-mode -M` instead gives wheel/drag to the multiplexer and can scroll outside
the app's live alternate screen.

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

Plain hover underlines both detected URLs and OSC 8 labels with the theme's yellow
hint; the open modifier switches to the action accent. OSC 8 coverage follows the
contiguous label across automatic wraps, including wide cells, but never crosses
hard line breaks or gaps into another occurrence. At most eight visible fragments
are painted, always retaining the pointed fragment of an overlong label.
URLs inside prose parentheses or square brackets are detected without including
the surrounding wrappers in the destination or underline.

Modifier-hover shows a local destination only after its current filesystem probe
validates it. Pending, missing, ambiguous, or rejected local targets have no preview,
so directory-listing columns are never shown as unverified paths. It adds no action labels or
error messages, and does not change the clipboard or authorize navigation. It uses
the same placement, escaping, wrapping, and dismissal as URL previews.

Holding the same modifier over any URL shows its destination beside the pointer,
including labeled OSC 8 links, links whose label already equals the destination,
and auto-detected plain-text URLs. Click while holding the modifier to open it.
Query separators such as `&` are preserved unchanged, so links with multiple query
parameters (including repository file and line links) can be opened. Scheme,
length, control-character, and other forbidden-character checks still apply.

Terminal underline styling continues across explicitly printed spaces that carry
the same underline style and color. Unstyled cells remain gaps; clearing cells
does not paint new underline ink even when underline mode is active.

The preview does not fetch a website or authorize navigation. Local file URIs
require the same path validation; control and directional formatting characters are escaped for display.
Failures are explained in click-triggered error notifications, not in the preview. Long destinations wrap,
with an explicit ellipsis if they cannot fit. Release the modifier or leave the
link to hide it; focus, modal, pane, viewport, and content changes refresh or clear
it. GPU and Windows software rendering use the same preview overlay.

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
suffixes (`*`, `@`, `=`, `|`), and raw paths containing wide, continuation,
combining, or OSC 8-owned cells remain inert.

Source references such as `install.sh:889–919`, `src/main.rs:12`, and
`src/main.rs:12:4` retain the full underline but resolve only the filename.
Line and column values must be positive; ranges accept `-` or `–` and must not
run backwards. Relative source names still require the exact pane's trusted CWD.
Windows, macOS, and Linux select the referenced file in its containing folder.
Line metadata does not launch an editor or restrict the file's type or contents.

### Local target behavior

Use Ctrl+click on Windows/Linux or Cmd+click on macOS.

| Target or state | Expected behavior |
| --- | --- |
| Existing ordinary directory | Navigate into the directory in the file manager. |
| Existing regular file, including scripts, executables, installers, and shortcut files | Open its containing folder and select the file itself. Never execute it, follow a shortcut, or invoke its associated application. |
| macOS application/package directory | Select the package in Finder without launching it. |
| Existing bare filename in a listing or prose | Resolve against the exact pane's trusted local working directory; underline and select only the validated filename, including spaces. |
| Unverified bare name or ordinary text | No preview, file action, error notification, or clipboard write. Listing metadata is not a filepath. |
| Explicit filepath that is missing, pending validation, or ambiguous | Show the filepath and the missing/pending/unclear reason on modifier-click. Do not navigate or copy on the first click. |
| Explicit rejected filepath | Show its filepath and rejection reason; never bypass identity or locality checks. |
| First file-manager action failure | Show the attempted filepath, reason, and second-click copy instruction. Leave the clipboard unchanged. |
| Second modifier-click on the same failed filepath while its error is visible | Copy that filepath instead of retrying, and report copy success or failure. |
| Dismissed, expired, or replaced error | A subsequent click starts a new attempt rather than confirming the previous error. |
| HTTP/HTTPS or mail URL | Keep URL preview and browser/mail navigation. |

Explicit filepaths include native absolute paths, `./`/`../`/`~/` paths,
relative paths containing separators, source-location references, and local file-URI
or native-path OSC 8 destinations. Explicit paths retain spaces in their filenames.
For unverified source references with a bare filename, failure feedback excludes
surrounding prose; spaced bare filenames require filesystem validation or an
explicit path/OSC 8 destination. Bare names become filepath targets only after
filesystem validation. File extensions, executable permissions, and file contents
do not prevent selection. Symlinks, reparse points, special devices, and unsupported
remote/network paths remain protected. Every platform revalidates target identity
and kind immediately before dispatch.
All platforms navigate directories and reveal files with the file selected,
without invoking the file's application. Windows selects files through
`SHOpenFolderAndSelectItems`; Finder uses `/usr/bin/open -R -- <target>`; Linux
uses `org.freedesktop.FileManager1.ShowItems`. Unavailable or rejected selection
is reported in the requesting window without falling back to file opening or
parent-only navigation. Only a validated local target invokes a native file action.
Explicit filepath failures receive click-triggered feedback; guessed bare-name spans do not.
The first failed action shows the attempted filepath, reason, and an instruction to
click the same link again while the error is visible. It does not change the clipboard.
The second modifier-click on that same failed filepath copies it instead of retrying
navigation, then reports copy success or failure. Dismissed, expired, or replaced
notifications and clicks on another target do not confirm a previous failure.
Short notifications fit their longest shaped line. Notifications preserve newlines
and wrap Unicode graphemes; content exceeding the viewport is marked with an ellipsis.
Hover never copies. Native file-manager errors return only while the initiating
pane still belongs to its window. A click without the modifier keeps normal selection.
Directory navigation retains each platform's ordinary directory opener. Path-based
file-manager requests still have the normal pathname race after revalidation.

OSC 8 destinations with native absolute paths (including Windows `C://…`),
Windows drive-rooted `file:c://…` links, and local `file://` URIs enter the same
filesystem authorization path. Local `#3`, `#L3`, and ascending `#L3-L7` fragments
are source-line metadata, not filename content; selection uses the file alone.
This convention also applies to native absolute OSC 8 destinations. For a literal
filename ending in `#3` or `#L3`, use a file URI with `%23` for the hash. Plain-text
native filename scanning retains its existing literal-hash behavior. File URI escapes
are decoded once; native-path percent characters remain literal. Remote authorities,
UNC/device paths and malformed destinations are rejected without using the displayed
label as a substitute. HTTP/HTTPS and mail links retain their existing behavior.

Opening a URI on Windows takes the same shell-free boundary as a validated
local target: `ShellExecuteExW` receives the URI as one NUL-terminated UTF-16
string, so no command interpreter parses it. Environment substitution stays disabled, so a
percent-delimited URI such as `https://example.com/%20space` or one containing
`%USERNAME%` reaches your browser or mail client exactly as shown on screen
rather than expanding to an environment value.

Set `terminal.clickable_bare_names = false` to disable contextual names. Set
`terminal.clickable_local_targets = false` to disable every raw local target.
The local-target setting also applies to local file URIs and native-path OSC 8 links;
web/mail URI links remain enabled. For exact defaults and reload
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
对已放大的 pane 分屏会退出放大状态、恢复分屏布局并聚焦新 pane；主窗口与拖出的窗口行为一致。
分屏被拒绝时，放大状态和焦点保持不变；存活子窗口中的拒绝不会把分屏操作转移到主窗口。

广播模式会把源 pane 的输入复制到当前标签页或所有标签页中的其它 pane，并标记
接收 pane。源 pane 不在接收集合中，因此不会收到两份输入。请谨慎使用，因为每个
接收 PTY 都会得到相同字节。

完整默认快捷键、action 名称和自定义格式见 [快捷键](Keybindings)。

### 窗口名称与编号

终端窗口在进程内从 1 开始编号，例如 `#1 SonicTerm`。在命令面板中选择
**重命名窗口**，可设置为 `#2 Work`。只需编辑自定义名称：Enter 去除
首尾空白并保存，留空恢复默认编号标题，Escape 取消。支持 Unicode 和输入法；去除
首尾空白后最多 128 个 Unicode 标量值。控制字符、换行及超长输入会被拒绝并显示原因。
已配置的粘贴快捷键只向编辑框插入文字，不会发送给 shell。

同一进程内编号永不复用或重新分配。新建窗口与拆出的标签页获得新编号和空名称；
把标签页移入现有窗口，或隐藏后恢复保留的窗口，都不会改变窗口身份。预热辅助窗口
在被采用前不编号。名称与编号不跨重启保存；不同进程都可能从 1 开始。标签页名称、
shell 命令、OSC 标题、工作目录、焦点变化及配置重载均不会重命名窗口。编辑器始终
针对打开它的窗口，窗口关闭时取消。READONLY 模式下仍可使用命令面板和重命名窗口，
终端输入及不安全操作仍被阻止。

原生标题用于适用的系统窗口列表、预览及切换器。Windows 包括任务栏预览和 Alt+Tab；
macOS 包括窗口列表、Dock 窗口菜单及 Mission Control，不包括应用级 Cmd+Tab。
Linux 取决于 X11/Wayland 桌面。系统可能隐藏或截断标题；应用分组、图标、应用 ID
及 Dock 应用名称保持不变。

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
当前字体设置、命令面板及重命名窗口仍可使用。完整控制与允许列表见 [快捷键](Keybindings)。

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

转发使相对路径使用准确窗格，并让主/子窗口的普通新标签页和分屏继承其 CWD。继承只接受
空主机、`localhost` 或准确本机主机名；原生绝对路径解码后 UTF-8 不超过 4,096 字节。
显式 CWD 优先，新窗口不继承。SonicTerm 可据此解析 `src/main.rs`、`./file` 和 bare name。
Windows 与 Linux 上，指向文字时按住 `Ctrl`；可打开目标会显示下划线，随后可以点击。
OSC 7 缺失、格式错误或声明远端 host 时，SonicTerm 仍会 fail closed：它不会从进程 CWD、
rmux status 元数据、其它 pane 或命名用户 home 猜测目录。绝对路径不依赖 OSC 7。

前台为 `rmux`、`tmux` 或 `screen` 时，即使已知 CWD，也优先显示非空原始 OSC 标题；
手动标题仍优先。其它进程保持普通的 CWD 优先自动标题。OSC 8 保留 URI 分号；OSC 133
`B` 结束提示符但不计时，`C` 开始执行，`A`/`D` 保持区域行为。这是有限范围的 shell 集成，
不表示完整 WezTerm 对等能力。

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

这些绑定先选择 pane；内层 TUI 请求鼠标报告时转发，否则进入复制模式。只有内层 TUI 能在
边缘拖动时显示更多虚拟会话记录。无条件把 `MouseDrag1Pane` 绑定为 `copy-mode -M` 会让
multiplexer 接管滚轮与拖动，并可能滚入应用 live alternate screen 外的历史。

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

普通悬停会以主题黄色提示为检测到的 URL 和 OSC 8 标签添加下划线；按住打开修饰键后改用
操作强调色。OSC 8 覆盖范围沿连续标签跨越自动换行，包括宽字符，但不会跨硬换行或间隔
连接另一次出现的链接。最多绘制八个可见片段；标签过长时仍保留指针所在片段。
正文圆括号或方括号中的 URL 也会被检测到，外层括号不会进入目标地址或下划线范围。

按住修饰键悬停时，本地目标只有通过当前文件系统探测验证后才显示预览。
待验证、不存在、有歧义或被拒绝的本地目标不显示预览，避免把目录列表字段显示为未经验证的路径。
预览不添加操作标签或错误信息，不更改剪贴板，也不授予打开权限。位置、转义、换行和隐藏
规则与 URL 预览相同。

在任意 URL 上按住同一修饰键，都会在指针旁预览目标，包括带标签的 OSC 8 链接、
标签与目标完全相同的链接，以及自动检测的纯文本 URL。按住修饰键并单击即可打开。
`&` 等查询分隔符原样保留，因此含多个查询参数的链接（包括仓库文件和行号链接）
可以打开。协议、长度、控制字符以及其他禁用字符的检查仍然有效。

终端下划线会跨越显式输出且具有相同下划线样式与颜色的空格。未设置下划线的单元格
仍会形成间隔；即使下划线模式处于开启状态，清除单元格也不会产生新的下划线。

预览不会访问网站，也不会授予打开权限。本地 file URI 同样需要路径验证；控制字符和方向格式字符
会转义后显示。失败原因在单击后的错误通知中说明，不会添加到预览。长目标自动换行，无法完整
容纳时明确显示省略号。松开修饰键或离开链接即隐藏；焦点、模态界面、pane、viewport
和内容变化会刷新或清除预览。GPU 与 Windows 软件渲染使用相同的预览覆盖层。

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
（`*`、`@`、`=`、`|`），以及含宽字符、续格、组合字符或已属于 OSC 8 的 cell 的原始路径
都保持不可操作。

`install.sh:889–919`、`src/main.rs:12` 和 `src/main.rs:12:4` 等源文件引用保留完整下划线，
但只解析文件名。行号与列号必须为正数；范围接受 `-` 或 `–`，且终点不能早于起点。
相对源文件名仍要求准确 pane 的可信 CWD。Windows、macOS 和 Linux 都在所在文件夹中
选中引用的文件。行号不启动编辑器，也不限制文件类型或内容。

### 本地目标行为

Windows/Linux 使用 Ctrl+单击，macOS 使用 Cmd+单击。

| 目标或状态 | 预期行为 |
| --- | --- |
| 存在的普通目录 | 在文件管理器中进入该目录。 |
| 存在的普通文件，包括脚本、可执行文件、安装包和快捷方式文件 | 打开所在文件夹并选中文件本身；不执行、不跟随快捷方式、不调用关联应用。 |
| macOS 应用或软件包目录 | 在 Finder 中选中软件包，不启动它。 |
| 列表或文字中存在的裸文件名 | 根据准确窗格的可信本地工作目录解析；仅给验证后的文件名加下划线并选择，保留文件名中的空格。 |
| 未验证的裸名称或普通文字 | 无预览、文件操作、错误通知或剪贴板写入；列表元数据不是文件路径。 |
| 不存在、待验证或有歧义的显式文件路径 | 按修饰键单击后显示路径及缺失、待验证或不明确的原因；第一次不导航、不复制。 |
| 被拒绝的显式路径 | 显示路径和拒绝原因，不绕过身份或本地性检查。 |
| 文件管理器操作第一次失败 | 显示尝试的路径、原因和再次单击复制的提示，保留剪贴板。 |
| 错误仍显示时再次按修饰键单击同一失败路径 | 复制该路径而非重试，并报告复制成功或失败。 |
| 错误已关闭、过期或被替换 | 后续单击开始新尝试，而非确认之前的错误。 |
| HTTP/HTTPS 或邮件 URL | 保持 URL 预览和浏览器或邮件导航行为。 |

显式路径包括本机绝对路径、`./`、`../`、`~/` 路径、带分隔符的相对路径、源位置引用，
以及本地 file URI 或本机路径 OSC 8 目标。显式路径保留文件名中的空格。对于未经验证、
以裸文件名为基础的源位置引用，失败提示排除周围正文；含空格裸文件名需要文件系统验证，
或通过显式路径/OSC 8 目标指定。裸名称只有通过文件系统验证后才成为文件路径目标。
文件扩展名、执行权限和文件内容不会阻止选中文件。符号链接、重解析点、特殊设备和不支持的
远端或网络路径仍受保护。各平台在调用前重新验证目标身份和类型。
所有平台都进入目录，或打开文件所在文件夹并选中文件，
不调用文件关联的应用。Windows 使用 `SHOpenFolderAndSelectItems`，Finder 使用
`/usr/bin/open -R -- <target>`，Linux 使用 `org.freedesktop.FileManager1.ShowItems`。
选择功能不可用或被拒绝时会在发起请求的窗口报告失败，不会回退到打开文件或仅打开父目录。
只有已验证的本地目标才调用原生文件操作。显式路径失败会显示单击触发的反馈；裸名称猜测不会。
操作第一次失败时，通知显示尝试的文件路径、原因，以及再次单击同一链接以复制的提示，
不会更改剪贴板。错误仍显示时，再次按住修饰键单击同一失败路径才复制，不重试打开，
随后报告复制成功或失败。通知关闭、过期、被替换或单击其他目标后，不确认之前的失败。
短通知按实际整形后的最长行收缩，保留换行并按 Unicode 字素换行；超过窗口空间的内容
明确以省略号标记。悬停不复制。原生错误只返回原始窗格仍属于的发起窗口。
不按修饰键的单击保留正常选择行为。目录仍使用各平台
现有的目录打开方式。基于路径的文件管理器请求在重新验证后仍存在通常的 pathname race。

OSC 8 中的本机绝对路径（包括 Windows `C://…`）、Windows 驱动器绝对路径 `file:c://…`
和本地 `file://` URI 使用相同的文件系统授权流程。本地 `#3`、`#L3` 和升序 `#L3-L7`
片段属于源文件行号信息，不属于文件名；选择操作仅使用文件路径。本机绝对路径 OSC 8
目标也遵循此约定。若文件名确实以 `#3` 或 `#L3` 结尾，请使用以 `%23` 表示井号的 file URI。
纯文本本机路径扫描仍保留原有的字面井号行为。file URI 转义只解码一次；本机路径中的百分号保持字面含义。远端 authority、
UNC/设备路径以及格式错误的目标会被拒绝，不会以显示标签替代。HTTP/HTTPS 和邮件链接
保持原有行为。

Windows 上打开 URI 与打开已验证本地目标使用同一条不经过 shell 的边界：`ShellExecuteExW`
以单个 NUL 结尾的 UTF-16 字符串接收 URI，不会有任何命令解释器解析它。环境变量替换保持关闭，因此
`https://example.com/%20space` 这类以百分号分隔的 URI，或包含 `%USERNAME%` 的 URI，会按
屏幕上显示的原样交给浏览器或邮件客户端，不会展开成环境变量的值。

设置 `terminal.clickable_bare_names = false` 可以关闭上下文名称。设置
`terminal.clickable_local_targets = false` 可以关闭所有原始本地目标，也包括本地 file URI
和本机路径 OSC 8 链接；网页和邮件 URI 链接不受影响。准确默认值和重载行为见 [配置](Configuration)。

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
