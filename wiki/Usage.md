# Usage / 用法

## English

### Install and first launch

Download the appropriate installer from the
[GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases) page:

- macOS Apple Silicon or Intel: architecture-specific `.dmg`
- Windows x64: `.msi`

On first launch SonicTerm creates `~/.sonicterm/`, writes a starter
`sonicterm.toml`, and seeds editable theme and keymap examples. Runtime state is
kept under this one directory.

### Common actions

These are the bundled defaults. The complete and editable list is on the
[Keybindings](Keybindings) page.

| Action | macOS | Windows |
| --- | --- | --- |
| New tab | `Cmd+T` | `Alt+T` or `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` |
| Split right | `Cmd+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` |
| Focus panes | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` |
| Search | `Cmd+F` | `Alt+F` |
| READONLY / copy mode | `Cmd+[` | `Alt+[` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| Reload config | `Cmd+R` | `Alt+R` |

Windows deliberately uses `Alt` as the application modifier so common
`Ctrl+<letter>` chords continue to reach PowerShell, cmd, readline, and terminal
applications. A few familiar compatibility aliases such as `Ctrl+T`,
`Ctrl+Shift+C`, and `Ctrl+Shift+V` are also bundled.

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

首次启动时，SonicTerm 会创建 `~/.sonicterm/`，生成初始
`sonicterm.toml`，并写入可编辑的主题和 keymap 示例。运行时用户状态都集中在这个目录。

### 常用操作

下表是内置默认值；完整且可编辑的列表见 [快捷键 / Keybindings](Keybindings)。

| 功能 | macOS | Windows |
| --- | --- | --- |
| 新建标签页 | `Cmd+T` | `Alt+T` 或 `Ctrl+T` |
| 关闭当前窗格或标签页 | `Cmd+W` | `Alt+W` |
| 向右分屏 | `Cmd+D` | `Alt+D` |
| 向下分屏 | `Cmd+Shift+D` | `Alt+Shift+D` |
| 切换窗格焦点 | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` |
| 搜索 | `Cmd+F` | `Alt+F` |
| READONLY / 复制模式 | `Cmd+[` | `Alt+[` |
| 广播到当前标签页 | `Cmd+Shift+B` | `Alt+Shift+B` |
| URL 快速选择 | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| 重新加载配置 | `Cmd+R` | `Alt+R` |

Windows 特意使用 `Alt` 作为应用级修饰键，使常见的 `Ctrl+<字母>` 仍可传给
PowerShell、cmd、readline 和终端程序。同时保留 `Ctrl+T`、`Ctrl+Shift+C`、
`Ctrl+Shift+V` 等常见兼容别名。

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
