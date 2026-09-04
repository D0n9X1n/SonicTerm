# Keybindings / 快捷键

## English

### Keymap files

SonicTerm keymaps are TOML files. Bundled files live in `assets/keymaps/`.
Editable copies live in:

```text
~/.sonicterm/keymaps/
```

The first launch seeds `sonicterm-macos.toml`, `sonicterm-windows.toml`, and
`sonicterm-linux.toml`. The active file comes from `keymap` in
`~/.sonicterm/sonicterm.toml`. A logical name checks the user directory before
bundled assets; dots are allowed, so `sonicterm-v1.2` remains a name. The portable
alias `keymap = "user"` selects the editable platform-default file on every OS.

Absolute paths and strings containing `/` or `\` are used directly. So are
Windows drive/UNC paths and names whose suffix is `.toml` (case-insensitive).
Relative explicit paths such as `custom.toml`, `./custom`, and `../custom` are
anchored to SonicTerm's process working directory.

**Edit keymap.toml** opens the platform-default user file. If `keymap` names a
different file, edit that file directly. Run **Reload Config** after saving.
SonicTerm has no keymap file watcher.

### Default shortcuts

The macOS application modifier is `Cmd`. Windows and Linux use `Alt`, which
leaves most `Ctrl+<letter>` input available to shells and terminal applications.
The listed `Alt` chords can therefore replace shell Meta bindings.

| Action | macOS | Windows | Linux |
| --- | --- | --- | --- |
| New tab | `Cmd+T`, `Cmd+Shift+T` | `Alt+T`, `Alt+Shift+T`, `Ctrl+T` | `Alt+T`, `Alt+Shift+T`, `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` | `Alt+W` |
| Next tab | `Cmd+Shift+]`, `Cmd+Right` | `Alt+Shift+]`, `Alt+Right` | `Alt+Shift+]`, `Alt+Right` |
| Previous tab | `Cmd+Shift+[`, `Cmd+Left` | `Alt+Shift+[`, `Alt+Left` | `Alt+Shift+[`, `Alt+Left` |
| Activate tabs 1–8 | `Cmd+1` … `Cmd+8` | `Alt+1` … `Alt+8` | `Alt+1` … `Alt+8` |
| Activate last tab | `Cmd+9` | `Alt+9` | `Alt+9` |
| Split right | `Cmd+D` | `Alt+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` | `Alt+Shift+D` |
| Close pane | `Cmd+Shift+W` | `Alt+Shift+W` | `Alt+Shift+W` |
| Toggle pane zoom | `Cmd+Shift+Z` | `Alt+Shift+Z` | `Alt+Shift+Z` |
| Focus pane left/down/up/right | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Resize pane left/right/up/down | `Cmd+Shift+Arrow` | `Alt+Shift+Arrow` | `Alt+Shift+Arrow` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` | `Alt+Shift+B` |
| Broadcast to all tabs | `Cmd+Ctrl+Shift+B` | `Ctrl+Alt+Shift+B` | `Ctrl+Alt+Shift+B` |
| Enter READONLY mode | `Cmd+[` | `Alt+[` | `Alt+[` |
| Copy selection | `Cmd+C` | `Alt+C`, `Ctrl+Shift+C` | `Alt+C`, `Ctrl+Shift+C` |
| Paste | `Cmd+V` | `Ctrl+Shift+V` | `Alt+V`, `Ctrl+Shift+V` |
| Increase font size | `Cmd+=`, `Cmd+Shift+=`, `Cmd++` | `Alt+=`, `Alt+Shift+=`, `Alt++` | `Alt+=`, `Alt+Shift+=`, `Alt++` |
| Decrease font size | `Cmd+-` | `Alt+-` | `Alt+-` |
| Reset font size to config | `Cmd+0` | `Alt+0` | `Alt+0` |
| New window | `Cmd+N` | `Alt+N` | `Alt+N` |
| Toggle fullscreen | `Cmd+Shift+F`, `Cmd+Enter` | `Alt+Shift+F`, `Alt+Enter`, `F11` | `Alt+Shift+F`, `Alt+Enter`, `F11` |
| Search | `Cmd+F` | `Alt+F` | `Alt+F` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` | `Alt+Shift+P` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` | `Alt+Shift+Space` |
| Scroll one line | `Cmd+Up`, `Cmd+Down` | `Alt+Up`, `Alt+Down` | `Alt+Up`, `Alt+Down` |
| Scroll one page | `Cmd+PageUp`, `Cmd+PageDown` | `Alt+PageUp`, `Alt+PageDown` | `Alt+PageUp`, `Alt+PageDown` |
| Scroll to top or bottom | `Cmd+Home`, `Cmd+End` | `Alt+Home`, `Alt+End` | `Alt+Home`, `Alt+End` |
| Reload config | `Cmd+R`, `Cmd+Shift+R` | `Alt+R`, `Alt+Shift+R` | `Alt+R`, `Alt+Shift+R` |
| Quit from keyboard | Press `Cmd+Q` twice within 5 seconds | — | — |

On macOS, the first `Cmd+Q` displays **Press ⌘Q one more time to quit**. Auto-repeat
does not confirm the quit. The prompt expires after 5 seconds. The native
**Quit SonicTerm** menu item quits immediately.

Windows deliberately does not bind `Alt+V`; that chord continues to the PTY.
Use `Ctrl+Shift+V` for paste. Linux binds both `Alt+V` and `Ctrl+Shift+V`.

### TOML syntax

A keymap needs a `[meta]` table and zero or more `[[binding]]` tables:

```toml
[meta]
name = "my-keymap"
version = "1.0"

[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+1"
action = { activate_tab = 0 }
```

Key names are lower case. Modifiers are normalized in this order:

```text
super+ctrl+alt+shift+key
```

| Name | Meaning |
| --- | --- |
| `super` | Command on macOS; the Super/Windows key on Windows and Linux |
| `ctrl` | Control |
| `alt` | Option on macOS; Alt on Windows and Linux |
| `shift` | Shift |

Chord lookup is case-insensitive. If the same chord appears more than once, the
first matching binding wins. Keys with no binding go to the terminal. One
Windows exception is preserved: an `alt+v` binding to `paste_from_clipboard`
still passes through to the terminal.

### Actions

Actions without arguments use a string. The active action names are:

| Group | Actions |
| --- | --- |
| Tabs | `new_tab`, `close_tab`, `close_active_pane_or_tab`, `next_tab`, `prev_tab`, `activate_last_tab` |
| Panes | `split_right`, `split_down`, `close_pane`, `toggle_pane_zoom`, `resize_pane_left`, `resize_pane_right`, `resize_pane_up`, `resize_pane_down` |
| Clipboard and navigation | `copy_to_clipboard`, `paste_from_clipboard`, `enter_copy_mode`, `enter_quick_select` |
| Font | `increase_font_size`, `decrease_font_size`, `reset_font_size`, `increase_font_weight`, `decrease_font_weight`, `reset_font_weight`, `save_current_settings` |
| UI | `toggle_tab_bar`, `rename_tab`, `update_tab_color`, `open_search`, `open_command_palette` |
| Window and app | `new_window`, `move_tab_to_new_window`, `toggle_fullscreen`, `quit_app` |
| Files and maintenance | `edit_config_file`, `open_keymap_file`, `reload_config`, `check_for_updates` |
| Shell navigation | `scroll_to_prev_prompt`, `scroll_to_next_prompt` |

Parameterized actions use inline TOML tables:

```toml
[[binding]]
keys = "super+3"
action = { activate_tab = 2 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+right"
action = { resize_pane = { dir = "right", amount = 10 } }

[[binding]]
keys = "super+pageup"
action = { scroll = "page_up" }

[[binding]]
keys = "super+shift+b"
action = { toggle_broadcast = { scope = "tab" } }

[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

`activate_tab` is zero-based. Directions are `left`, `right`, `up`, and `down`.
Each named resize action moves the divider by 5%. `resize_pane.amount` repeats
that 5% step; `0` does nothing. Scroll values are
`line_up`, `line_down`, `page_up`, `page_down`, `to_top`, and `to_bottom`.
Broadcast scopes are `tab` and `all_tabs`.

Font-size actions step by `1` point and clamp the live size to `8..=48`.
Font-weight actions step by `0.25` and clamp to `0.5..=5.0`. Reset returns to
the last loaded or saved config value. The weight and save actions have no
default shortcut, but they are available in the command palette.

### Selection and explicit copy

A normal drag selects cells. A double-click selects a word, and a triple-click
selects a line. Continuing to drag after a double- or triple-click extends by
whole words or lines. Releasing a drag does not copy automatically.

When a terminal application enables mouse tracking, an unmodified left-button
gesture belongs to that application. Start with `Shift+Left` to give the whole
gesture to SonicTerm and select local text instead. The owner is fixed on the
button press, so changing Shift or the application’s tracking mode during the
drag does not transfer it.

`copy_to_clipboard` copies a valid explicit selection. On the primary screen,
the selection remains highlighted after a successful copy. On the alternate
screen, a successful clipboard write clears the explicit selection and redraws
the window immediately. A clipboard failure keeps that valid selection so the
copy can be retried. If selected cells changed before the copy, SonicTerm clears
the stale selection and leaves the clipboard unchanged.

### READONLY and quick select

`enter_copy_mode` opens READONLY mode at the terminal cursor. It blocks all PTY
input and does not create a selection. These local controls remain active:

| Key | READONLY action |
| --- | --- |
| `Left` / `h` | Move one cell left |
| `Down` / `j` | Move one row down |
| `Up` / `k` | Move one row up |
| `Right` / `l` | Move one cell right |
| `w` / `b` | Move to the next / previous word |
| `0` / `$` | Move to the start / end of the row |
| `g` / `G` | Move to the top / bottom |
| `Escape` | Exit READONLY mode |

READONLY also permits keymap actions that switch or activate tabs, focus panes,
open search, check for updates, or save current font settings. All other bound
actions are consumed without running and without reaching the PTY. Search text
is still editable.

`enter_quick_select` labels up to 26 URLs on the current screen with `a` through
`z`. Press a label to copy that URL and close the overlay. Press `Escape` to
cancel.

### App text fields

Search, command-palette filtering, and tab renaming support the same single-line
editing controls. These exact chords work only while an app text field owns
input:

| Key | Action |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | Move to start / end |
| `Ctrl+B` / `Ctrl+F` | Move left / right by one Unicode character |
| `Ctrl+H` / `Ctrl+D` | Delete backward / forward by one Unicode character |
| `Ctrl+W` | Delete left whitespace, then the previous non-whitespace run |
| `Ctrl+U` / `Ctrl+K` | Delete from start to caret / caret to end |
| `Left`, `Right`, `Home`, `End`, `Delete` | Standard caret movement and forward deletion |

Adding Shift, Alt, or Super makes a different chord. When no SonicTerm text
field is active, `Ctrl+<letter>` continues to the PTY.

### Load failures

At startup, invalid TOML or a missing `[meta]` table falls back to the bundled
platform keymap. During reload, the current in-memory keymap remains active
instead. A structurally valid keymap handles bad actions per binding: SonicTerm
logs a warning, skips that binding, and keeps the rest. A successful reload also
updates command-palette shortcut hints.

## 中文

### Keymap 文件

SonicTerm 的 keymap 是 TOML 文件。内置文件位于 `assets/keymaps/`。用户可编辑
副本位于：

```text
~/.sonicterm/keymaps/
```

首次启动会写入 `sonicterm-macos.toml`、`sonicterm-windows.toml` 和
`sonicterm-linux.toml`。当前文件由 `~/.sonicterm/sonicterm.toml` 中的
`keymap` 决定。逻辑名称会先查找用户目录，再查找内置资产；名称可以包含点，
所以 `sonicterm-v1.2` 仍按名称处理。可移植别名 `keymap = "user"` 在每个平台上
都选择该平台可编辑的默认 keymap 文件。

绝对路径以及包含 `/` 或 `\` 的字符串会直接使用；Windows 盘符/UNC 路径和以
`.toml` 结尾（不区分大小写）的名称也按路径处理。`custom.toml`、`./custom`、
`../custom` 等相对显式路径以 SonicTerm 进程的工作目录为基准。

**Edit keymap.toml** 打开当前平台的默认用户文件。如果 `keymap` 指向其它文件，
请直接编辑那个文件。保存后执行 **Reload Config**。SonicTerm 没有 keymap 文件 watcher。

### 默认快捷键

macOS 的应用修饰键是 `Cmd`。Windows 和 Linux 使用 `Alt`，这样大多数
`Ctrl+<字母>` 可以继续交给 shell 和终端程序。表中的 `Alt` 快捷键也会占用 shell
原有的 Meta 快捷键。

| 功能 | macOS | Windows | Linux |
| --- | --- | --- | --- |
| 新建标签页 | `Cmd+T`、`Cmd+Shift+T` | `Alt+T`、`Alt+Shift+T`、`Ctrl+T` | `Alt+T`、`Alt+Shift+T`、`Ctrl+T` |
| 关闭当前 pane 或标签页 | `Cmd+W` | `Alt+W` | `Alt+W` |
| 下一个标签页 | `Cmd+Shift+]`、`Cmd+Right` | `Alt+Shift+]`、`Alt+Right` | `Alt+Shift+]`、`Alt+Right` |
| 上一个标签页 | `Cmd+Shift+[`、`Cmd+Left` | `Alt+Shift+[`、`Alt+Left` | `Alt+Shift+[`、`Alt+Left` |
| 切换到标签页 1–8 | `Cmd+1` … `Cmd+8` | `Alt+1` … `Alt+8` | `Alt+1` … `Alt+8` |
| 切换到最后一个标签页 | `Cmd+9` | `Alt+9` | `Alt+9` |
| 向右分屏 | `Cmd+D` | `Alt+D` | `Alt+D` |
| 向下分屏 | `Cmd+Shift+D` | `Alt+Shift+D` | `Alt+Shift+D` |
| 关闭 pane | `Cmd+Shift+W` | `Alt+Shift+W` | `Alt+Shift+W` |
| 切换 pane zoom | `Cmd+Shift+Z` | `Alt+Shift+Z` | `Alt+Shift+Z` |
| 向左/下/上/右切换 pane | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 向左/右/上/下调整 pane | `Cmd+Shift+方向键` | `Alt+Shift+方向键` | `Alt+Shift+方向键` |
| 广播到当前标签页 | `Cmd+Shift+B` | `Alt+Shift+B` | `Alt+Shift+B` |
| 广播到所有标签页 | `Cmd+Ctrl+Shift+B` | `Ctrl+Alt+Shift+B` | `Ctrl+Alt+Shift+B` |
| 进入 READONLY 模式 | `Cmd+[` | `Alt+[` | `Alt+[` |
| 复制选区 | `Cmd+C` | `Alt+C`、`Ctrl+Shift+C` | `Alt+C`、`Ctrl+Shift+C` |
| 粘贴 | `Cmd+V` | `Ctrl+Shift+V` | `Alt+V`、`Ctrl+Shift+V` |
| 增大字号 | `Cmd+=`、`Cmd+Shift+=`、`Cmd++` | `Alt+=`、`Alt+Shift+=`、`Alt++` | `Alt+=`、`Alt+Shift+=`、`Alt++` |
| 减小字号 | `Cmd+-` | `Alt+-` | `Alt+-` |
| 把字号重置为配置值 | `Cmd+0` | `Alt+0` | `Alt+0` |
| 新建窗口 | `Cmd+N` | `Alt+N` | `Alt+N` |
| 切换全屏 | `Cmd+Shift+F`、`Cmd+Enter` | `Alt+Shift+F`、`Alt+Enter`、`F11` | `Alt+Shift+F`、`Alt+Enter`、`F11` |
| 搜索 | `Cmd+F` | `Alt+F` | `Alt+F` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` | `Alt+Shift+P` |
| 快速选择 URL | `Cmd+Shift+Space` | `Alt+Shift+Space` | `Alt+Shift+Space` |
| 滚动一行 | `Cmd+Up`、`Cmd+Down` | `Alt+Up`、`Alt+Down` | `Alt+Up`、`Alt+Down` |
| 滚动一页 | `Cmd+PageUp`、`Cmd+PageDown` | `Alt+PageUp`、`Alt+PageDown` | `Alt+PageUp`、`Alt+PageDown` |
| 滚动到顶部或底部 | `Cmd+Home`、`Cmd+End` | `Alt+Home`、`Alt+End` | `Alt+Home`、`Alt+End` |
| 重载配置 | `Cmd+R`、`Cmd+Shift+R` | `Alt+R`、`Alt+Shift+R` | `Alt+R`、`Alt+Shift+R` |
| 用键盘退出 | 5 秒内按两次 `Cmd+Q` | — | — |

macOS 中，第一次按 `Cmd+Q` 会显示 **Press ⌘Q one more time to quit**。
按键自动重复不会确认退出。提示会在 5 秒后失效。原生菜单中的
**Quit SonicTerm** 会立即退出。

Windows 特意不绑定 `Alt+V`；该组合键会继续发送给 PTY。请使用
`Ctrl+Shift+V` 粘贴。Linux 同时绑定 `Alt+V` 和 `Ctrl+Shift+V`。

### TOML 格式

Keymap 必须有 `[meta]`，并可以包含任意数量的 `[[binding]]`：

```toml
[meta]
name = "my-keymap"
version = "1.0"

[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+1"
action = { activate_tab = 0 }
```

按键名使用小写。修饰键按以下顺序规范化：

```text
super+ctrl+alt+shift+key
```

| 名称 | 含义 |
| --- | --- |
| `super` | macOS 上的 Command；Windows 和 Linux 上的 Super/Windows 键 |
| `ctrl` | Control |
| `alt` | macOS 上的 Option；Windows 和 Linux 上的 Alt |
| `shift` | Shift |

快捷键匹配不区分大小写。同一组合键出现多次时，第一个匹配的 binding 生效。
没有 binding 的按键会发送给终端。Windows 有一个保留例外：即使把 `alt+v`
绑定到 `paste_from_clipboard`，它仍会发送给终端。

### Action

不带参数的 action 使用字符串。当前有效名称如下：

| 分组 | Action |
| --- | --- |
| 标签页 | `new_tab`、`close_tab`、`close_active_pane_or_tab`、`next_tab`、`prev_tab`、`activate_last_tab` |
| Pane | `split_right`、`split_down`、`close_pane`、`toggle_pane_zoom`、`resize_pane_left`、`resize_pane_right`、`resize_pane_up`、`resize_pane_down` |
| 剪贴板与导航 | `copy_to_clipboard`、`paste_from_clipboard`、`enter_copy_mode`、`enter_quick_select` |
| 字体 | `increase_font_size`、`decrease_font_size`、`reset_font_size`、`increase_font_weight`、`decrease_font_weight`、`reset_font_weight`、`save_current_settings` |
| UI | `toggle_tab_bar`、`rename_tab`、`update_tab_color`、`open_search`、`open_command_palette` |
| 窗口与应用 | `new_window`、`move_tab_to_new_window`、`toggle_fullscreen`、`quit_app` |
| 文件与维护 | `edit_config_file`、`open_keymap_file`、`reload_config`、`check_for_updates` |
| Shell 导航 | `scroll_to_prev_prompt`、`scroll_to_next_prompt` |

带参数的 action 使用 inline TOML table：

```toml
[[binding]]
keys = "super+3"
action = { activate_tab = 2 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+right"
action = { resize_pane = { dir = "right", amount = 10 } }

[[binding]]
keys = "super+pageup"
action = { scroll = "page_up" }

[[binding]]
keys = "super+shift+b"
action = { toggle_broadcast = { scope = "tab" } }

[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

`activate_tab` 从 `0` 开始。方向值是 `left`、`right`、`up`、`down`。
每个命名 resize action 会把 divider 移动 5%。`resize_pane.amount` 表示重复这个
5% step 的次数；`0` 不执行调整。滚动值是 `line_up`、
`line_down`、`page_up`、`page_down`、`to_top`、`to_bottom`。广播范围是
`tab` 和 `all_tabs`。

字号 action 每次调整 `1` point，并把当前字号限制在 `8..=48`。字重 action
每次调整 `0.25`，范围是 `0.5..=5.0`。Reset 会回到最近加载或保存的配置值。
字重与保存 action 默认没有快捷键，但可从命令面板执行。

### 选区与显式复制

普通拖动按 cell 选择。双击选择单词，三击选择整行。双击或三击后继续拖动时，
会按完整单词或整行扩展。松开鼠标不会自动复制。

终端程序启用 mouse tracking 后，未加修饰键的左键 gesture 归该程序处理。
从 `Shift+左键` 开始可以把整个 gesture 交给 SonicTerm，在本地选择文字。
Gesture owner 在按下鼠标时确定；拖动过程中改变 Shift 状态或程序的 tracking mode
不会转移 owner。

`copy_to_clipboard` 会复制仍然有效的显式选区。在 primary screen 中，复制成功后
选区仍保持高亮。在 alternate screen 中，写入剪贴板成功后会清除显式选区并立即
重绘窗口。如果剪贴板写入失败，有效选区会保留，用户可以重试。如果复制前所选
cell 已经变化，SonicTerm 会清除过期选区，并保持剪贴板不变。

### READONLY 与快速选择

`enter_copy_mode` 会在终端光标处进入 READONLY 模式。它阻止所有 PTY 输入，
也不会创建选区。以下本地控制仍可使用：

| 按键 | READONLY 行为 |
| --- | --- |
| `Left` / `h` | 向左移动一个 cell |
| `Down` / `j` | 向下移动一行 |
| `Up` / `k` | 向上移动一行 |
| `Right` / `l` | 向右移动一个 cell |
| `w` / `b` | 移到下一个 / 上一个单词 |
| `0` / `$` | 移到行首 / 行尾 |
| `g` / `G` | 移到顶部 / 底部 |
| `Escape` | 退出 READONLY 模式 |

READONLY 还允许执行切换或激活标签页、切换 pane 焦点、打开搜索、检查更新、保存
当前字体设置的 keymap action。其它已绑定 action 会被直接拦截，不执行，也不会发送
给 PTY。搜索框仍可编辑。

`enter_quick_select` 会用 `a` 到 `z` 标记当前屏幕上最多 26 个 URL。按对应字母
可复制 URL 并关闭 overlay。按 `Escape` 取消。

### 应用文本框

搜索、命令面板筛选和标签页重命名使用同一套单行编辑控制。只有应用文本框接管输入时，
以下精确组合键才生效：

| 按键 | 行为 |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | 移到开头 / 结尾 |
| `Ctrl+B` / `Ctrl+F` | 向左 / 向右移动一个 Unicode 字符 |
| `Ctrl+H` / `Ctrl+D` | 向后 / 向前删除一个 Unicode 字符 |
| `Ctrl+W` | 删除左侧空白，再删除前一个连续非空白片段 |
| `Ctrl+U` / `Ctrl+K` | 删除开头到光标 / 光标到结尾 |
| `Left`、`Right`、`Home`、`End`、`Delete` | 标准光标移动和向前删除 |

额外按下 Shift、Alt 或 Super 会形成不同组合键。没有 SonicTerm 文本框接管输入时，
`Ctrl+<字母>` 会继续发送给 PTY。

### 加载失败

启动时，如果 TOML 无效或缺少 `[meta]`，SonicTerm 会回退到当前平台的内置 keymap。
重载时遇到同样错误，会继续使用内存中的当前 keymap。结构正确的 keymap 会按 binding
处理错误 action：SonicTerm 记录 warning，只跳过该 binding，并保留其它 binding。
重载成功后，命令面板中的快捷键提示也会更新。
