# Usage / 用法

## English

Install the `.dmg` on macOS or `.msi` on Windows from the GitHub Release page.
SonicTerm creates its config directory on first launch.

Common actions:

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| New tab | `Cmd+T` | `Alt+T` (`Ctrl+T` alias) |
| Close pane/tab | `Cmd+W` | `Alt+W` |
| Split right | `Cmd+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` |
| Search | `Cmd+F` | `Alt+F` |
| READONLY mode | `Cmd+[` | `Alt+[` |

Tabs can be dragged out into separate windows and dragged back to merge.

Command palette input behaves like a single-line text field: spaces, CJK IME
composition, left/right arrows, Home, End, Delete, and Backspace all edit the
query or rename text in place. IME candidate windows anchor to the palette input
caret when the palette is open. **Rename Active Tab** accepts an empty submit to
clear the custom title and return to the automatic title. **Update Tab Color**
opens a theme-color picker for the current tab; colors come from the active
ANSI/bright ANSI theme palette only. The first option, **Reset to Default**,
clears the custom color and restores the default tab highlight. Choosing a color
updates the tab text + top accent. **Check for Updates** checks GitHub Releases
and shows a top-right notification bubble; it does not install anything
automatically.

Broadcast mode sends input from the active pane to peer panes in the current tab
(or all tabs when configured by keymap action), including panes in torn-out
windows.

READONLY mode is scoped to the current window. It blocks terminal input in that
window and only keeps a small shortcut whitelist active: tab switching, pane
focus, and search. If search is open while READONLY is active, typing edits the
search box only and is not sent to the terminal.

## 中文

从 GitHub Release 下载 macOS `.dmg` 或 Windows `.msi` 安装包。首次启动时
SonicTerm 会自动创建配置目录。

常用操作：

| 功能 | macOS | Windows / Linux |
| --- | --- | --- |
| 新建 Tab | `Cmd+T` | `Alt+T`（`Ctrl+T` 兼容别名） |
| 关闭 Pane/Tab | `Cmd+W` | `Alt+W` |
| 向右分屏 | `Cmd+D` | `Alt+D` |
| 向下分屏 | `Cmd+Shift+D` | `Alt+Shift+D` |
| 命令面板 | `Cmd+Shift+P` | `Alt+Shift+P` |
| 搜索 | `Cmd+F` | `Alt+F` |
| READONLY 模式 | `Cmd+[` | `Alt+[` |

Tab 可以拖出成为独立窗口，也可以拖回合并。

命令面板输入框支持普通单行文本编辑：空格、中文/日文等 IME 组合输入、左右方向键、Home、End、Delete 和 Backspace 都会编辑当前 query 或重命名文本。命令面板打开时，输入法候选词窗口会跟随命令面板输入光标。**Rename Active Tab** 提交空标题会清除自定义标题并恢复自动标题。**Update Tab Color** 会为当前 Tab 打开主题颜色选择器；颜色只来自当前 theme 的 ANSI / bright ANSI palette。第一个选项 **Reset to Default** 会清除自定义颜色并恢复默认高亮；选中颜色后会更新 Tab 文本颜色和顶部 accent。**Check for Updates** 会检查 GitHub Releases 并在右上角显示 notification bubble；它不会自动安装任何内容。

Broadcast 模式会把当前 Pane 的输入同步发送到同一个 Tab 的其他 Pane，也可以通过 keymap action 配成所有 Tab；拖出的独立窗口也会参与对应范围。

READONLY 模式作用于当前窗口，用于安全查看 scrollback / 导航。它会阻止该窗口输入进入终端，只保留一个很小的快捷键白名单：切换 Tab、切换 Pane 焦点、搜索。如果 READONLY 中打开了搜索框，键盘输入只会修改搜索框，不会发送到终端。
