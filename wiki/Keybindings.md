# Keybindings / 快捷键

## English

Default keymaps live in `assets/keymaps/`:

- `sonicterm-macos.toml`
- `sonicterm-windows.toml`
- `sonicterm-linux.toml`

User keymaps are copied into `~/.sonicterm/keymaps/`. A binding looks like:

```toml
[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+d"
action = "split_right"
```

Actions may also carry parameters:

```toml
[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }
```

## 中文

默认快捷键在 `assets/keymaps/`。用户快捷键会复制到 `~/.sonicterm/keymaps/`。格式与上面的
TOML 示例相同。

常见修饰键：

- macOS: `super` = Command
- Windows: `ctrl` = Control
- 可组合：`shift`、`alt`、`super`、`ctrl`

命令面板会显示当前 keymap 中对应命令的快捷键。
