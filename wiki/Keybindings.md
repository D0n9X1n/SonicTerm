# Keybindings / 快捷键

## English

SonicTerm keymaps are TOML files. Bundled defaults live in `assets/keymaps/`,
and editable user copies live in:

```text
~/.snoicterm/keymaps/
├── sonicterm-macos.toml
├── sonicterm-windows.toml
└── sonicterm-linux.toml
```

The active keymap is selected from `~/.snoicterm/sonicterm.toml`:

```toml
keymap = "sonicterm-macos"
```

You can also point `keymap` at any TOML file path.

### Edit the active keymap

1. Open the command palette.
2. Run **Edit keymap.toml**.
3. Change `~/.snoicterm/keymaps/<name>.toml`.
4. Run **Reload Config** from the command palette.

The command palette and keymap cheatsheet read the active keymap, so shortcut
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
| `super` | Command on macOS, Windows/Super key on Windows |
| `ctrl` | Control |
| `shift` | Shift |
| `alt` | Option/Alt |

The default macOS keymap mostly uses `super`. The default Windows keymap mostly
uses `ctrl` and `ctrl+shift`, so Windows users usually copy examples from
`sonicterm-windows.toml`.

Keys are written in lower case. Examples: `super+t`, `super+shift+p`,
`ctrl+alt+shift+d`, `super+left`, `super+pageup`, `super+enter`.

### Actions with parameters

Some actions need a value:

```toml
[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+left"
action = { resize_pane = { dir = "left", amount = 5 } }

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
| Resize pane | `{ resize_pane = { dir = "left", amount = 5 } }` |
| Copy / paste | `copy_to_clipboard`, `paste_from_clipboard` |
| Read-only navigation mode | `enter_copy_mode` |
| Quick select URL hints | `enter_quick_select` |
| Font size | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| New window | `new_window` |
| Fullscreen | `toggle_fullscreen` |
| Search | `open_search` |
| Command palette | `open_command_palette` |
| Keymap cheatsheet | `show_keymap_cheatsheet` |
| Edit config file | `edit_config_file` |
| Edit keymap file | `open_keymap_file` |
| Reload config | `reload_config` |

### Example: make pane resize larger

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

## 中文

SonicTerm 的快捷键是 TOML 文件。内置默认文件在 `assets/keymaps/`，首次启动后会
复制一份可编辑版本到：

```text
~/.snoicterm/keymaps/
├── sonicterm-macos.toml
├── sonicterm-windows.toml
└── sonicterm-linux.toml
```

当前使用哪个 keymap，由 `~/.snoicterm/sonicterm.toml` 决定：

```toml
keymap = "sonicterm-macos"
```

也可以把 `keymap` 写成任意 TOML 文件路径。

### 修改当前 keymap

1. 打开命令面板。
2. 执行 **Edit keymap.toml**。
3. 修改 `~/.snoicterm/keymaps/<name>.toml`。
4. 回到命令面板执行 **Reload Config**。

命令面板和快捷键 cheatsheet 都会读取当前 keymap，所以 reload 之后快捷键提示也会更新。

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
| `super` | macOS 上是 Command，Windows 上是 Windows/Super 键 |
| `ctrl` | Control |
| `shift` | Shift |
| `alt` | Option/Alt |

默认 macOS keymap 主要使用 `super`。默认 Windows keymap 主要使用 `ctrl` 和
`ctrl+shift`，所以 Windows 用户通常直接参考 `sonicterm-windows.toml`。

按键名用小写。比如：`super+t`、`super+shift+p`、`ctrl+alt+shift+d`、
`super+left`、`super+pageup`、`super+enter`。

### 带参数的 action

有些 action 需要额外参数：

```toml
[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+left"
action = { resize_pane = { dir = "left", amount = 5 } }

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
| 快捷键 cheatsheet | `show_keymap_cheatsheet` |
| 编辑配置文件 | `edit_config_file` |
| 编辑 keymap 文件 | `open_keymap_file` |
| 重新加载配置 | `reload_config` |

### 示例：把 pane resize 改大

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
