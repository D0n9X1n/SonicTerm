# Usage / 用法

## English

Install the `.dmg` on macOS or `.msi` on Windows from the GitHub Release page.
SonicTerm creates its config directory on first launch.

Common actions:

| Action | macOS | Windows |
| --- | --- | --- |
| New tab | `Cmd+T` | `Ctrl+T` |
| Close pane/tab | `Cmd+W` | `Ctrl+Shift+W` |
| Split right | `Cmd+D` | `Ctrl+Shift+D` |
| Split down | `Cmd+Shift+D` | `Ctrl+Alt+Shift+D` |
| Command palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Search | `Cmd+F` | `Ctrl+Shift+F` |
| READONLY mode | `Cmd+[` | `Ctrl+Shift+[` |

Tabs can be dragged out into separate windows and dragged back to merge.

READONLY mode is for safe scrollback/navigation. It blocks terminal input and
only keeps a small shortcut whitelist active: tab switching, pane focus, and
search. If search is open while READONLY is active, typing edits the search box
only and is not sent to the terminal.

## 中文

从 GitHub Release 下载 macOS `.dmg` 或 Windows `.msi` 安装包。首次启动时
SonicTerm 会自动创建配置目录。

常用操作：

| 功能 | macOS | Windows |
| --- | --- | --- |
| 新建 Tab | `Cmd+T` | `Ctrl+T` |
| 关闭 Pane/Tab | `Cmd+W` | `Ctrl+Shift+W` |
| 向右分屏 | `Cmd+D` | `Ctrl+Shift+D` |
| 向下分屏 | `Cmd+Shift+D` | `Ctrl+Alt+Shift+D` |
| 命令面板 | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| 搜索 | `Cmd+F` | `Ctrl+Shift+F` |
| READONLY 模式 | `Cmd+[` | `Ctrl+Shift+[` |

Tab 可以拖出成为独立窗口，也可以拖回合并。

READONLY 模式用于安全查看 scrollback / 导航。它会阻止输入进入终端，只保留一个很小的
快捷键白名单：切换 Tab、切换 Pane 焦点、搜索。如果 READONLY 中打开了搜索框，键盘输入只会修改搜索框，不会发送到终端。
