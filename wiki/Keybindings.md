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
| Reset font size | `Cmd+0` | `Alt+0` |
| New window | `Cmd+N` | `Alt+N` |
| Quit app (hold) | `Cmd+Q` **hold ~0.8s** | — |
| Toggle fullscreen | `Cmd+Enter` / `Cmd+Shift+F` | `Alt+Enter` / `Alt+Shift+F` / `F11` |
| Search | `Cmd+F` | `Alt+F` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` |
| Quick select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| Scroll line up/down | `Cmd+Up` / `Cmd+Down` | `Alt+Up` / `Alt+Down` |
| Scroll page up/down | `Cmd+PageUp` / `Cmd+PageDown` | `Alt+PageUp` / `Alt+PageDown` |
| Scroll to top/bottom | `Cmd+Home` / `Cmd+End` | `Alt+Home` / `Alt+End` |
| Reload config | `Cmd+R` / `Cmd+Shift+R` | `Alt+R` / `Alt+Shift+R` |

These are the bundled defaults; every row is editable in the keymap TOML below.

> **Hold-to-quit (macOS).** `Cmd+Q` does not quit immediately. A single press
> shows a red **“Hold ⌘Q to quit the app”** alert in the top-right corner;
> the app exits only if you keep the chord held for about 0.8s. Release early
> and nothing happens. This guards against losing every tab to a fat-fingered
> `Cmd+Q`. The **Quit SonicTerm** menu item (no key equivalent) quits at once.

### Edit the active keymap

1. Open the command palette.
2. Run **Edit keymap.toml**.
3. Change `~/.sonicterm/keymaps/<name>.toml`.
4. Run **Reload Config** from the command palette.

The command palette reads the active keymap, so shortcut
hints update after reload.

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
| New window | `new_window` |
| Quit app | `quit_app` |
| Fullscreen | `toggle_fullscreen` |
| Search | `open_search` |
| Command palette | `open_command_palette` |
| Update tab color | `update_tab_color` |
| Edit config file | `edit_config_file` |
| Edit keymap file | `open_keymap_file` |
| Reload config | `reload_config` |

### Example: make pane resize larger

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

If a keymap file fails to parse, SonicTerm logs the error and falls back to the
bundled platform default.

### READONLY mode shortcut policy

`enter_copy_mode` opens READONLY mode. In this mode SonicTerm blocks terminal
input and only allows shortcut actions for tab switching, pane focus, and search:
`next_tab`, `prev_tab`, `{ activate_tab = N }`, `activate_last_tab`,
`{ focus_pane = "left|right|up|down" }`, and `open_search`.

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
| 重置字号 | `Cmd+0` | `Alt+0` |
| 新建窗口 | `Cmd+N` | `Alt+N` |
| 切换全屏 | `Cmd+Enter` / `Cmd+Shift+F` | `Alt+Enter` / `Alt+Shift+F` / `F11` |
| 搜索 | `Cmd+F` | `Alt+F` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` |
| URL 快速选择 | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| 上滚 / 下滚一行 | `Cmd+Up` / `Cmd+Down` | `Alt+Up` / `Alt+Down` |
| 上滚 / 下滚一页 | `Cmd+PageUp` / `Cmd+PageDown` | `Alt+PageUp` / `Alt+PageDown` |
| 滚动到顶部 / 底部 | `Cmd+Home` / `Cmd+End` | `Alt+Home` / `Alt+End` |
| 重新加载配置 | `Cmd+R` / `Cmd+Shift+R` | `Alt+R` / `Alt+Shift+R` |

以上是内置默认值；下面的 keymap TOML 里每一行都可以自定义。

### 修改当前 keymap

1. 打开命令面板。
2. 执行 **Edit keymap.toml**。
3. 修改 `~/.sonicterm/keymaps/<name>.toml`。
4. 回到命令面板执行 **Reload Config**。

命令面板会读取当前 keymap，所以 reload 之后快捷键提示也会更新。

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
| 调整 Pane 大小 | `{ resize_pane = { dir = "left", amount = 5 } }` |
| 复制 / 粘贴 | `copy_to_clipboard`, `paste_from_clipboard` |
| 只读导航模式 | `enter_copy_mode` |
| URL 快速选择 | `enter_quick_select` |
| 字体大小 | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| 新建窗口 | `new_window` |
| 全屏 | `toggle_fullscreen` |
| 搜索 | `open_search` |
| 命令面板 | `open_command_palette` |
| 修改 Tab 颜色 | `update_tab_color` |
| 编辑配置文件 | `edit_config_file` |
| 编辑 keymap 文件 | `open_keymap_file` |
| 重新加载配置 | `reload_config` |

### 示例：把 pane resize 改大

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

如果 keymap 文件解析失败，SonicTerm 会写日志，并回退到当前平台的内置默认 keymap。

### READONLY 模式快捷键策略

`enter_copy_mode` 会进入 READONLY 模式。在这个模式下，SonicTerm 会阻止输入进入终端，
只允许三个类别的快捷键：切换 Tab、切换 Pane 焦点、搜索。对应 action 是
`next_tab`、`prev_tab`、`{ activate_tab = N }`、`activate_last_tab`、
`{ focus_pane = "left|right|up|down" }` 和 `open_search`。

其它快捷键会被 READONLY 模式忽略，也不会转发给终端。READONLY 中打开搜索框后，搜索框仍然可以编辑。
