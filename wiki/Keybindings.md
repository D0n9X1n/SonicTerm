# Keybindings / 快捷键

## English

SonicTerm keymaps are TOML files. Bundled defaults live in `assets/keymaps/`,
and editable user copies live in:

```text
~/.sonicterm/keymaps/
├── sonicterm-macos.toml
├── sonicterm-windows.toml
└── sonicterm-linux.toml
```

The active keymap is selected from `~/.sonicterm/sonicterm.toml`:

```toml
keymap = "sonicterm-macos"
```

You can also point `keymap` at any TOML file path.

### Default shortcuts

`Cmd` is the SonicTerm app modifier on macOS; `Alt` is the app modifier on
Windows. On Windows, `Ctrl+<letter>` is left to the shell (Ctrl+C = SIGINT,
Ctrl+R = history search, Ctrl+W = kill word, …), so a few terminal-standard
aliases (`Ctrl+T`, `Ctrl+Shift+C`, `Ctrl+Shift+V`) are also bound.

| Action | macOS | Windows |
| --- | --- | --- |
| New tab | `Cmd+T` (`Cmd+Shift+T`) | `Alt+T` / `Ctrl+T` (`Alt+Shift+T`) |
| Close pane or tab | `Cmd+W` | `Alt+W` |
| Next tab | `Cmd+Shift+]` / `Cmd+Right` | `Alt+Shift+]` / `Alt+Right` |
| Previous tab | `Cmd+Shift+[` / `Cmd+Left` | `Alt+Shift+[` / `Alt+Left` |
| Activate tab 1–8 | `Cmd+1` … `Cmd+8` | `Alt+1` … `Alt+8` |
| Activate last tab | `Cmd+9` | `Alt+9` |
| Split right | `Cmd+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` |
| Close pane | `Cmd+Shift+W` | `Alt+Shift+W` |
| Zoom pane | `Cmd+Shift+Z` | `Alt+Shift+Z` |
| Focus pane (left/down/up/right) | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Resize pane (left/right/up/down) | `Cmd+Shift+Arrow` | `Alt+Shift+Arrow` |
| Broadcast to tab | `Cmd+Shift+B` | `Alt+Shift+B` |
| Broadcast to all tabs | `Cmd+Ctrl+Shift+B` | `Ctrl+Alt+Shift+B` |
| READONLY / copy mode | `Cmd+[` | `Alt+[` |
| Copy | `Cmd+C` | `Alt+C` / `Ctrl+Shift+C` |
| Paste | `Cmd+V` | `Ctrl+Shift+V` |
| Increase font size | `Cmd+=` / `Cmd++` | `Alt+=` / `Alt++` |
| Decrease font size | `Cmd+-` | `Alt+-` |
| Reset font size (to configured `size`) | `Cmd+0` | `Alt+0` |
| New window | `Cmd+N` | `Alt+N` |
| Quit app (confirm) | `Cmd+Q`, then `Cmd+Q` again within 5s | — |
| Toggle fullscreen | `Cmd+Enter` / `Cmd+Shift+F` | `Alt+Enter` / `Alt+Shift+F` / `F11` |
| Search | `Cmd+F` | `Alt+F` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` |
| Quick select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| Scroll line up/down | `Cmd+Up` / `Cmd+Down` | `Alt+Up` / `Alt+Down` |
| Scroll page up/down | `Cmd+PageUp` / `Cmd+PageDown` | `Alt+PageUp` / `Alt+PageDown` |
| Scroll to top/bottom | `Cmd+Home` / `Cmd+End` | `Alt+Home` / `Alt+End` |
| Reload config | `Cmd+R` / `Cmd+Shift+R` | `Alt+R` / `Alt+Shift+R` |

The action model also supports prompt navigation (`scroll_to_prev_prompt`,
`scroll_to_next_prompt`), theme application, update checks, and an experimental
SSH-pane action. Those are not all bound by default; SSH transport is optional
and its live GUI session is not yet a complete shipping feature.

These are the bundled defaults; every row is editable in the keymap TOML below.

> **Quit confirmation (macOS).** `Cmd+Q` does not quit immediately. The first
> press shows a red **“Press ⌘Q one more time to quit”** alert in the top-right
> corner. Press `Cmd+Q` again within 5 seconds to quit; otherwise the alert
> closes automatically and nothing happens. The **Quit SonicTerm** menu item
> (no key equivalent) quits at once.

### Edit the platform-default user keymap

1. Open the command palette.
2. Run **Edit keymap.toml**. This opens the platform-default user file.
3. Edit that file, or manually open the custom path named by `keymap`.
4. Save, then run **Reload Config**. This applies to any keymap the `keymap`
   key names, including custom-named files — a reload re-reads the keymap file
   whether or not the selector changed, so no rename or restart is needed.

When a keymap reloads successfully, command-palette shortcut hints update with it.

### Binding syntax

Each shortcut is one `[[binding]]` table:

```toml
[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+d"
action = "split_right"
```

Modifier names:

| Modifier | Meaning |
| --- | --- |
| `super` | Command on macOS, Windows/Super key on Windows. App modifier on macOS. |
| `ctrl` | Control |
| `shift` | Shift |
| `alt` | Option on macOS, Alt on Windows/Linux. App modifier on Windows and Linux. |

The default macOS keymap mostly uses `super` (Command). The default Windows and
Linux keymaps use `alt` as SonicTerm's app modifier so `ctrl` shortcuts keep
working inside the shell. Windows/Linux keep a few compatibility aliases such as
`ctrl+t` and `ctrl+shift+c` / `ctrl+shift+v`.

Keys are written in lower case. Modifier order is normalized as
`super+ctrl+alt+shift+key`. Examples: `super+t`, `super+shift+p`, `alt+d`,
`alt+shift+d`, `ctrl+alt+shift+b`, `alt+left`, `alt+pageup`, `super+enter`.

### Actions with parameters

Some actions need a value; pane resize also has direction-specific shortcut actions:

```toml
[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+left"
action = "resize_pane_left"

[[binding]]
keys = "super+up"
action = { scroll = "line_up" }

[[binding]]
keys = "super+shift+b"
action = { toggle_broadcast = { scope = "tab" } }
```

Directions are `left`, `right`, `up`, `down`. Scroll values are `line_up`,
`line_down`, `page_up`, `page_down`, `to_top`, and `to_bottom`.

### Common action names

| Action | TOML value |
| --- | --- |
| New tab | `new_tab` |
| Close tab | `close_tab` |
| Close active pane or tab | `close_active_pane_or_tab` |
| Next / previous tab | `next_tab`, `prev_tab` |
| Split pane | `split_right`, `split_down` |
| Close pane | `close_pane` |
| Zoom pane | `toggle_pane_zoom` |
| Focus pane | `{ focus_pane = "left" }` |
| Resize pane | `resize_pane_left`, `resize_pane_right`, `resize_pane_up`, `resize_pane_down` |
| Copy / paste | `copy_to_clipboard`, `paste_from_clipboard` |
| Read-only navigation mode | `enter_copy_mode` |
| Quick select URL hints | `enter_quick_select` |
| Font size | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| Font weight | `increase_font_weight`, `decrease_font_weight`, `reset_font_weight` |
| New window | `new_window` |
| Move active tab to new window | `move_tab_to_new_window` |
| Quit app | `quit_app` |
| Fullscreen | `toggle_fullscreen` |
| Search | `open_search` |
| Command palette | `open_command_palette` |
| Update tab color | `update_tab_color` |
| Edit config file | `edit_config_file` |
| Edit keymap file | `open_keymap_file` |
| Reload config | `reload_config` |

### Example: make pane resize larger

The default keymaps bind the four `resize_pane_*` actions listed above, which
use the built-in step size. Use the parameterized `resize_pane` form when you
want a different step:

These examples use macOS `super`; on Windows/Linux use `alt` for the same app-level chord.

```toml
[[binding]]
keys = "super+shift+left"
action = { resize_pane = { dir = "left", amount = 10 } }

[[binding]]
keys = "super+shift+right"
action = { resize_pane = { dir = "right", amount = 10 } }
```

### Example: use Vim-style pane focus

```toml
[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+j"
action = { focus_pane = "down" }

[[binding]]
keys = "super+shift+k"
action = { focus_pane = "up" }

[[binding]]
keys = "super+shift+l"
action = { focus_pane = "right" }
```

At startup, an invalid selected keymap is logged and falls back to the bundled
platform default. A failed reload leaves the current in-memory keymap active.
Keymap edits apply when you run **Reload Config** from the command palette;
there is no file watcher.

### Terminal-style editing in app text fields

When terminal search, command-palette filtering, or tab renaming owns keyboard
input, SonicTerm provides the same core single-line editing controls in every
field. Search has a movable caret; editing and IME composition occur at that
caret in both the main window and torn-out windows.

| Key | App text-field action |
| --- | --- |
| `Ctrl+A` | Move to the start |
| `Ctrl+E` | Move to the end |
| `Ctrl+B` / `Ctrl+F` | Move one Unicode character left / right |
| `Ctrl+H` | Delete one Unicode character backward |
| `Ctrl+D` | Delete one Unicode character forward |
| `Ctrl+W` | Delete left whitespace, then the previous non-whitespace run |
| `Ctrl+U` | Delete from the start through the caret |
| `Ctrl+K` | Delete from the caret through the end |

Search also supports the standard unmodified `Left`, `Right`, `Home`, `End`,
and `Delete` keys for caret movement and forward deletion, matching the command
palette and tab rename fields.

These are exact field-local chords; adding Shift, Alt, or Super does not alias
them. The tab-color picker is selection-only and does not use text editing.
When no SonicTerm text field is active, the same `Ctrl+<letter>` keys continue
to the PTY as terminal control bytes, so shells and terminal applications keep
their own readline/ZLE behavior.

### READONLY mode shortcut policy

`enter_copy_mode` opens READONLY mode. In this mode SonicTerm blocks terminal
input and only allows shortcut actions for tab switching, pane focus, and search:
`next_tab`, `prev_tab`, `{ activate_tab = N }`, `activate_last_tab`,
`{ focus_pane = "left|right|up|down" }`, `open_search`, and
`check_for_updates`.

All other shortcuts are ignored by READONLY mode and are not forwarded to the
terminal. Search remains editable inside READONLY mode.

## 中文

SonicTerm 的快捷键是 TOML 文件。内置默认文件在 `assets/keymaps/`，首次启动后会
复制一份可编辑版本到：

```text
~/.sonicterm/keymaps/
├── sonicterm-macos.toml
├── sonicterm-windows.toml
└── sonicterm-linux.toml
```

当前使用哪个 keymap，由 `~/.sonicterm/sonicterm.toml` 决定：

```toml
keymap = "sonicterm-macos"
```

也可以把 `keymap` 写成任意 TOML 文件路径。

### 默认快捷键

macOS 上的应用修饰键是 `Cmd`；Windows 上是 `Alt`。在 Windows 上，`Ctrl+<字母>`
仍然交给 shell（Ctrl+C = SIGINT，Ctrl+R = 历史搜索，Ctrl+W = 删除单词……），
因此另外保留了几个终端通用的兼容别名（`Ctrl+T`、`Ctrl+Shift+C`、`Ctrl+Shift+V`）。

| 功能 | macOS | Windows |
| --- | --- | --- |
| 新建 Tab | `Cmd+T`（`Cmd+Shift+T`） | `Alt+T` / `Ctrl+T`（`Alt+Shift+T`） |
| 关闭 Pane 或 Tab | `Cmd+W` | `Alt+W` |
| 下一个 Tab | `Cmd+Shift+]` / `Cmd+Right` | `Alt+Shift+]` / `Alt+Right` |
| 上一个 Tab | `Cmd+Shift+[` / `Cmd+Left` | `Alt+Shift+[` / `Alt+Left` |
| 切换到 Tab 1–8 | `Cmd+1` … `Cmd+8` | `Alt+1` … `Alt+8` |
| 切换到最后一个 Tab | `Cmd+9` | `Alt+9` |
| 向右分屏 | `Cmd+D` | `Alt+D` |
| 向下分屏 | `Cmd+Shift+D` | `Alt+Shift+D` |
| 关闭 Pane | `Cmd+Shift+W` | `Alt+Shift+W` |
| 放大 Pane（Zoom） | `Cmd+Shift+Z` | `Alt+Shift+Z` |
| 切换 Pane 焦点（左/下/上/右） | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 调整 Pane 大小（左/右/上/下） | `Cmd+Shift+方向键` | `Alt+Shift+方向键` |
| 广播到当前 Tab | `Cmd+Shift+B` | `Alt+Shift+B` |
| 广播到所有 Tab | `Cmd+Ctrl+Shift+B` | `Ctrl+Alt+Shift+B` |
| READONLY / 复制模式 | `Cmd+[` | `Alt+[` |
| 复制 | `Cmd+C` | `Alt+C` / `Ctrl+Shift+C` |
| 粘贴 | `Cmd+V` | `Ctrl+Shift+V` |
| 增大字号 | `Cmd+=` / `Cmd++` | `Alt+=` / `Alt++` |
| 减小字号 | `Cmd+-` | `Alt+-` |
| 重置字号（回到配置的 `size`） | `Cmd+0` | `Alt+0` |
| 新建窗口 | `Cmd+N` | `Alt+N` |
| 退出应用（确认） | 先按 `Cmd+Q`，5 秒内再按一次 `Cmd+Q` | — |
| 切换全屏 | `Cmd+Enter` / `Cmd+Shift+F` | `Alt+Enter` / `Alt+Shift+F` / `F11` |
| 搜索 | `Cmd+F` | `Alt+F` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` |
| URL 快速选择 | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| 上滚 / 下滚一行 | `Cmd+Up` / `Cmd+Down` | `Alt+Up` / `Alt+Down` |
| 上滚 / 下滚一页 | `Cmd+PageUp` / `Cmd+PageDown` | `Alt+PageUp` / `Alt+PageDown` |
| 滚动到顶部 / 底部 | `Cmd+Home` / `Cmd+End` | `Alt+Home` / `Alt+End` |
| 重新加载配置 | `Cmd+R` / `Cmd+Shift+R` | `Alt+R` / `Alt+Shift+R` |

action 模型还支持 prompt 导航（`scroll_to_prev_prompt`、`scroll_to_next_prompt`）、
应用主题、检查更新和实验性 SSH pane action。这些 action 并非全部带有默认绑定；SSH transport
是可选实现，其实时 GUI session 目前还不是完整发布功能。

以上是内置默认值；下面的 keymap TOML 里每一行都可以自定义。

> **退出确认（macOS）。** `Cmd+Q` 不会立即退出。第一次按下时，右上角会显示红色的
> **“Press ⌘Q one more time to quit”** 提示；5 秒内再次按 `Cmd+Q` 才会退出，
> 超时后提示自动关闭且不执行任何操作。原生菜单里的 **Quit SonicTerm** 没有快捷键，
> 点击该菜单项会立即退出。

### 修改平台默认用户 keymap

1. 打开命令面板。
2. 执行 **Edit keymap.toml**；它会打开平台默认用户文件。
3. 编辑该文件，或手动打开 `keymap` 指向的自定义路径。
4. 保存后执行 **Reload Config**。这对 `keymap` 指向的任何 keymap 都有效，包括自定义
   名称的文件——重载会重新读取 keymap 文件，无论选择器是否变化，因此无需改名或重启。

keymap 成功重载时，命令面板中的快捷键提示也会一起更新。

### 绑定格式

每个快捷键都是一个 `[[binding]]`：

```toml
[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+d"
action = "split_right"
```

修饰键名称：

| 修饰键 | 含义 |
| --- | --- |
| `super` | macOS 上是 Command，Windows 上是 Windows/Super 键；macOS 的应用修饰键 |
| `ctrl` | Control |
| `shift` | Shift |
| `alt` | macOS 上是 Option，Windows/Linux 上是 Alt；Windows 和 Linux 的应用修饰键 |

默认 macOS keymap 主要使用 `super`（Command）。默认 Windows / Linux keymap 使用
`alt` 作为 SonicTerm 的应用修饰键，这样 `ctrl` 快捷键可以继续交给 shell 使用。
Windows / Linux 仍保留少量兼容别名，例如 `ctrl+t` 和 `ctrl+shift+c` / `ctrl+shift+v`。

按键名用小写，修饰键会按 `super+ctrl+alt+shift+key` 的顺序规范化。比如：
`super+t`、`super+shift+p`、`alt+d`、`alt+shift+d`、`ctrl+alt+shift+b`、
`alt+left`、`alt+pageup`、`super+enter`。

### 带参数的 action

有些 action 需要额外参数；Pane resize 也提供按方向命名的快捷 action：

```toml
[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+left"
action = "resize_pane_left"

[[binding]]
keys = "super+up"
action = { scroll = "line_up" }

[[binding]]
keys = "super+shift+b"
action = { toggle_broadcast = { scope = "tab" } }
```

方向值是 `left`、`right`、`up`、`down`。滚动值是 `line_up`、`line_down`、
`page_up`、`page_down`、`to_top`、`to_bottom`。

### 常用 action 名称

| 功能 | TOML 值 |
| --- | --- |
| 新建 Tab | `new_tab` |
| 关闭 Tab | `close_tab` |
| 关闭当前 Pane 或 Tab | `close_active_pane_or_tab` |
| 下一个 / 上一个 Tab | `next_tab`, `prev_tab` |
| 分屏 | `split_right`, `split_down` |
| 关闭 Pane | `close_pane` |
| 放大 Pane | `toggle_pane_zoom` |
| 切换 Pane 焦点 | `{ focus_pane = "left" }` |
| 调整 Pane 大小 | `resize_pane_left`, `resize_pane_right`, `resize_pane_up`, `resize_pane_down` |
| 复制 / 粘贴 | `copy_to_clipboard`, `paste_from_clipboard` |
| 只读导航模式 | `enter_copy_mode` |
| URL 快速选择 | `enter_quick_select` |
| 字体大小 | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| 字体粗细 | `increase_font_weight`, `decrease_font_weight`, `reset_font_weight` |
| 新建窗口 | `new_window` |
| 将当前 Tab 移至新窗口 | `move_tab_to_new_window` |
| 退出应用 | `quit_app` |
| 全屏 | `toggle_fullscreen` |
| 搜索 | `open_search` |
| 命令面板 | `open_command_palette` |
| 修改 Tab 颜色 | `update_tab_color` |
| 编辑配置文件 | `edit_config_file` |
| 编辑 keymap 文件 | `open_keymap_file` |
| 重新加载配置 | `reload_config` |

### 示例：把 pane resize 改大

默认 keymap 绑定的是上表列出的四个 `resize_pane_*` action，使用内置步长。若需要
不同的步长，请使用带参数的 `resize_pane` 形式：

下面示例使用 macOS 的 `super`；Windows/Linux 上同样的应用级快捷键请把 `super` 换成 `alt`。

```toml
[[binding]]
keys = "super+shift+left"
action = { resize_pane = { dir = "left", amount = 10 } }

[[binding]]
keys = "super+shift+right"
action = { resize_pane = { dir = "right", amount = 10 } }
```

### 示例：Vim 风格切换 pane

```toml
[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+j"
action = { focus_pane = "down" }

[[binding]]
keys = "super+shift+k"
action = { focus_pane = "up" }

[[binding]]
keys = "super+shift+l"
action = { focus_pane = "right" }
```

启动时，选中的 keymap 解析失败会记录日志并回退到平台内置默认值；重载失败时则继续使用当前内存中的 keymap。keymap 的修改在你从命令面板执行 **Reload Config** 时生效；系统中没有文件 watcher。

### 应用文本框中的终端风格编辑

当终端搜索、命令面板筛选或 Tab 重命名文本框接管键盘输入时，SonicTerm 在所有文本框中
提供一致的核心单行编辑键。搜索框现在也有可移动光标；主窗口和拖出的子窗口都会在该光标
位置执行编辑和 IME 组合输入。

| 按键 | 应用文本框行为 |
| --- | --- |
| `Ctrl+A` | 移到开头 |
| `Ctrl+E` | 移到结尾 |
| `Ctrl+B` / `Ctrl+F` | 向左 / 向右移动一个 Unicode 字符 |
| `Ctrl+H` | 向后删除一个 Unicode 字符 |
| `Ctrl+D` | 向前删除一个 Unicode 字符 |
| `Ctrl+W` | 先删除光标左侧空白，再删除前一个连续非空白片段 |
| `Ctrl+U` | 删除开头到光标之间的内容 |
| `Ctrl+K` | 删除光标到结尾之间的内容 |

搜索框还支持未带修饰键的 `Left`、`Right`、`Home`、`End` 和 `Delete`，用于移动
光标和向前删除；这与命令面板和 Tab 重命名文本框保持一致。

这些是文本框内的精确组合键；额外按下 Shift、Alt 或 Super 不会被当成同一个编辑命令。
Tab 颜色选择器只用于选择，不属于文本输入。没有 SonicTerm 文本框处于活动状态时，同样的
`Ctrl+<字母>` 仍会作为终端控制字节发送给 PTY，因此 shell 和终端程序会继续使用自己的
readline/ZLE 行为。

### READONLY 模式快捷键策略

`enter_copy_mode` 会进入 READONLY 模式。在这个模式下，SonicTerm 会阻止输入进入终端，
只允许四个类别的快捷键：切换 Tab、切换 Pane 焦点、搜索、检查更新。对应 action 是
`next_tab`、`prev_tab`、`{ activate_tab = N }`、`activate_last_tab`、
`{ focus_pane = "left|right|up|down" }`、`open_search` 和 `check_for_updates`。

其它快捷键会被 READONLY 模式忽略，也不会转发给终端。READONLY 中打开搜索框后，搜索框仍然可以编辑。
