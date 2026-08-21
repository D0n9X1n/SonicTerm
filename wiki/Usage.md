# Usage / 用法

## English

### Install and first launch

Download the appropriate installer from the
[GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases) page:

- macOS Apple Silicon or Intel: architecture-specific `.dmg`
- Windows x64: `.msi`
- Linux x86_64 with glibc 2.35 or newer: `.deb` or portable `.tar.gz`

Install the Debian package with `sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb`.
For the portable archive, extract it and run the `sonicterm` binary from its
payload directory so the adjacent `assets/` tree remains available. Both X11
and Wayland are supported.

On first launch SonicTerm creates `~/.sonicterm/`, writes a starter
`sonicterm.toml`, and seeds editable theme and keymap examples. Runtime state is
kept under this one directory.

### Common actions

These are the bundled defaults. The complete and editable list is on the
[Keybindings](Keybindings) page.

| Action | macOS | Windows | Linux |
| --- | --- | --- | --- |
| New tab | `Cmd+T` | `Alt+T` or `Ctrl+T` | `Alt+T` or `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` | `Alt+W` |
| Split right | `Cmd+D` | `Alt+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` | `Alt+Shift+D` |
| Focus panes | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` | `Alt+Shift+P` |
| Search | `Cmd+F` | `Alt+F` | `Alt+F` |
| READONLY / copy mode | `Cmd+[` | `Alt+[` | `Alt+[` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` | `Alt+Shift+B` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` | `Alt+Shift+Space` |
| Reload config | `Cmd+R` | `Alt+R` | `Alt+R` |

Windows and Linux deliberately use `Alt` as the application modifier so common
`Ctrl+<letter>` chords continue to reach PowerShell, cmd, readline, and terminal
applications. A few familiar compatibility aliases such as `Ctrl+T`,
`Ctrl+Shift+C`, and `Ctrl+Shift+V` are also bundled.

### Opening local filesystem targets

Hold `Cmd` on macOS or `Ctrl` on Windows/Linux while pointing at an eligible
local filesystem target printed in terminal output. Once the background
openability check finishes, the target receives the active underline and
pointer. Clicking opens the target itself: directories open in the platform file
manager, while ordinary files open in their default associated application.
Existing OSC 8 links and `http://`, `https://`, `mailto:`, and `file://` URLs keep
their existing behavior and take precedence over filesystem detection.

SonicTerm recognizes native absolute paths such as `/usr/local/etc`,
`C:/Users/dotan`, and `C:\\Users\\dotan`; current-home paths such as `~/notes`
(and `~\\notes` on Windows); separator-relative paths such as `src/main.rs`
(and `src\\main.rs` on Windows); and explicit dot-relative forms such as
`./file`, `../file`, and `../../file`. These forms may contain ordinary spaces,
for example `/tmp/My Folder`, `C:\\Program Files\\SonicTerm`, or
`./My Folder/file.txt`. Current-home paths resolve against the local user's
native absolute home directory. Separator-relative paths, dot-relative paths,
and contextual names such as `sonicterm`, `.DS_Store`, or `My Folder` resolve
only when that exact pane has reported a trustworthy absolute local working
directory through OSC 7. A missing, malformed, or foreign-host OSC 7 value fails
closed; SonicTerm never substitutes its process working directory or another
pane.

Bare-name detection deliberately does not parse `ls` columns. Any row-local
candidate can become clickable when an identically named eligible entry exists
in the trusted pane CWD—even text spanning ordinary spaces. SonicTerm asks the
bounded background probe to test at most 37 candidates, each spanning no more
than eight non-space parts, and uses the longest unambiguous openable span
containing the pointed cell. A blocked existing span or equal-length ambiguity
fails closed rather than falling back to a shorter name. Quoted or escaped names
and `ls -F` decorations (`*`, `@`, `=`, `|`)
remain inert unless OSC 8 provides their identity. Set
`terminal.clickable_bare_names = false` to disable this contextual behavior, or
`terminal.clickable_local_targets = false` to disable all raw local-target
activation; neither switch changes URI or OSC 8 handling.

Openability checks and native opens run on bounded workers, never on the window
thread. Missing, inaccessible, stale, symlink/reparse-point, socket, device, and
other special entries stay ordinary text. A result cannot authorize a changed
row, pane, CWD, viewport, target kind, or modifier state. Launcher, executable,
script, shortcut, and installer classes also remain inert: macOS blocks app
bundles, executable mode, common script suffixes, shebang/ELF/PE content, and
Mach-O/fat-binary magic; Windows blocks PATHEXT and known launcher classes plus
ADS/trailing-dot/trailing-space names, and Linux blocks
`.desktop`, `.AppImage`, ELF, shebang, and `MZ` content. Named-user home
syntax (`~user`), environment-variable expansion, UNC/network/WSL/remote paths,
editor `:line:column` suffixes, wrapped multi-row paths, and tokens containing
wide or combining cells are not clickable.

macOS revalidates immediately before `/usr/bin/open -- <target>`, but
LaunchServices can still observe a later pathname replacement or show quarantine
prompts. Windows revalidates before a COM-initialized `ShellExecuteExW` call and
never invokes a shell. Linux prefers an already-open file descriptor through the
desktop portal; when portal infrastructure is unavailable, a fixed
`/usr/bin/xdg-open` or `/bin/xdg-open` fallback revalidates first but retains the
usual pathname race between that check and the external opener.

### Opening script files

Installed builds can be selected as a handler for runnable script files. On
macOS, choose SonicTerm from Finder's **Open With** menu for `.sh`, `.command`,
or `.tool`. On Windows, choose SonicTerm in **Open with** or **Default apps**
for `.ps1`, `.cmd`, `.bat`, or `.sh`. Installation only makes SonicTerm a
candidate; it does not replace the current default.

Opening a supported file creates a tab in the script's parent directory and
places a safely quoted command at the prompt **without pressing Enter**. Review,
edit, or clear it before submitting. Unsupported shell/script combinations and
unsafe paths still open the tab but show a warning instead of inserting a
command. Windows opens one new SonicTerm process per association invocation;
macOS sends later files to the running app and appends tabs.

This is a draft-input feature, not a sandbox. Shell startup files run before the
prompt and can read or evaluate PTY input themselves. A profile that performs a
fixed-length read followed by `eval` can execute or swallow the draft without
Enter; users with such startup code should not select SonicTerm as a script
handler. SonicTerm itself never invokes the script or an interpreter for the
open action and appends no submit/control byte.

### Tabs, panes, and windows

Each pane owns a separate child PTY. A tab can contain a split-pane tree, and
tabs can be reordered or dragged between SonicTerm windows. Dragging a tab out
creates or reuses a pre-warmed child window; the live pane and PTY move with the
tab rather than restarting the shell.

Closing a split closes that pane's PTY. Closing the last pane in a tab closes the
tab. The `quit_on_last_window_close` configuration key controls whether the
macOS process remains available from the Dock after its final window closes;
non-macOS platforms exit when no windows remain.

### READONLY mode

READONLY mode is intended for safe scrollback inspection. It blocks ordinary
terminal input while retaining a small navigation whitelist:

- switch or activate tabs;
- move focus between panes;
- open and edit terminal search;
- check for updates.

When search is open, typed text edits the search query and is never forwarded to
the PTY. Exit READONLY mode before entering shell commands.

### Broadcast input

Broadcast mode mirrors input from one source pane to either the current tab or
all tabs. Receiver panes are visually marked. Use it carefully: every receiving
PTY gets the same bytes, while the source pane is excluded from the receiver
set to avoid duplicate input.

### Configuration and troubleshooting

- Edit preferences: [Configuration](Configuration)
- Customize shortcuts: [Keybindings](Keybindings)
- Customize colors: [Themes](Themes)
- Locate diagnostics: [Logging](Logging)

## 中文

### 安装与首次启动

从 [GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases) 页面下载对应安装包：

- macOS Apple Silicon 或 Intel：对应架构的 `.dmg`
- Windows x64：`.msi`
- Linux x86_64（glibc 2.35 或更高版本）：`.deb` 或便携 `.tar.gz`

使用 `sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb` 安装 Debian 包。
使用便携归档时，请先解压，再从 payload 目录运行 `sonicterm`，以保留二进制旁的
`assets/` 目录。X11 与 Wayland 均受支持。

首次启动时，SonicTerm 会创建 `~/.sonicterm/`，生成初始
`sonicterm.toml`，并写入可编辑的主题和 keymap 示例。运行时用户状态都集中在这个目录。

### 常用操作

下表是内置默认值；完整且可编辑的列表见 [快捷键 / Keybindings](Keybindings)。

| 功能 | macOS | Windows | Linux |
| --- | --- | --- | --- |
| 新建标签页 | `Cmd+T` | `Alt+T` 或 `Ctrl+T` | `Alt+T` 或 `Ctrl+T` |
| 关闭当前窗格或标签页 | `Cmd+W` | `Alt+W` | `Alt+W` |
| 向右分屏 | `Cmd+D` | `Alt+D` | `Alt+D` |
| 向下分屏 | `Cmd+Shift+D` | `Alt+Shift+D` | `Alt+Shift+D` |
| 切换窗格焦点 | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` | `Alt+Shift+P` |
| 搜索 | `Cmd+F` | `Alt+F` | `Alt+F` |
| READONLY / 复制模式 | `Cmd+[` | `Alt+[` | `Alt+[` |
| 广播到当前标签页 | `Cmd+Shift+B` | `Alt+Shift+B` | `Alt+Shift+B` |
| URL 快速选择 | `Cmd+Shift+Space` | `Alt+Shift+Space` | `Alt+Shift+Space` |
| 重新加载配置 | `Cmd+R` | `Alt+R` | `Alt+R` |

Windows 与 Linux 特意使用 `Alt` 作为应用级修饰键，使常见的 `Ctrl+<字母>` 仍可传给
PowerShell、cmd、readline 和终端程序。同时保留 `Ctrl+T`、`Ctrl+Shift+C`、
`Ctrl+Shift+V` 等常见兼容别名。

### 打开本地文件系统目标

鼠标指向终端输出中的合格本地文件系统目标时，在 macOS 按住 `Cmd`，在 Windows/Linux
按住 `Ctrl`。后台可打开性检查完成后，目标会显示 active underline 与 pointer。点击会
打开目标本身：目录在平台文件管理器中打开，普通文件由默认关联应用打开。现有 OSC 8 link，
以及 `http://`、`https://`、`mailto:`、`file://` URL 的行为保持不变，并且优先于
文件系统检测。

SonicTerm 可识别 `/usr/local/etc`、`C:/Users/dotan`、`C:\\Users\\dotan`
等原生绝对路径，`~/notes`（Windows 也支持 `~\\notes`）等当前用户主目录路径，
`src/main.rs`（Windows 也支持 `src\\main.rs`）等带分隔符的相对路径，以及 `./file`、
`../file`、`../../file` 等显式点相对路径。这些形式可以包含普通空格，例如
`/tmp/My Folder`、`C:\\Program Files\\SonicTerm` 或 `./My Folder/file.txt`。
当前用户主目录路径按本机绝对主目录解析。带分隔符的相对路径、点相对路径，以及
`sonicterm`、`.DS_Store`、`My Folder` 这类 contextual 名称，只有在该准确 pane 通过
OSC 7 报告了可信本机绝对工作目录时才会解析。缺失、格式错误或来自远端 host 的 OSC 7
值都会 fail closed；SonicTerm 不会改用进程工作目录或其他 pane。

裸名称检测不会解析 `ls` 的列。只要可信 pane CWD 中存在同名合格条目，行内候选（包括跨越
普通空格的文本）就可能变为可点击目标。SonicTerm 由有界后台 probe 检查最多 37 个候选，
每个候选最多跨越 8 个非空格部分，并选取包含鼠标 cell 的最长、无歧义且可打开 span。若已有
较长 span 被阻止，或同长度候选有歧义，则 fail closed，不会退回较短名称。quoted/escaped
名称和 `ls -F` 装饰（`*`、`@`、
`=`、`|`）保持 inert，除非 OSC 8 提供明确 identity。设置
`terminal.clickable_bare_names = false` 可关闭这种 contextual 行为；设置
`terminal.clickable_local_targets = false` 可关闭全部原始本地目标 activation。两者都不影响
URI 或 OSC 8。

可打开性检查与原生打开动作都在有界 worker 上运行，不会阻塞窗口线程。不存在、无法访问、
过期、symlink/reparse point、socket、device 及其它特殊条目保持普通文本；检查结果不能授权
已变化的行、pane、CWD、viewport、目标类型或修饰键状态。launcher、executable、script、
shortcut 和 installer 类也保持 inert：macOS 阻止 app bundle、可执行 mode、常见 script
后缀、shebang/ELF/PE 内容，以及 Mach-O/fat-binary magic；Windows 阻止 PATHEXT、已知
launcher 类、ADS、末尾点和末尾空格；Linux 阻止 `.desktop`、
`.AppImage`、ELF、shebang 与 `MZ` 内容。命名用户主目录语法（`~user`）、环境变量展开、
UNC/network/WSL/远端路径、editor `:line:column` 后缀、跨行路径，以及含宽字符/组合
cell 的 token 均不可点击。

macOS 在调用 `/usr/bin/open -- <target>` 前立即重新验证，但 LaunchServices 仍可能观察到
之后发生的路径替换，也可能显示 quarantine prompt。Windows 在 COM-initialized
`ShellExecuteExW` 前重新验证，且永不调用 shell。Linux 优先把已打开的 file descriptor
交给 desktop portal；portal 基础设施不可用时，固定的 `/usr/bin/xdg-open` 或
`/bin/xdg-open` fallback 会先重新验证，但检查与外部 opener 之间仍保留通常的 pathname race。

### 打开脚本文件

安装后的 SonicTerm 可被用户选作可执行脚本的处理程序。macOS 可在 Finder 的
**打开方式**中为 `.sh`、`.command` 或 `.tool` 选择 SonicTerm；Windows 可在
**打开方式**或**默认应用**中为 `.ps1`、`.cmd`、`.bat`、`.sh` 选择 SonicTerm。安装只声明候选资格，
不会替换当前默认程序。

打开受支持文件时，会在脚本父目录创建标签页，并把安全引用后的命令放到提示符中，
**不会自动按下 Enter**。提交前可以检查、修改或清空。shell 与脚本类型不兼容、或路径
无法安全表示时，标签页仍会打开，但只显示 warning，不会填入命令。Windows 每次文件
关联调用会启动一个新的 SonicTerm 进程；macOS 会把后续文件发送给运行中的实例并追加标签页。

该功能提供的是输入草稿，不是沙箱。shell 启动文件先于提示符运行，也可以自行读取或执行
PTY 输入；例如定长 `read` 后接 `eval` 的配置，即使没有 Enter，也可能执行或吞掉草稿。
有这类启动配置的用户不应把 SonicTerm 设为脚本处理程序。对于“打开”动作，SonicTerm
自身不会调用脚本或解释器，也不会追加提交/控制字节。

### 标签页、窗格和窗口

每个窗格拥有独立的子 PTY。一个标签页内部可以是一棵分屏树；标签页可以排序，也可以在
SonicTerm 窗口之间拖动。把标签页拖出时，程序会创建或复用预热的子窗口；现有窗格和 PTY
会随标签页移动，不会重新启动 shell。

关闭分屏会关闭对应窗格的 PTY；关闭标签页中的最后一个窗格会关闭该标签页。
`quit_on_last_window_close` 决定 macOS 最后一个窗口关闭后进程是否仍留在 Dock 中；
非 macOS 平台在没有窗口时会退出。

### READONLY 模式

READONLY 模式用于安全查看回滚缓冲。它阻止普通输入进入终端，只保留少量导航操作：

- 切换或激活标签页；
- 在窗格之间移动焦点；
- 打开并编辑终端搜索；
- 检查更新。

搜索框打开时，键盘输入只修改搜索词，不会发送给 PTY。需要输入 shell 命令前，请先退出
READONLY 模式。

### 广播输入

广播模式会把源窗格的输入镜像到当前标签页或全部标签页中的接收窗格，并对接收窗格显示
醒目标记。请谨慎使用：每个接收 PTY 都会收到相同字节；源窗格本身不会加入接收集合，
从而避免重复输入。

### 配置与排障

- 修改偏好：[配置 / Configuration](Configuration)
- 自定义快捷键：[快捷键 / Keybindings](Keybindings)
- 自定义颜色：[主题 / Themes](Themes)
- 查找诊断信息：[日志 / Logging](Logging)
