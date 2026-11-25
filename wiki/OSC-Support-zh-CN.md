# OSC 支持

[English](OSC-Support)

程序通过操作系统命令（OSC）序列与终端通信：`ESC ] <编号> ; <内容>`，以 `ESC \` 或 BEL 结束。本页列出
WezTerm、Ghostty 或 kitty 实现的每一种 OSC，以及 SonicTerm 对每一种的处理。三者中至少两个实现的形式，
SonicTerm 都实现；只有一个实现、但常见程序会发送的形式，SonicTerm 也实现。其余形式列在
[不支持的序列及原因](#不支持的序列及原因)中，并写明原因。

SonicTerm 不支持的序列会被读到结束符后丢弃，其内容不会打印出来。解析细节、大小上限与具体回复字节见
[终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)。剪贴板策略键见[配置](Configuration-zh-CN)。

“同样支持”一列中，W 表示 WezTerm，G 表示 Ghostty，K 表示 kitty。

## 支持的序列

| 序列 | 含义 | 同样支持 | 说明 |
|---|---|---|---|
| `0`、`2` | 窗口标题 | W G K | `0` 同时清除 `1` 设置的标签页名称。 |
| `1` | 标签页名称 | W | 在收到 `0`、空的或不带参数的 `1`、或重置（RIS）之前作为标签页标签显示；oh-my-zsh 和 fish 会发送它。窗口标题不变，用户重命名过的标签页保留用户的名称。 |
| `4 ; n ; spec` / `?` | 设置或查询调色板第 `n` 项 | W G K | 一个序列可带多组；回复 `rgb:rrrr/gggg/bbbb`。 |
| `104 [; n…]` | 重置指定调色板项或全部 | W G K | |
| `10`/`11`/`12` `; spec` / `?` | 前景色、背景色、光标颜色 | W G K | 每多一个值就顺延到下一种颜色。 |
| `17`/`19` `; spec` / `?` | 选区背景色、前景色 | W K | 只有程序设置 `19` 之后，选中文字才使用该前景色。 |
| `110`、`111`、`112`、`117`、`119` | 重置上述颜色 | W K（G：110–112） | |
| `21 ; key=value;…` | kitty 颜色协议 | G K | 调色板、`foreground`、`background`、`cursor`、`cursor_text`、`selection_*`；未知键回复 `unknown=`。 |
| `22 ; [=><?] names` | 指针形状、形状栈与查询 | G K | CSS 名称与 X11 别名；主屏幕与备用屏幕各有一个栈。 |
| `7 ; file://host/path` | 工作目录 | W G K | 也接受 `kitty-shell-cwd://`。只有本机主机名才授权本地路径；空值清除。 |
| `9 ; 9 ; path` | 工作目录（ConEmu） | G K | 绝对路径，可以带一对引号；按本地 OSC 7 处理。 |
| `1337 ; CurrentDir=path` | 工作目录（iTerm2） | G | 本地 `file://` 或 `kitty-shell-cwd://` URI 按 OSC 7 处理；裸路径只用于显示。 |
| `8 ; params ; uri` | 超链接 | W G K | |
| `9 ; text` | 通知 | W G K | |
| `777 ; notify ; title [; body]` | 通知 | W G K | 没有正文时，文字作为正文显示。 |
| `99 ; …` | kitty 通知协议 | K | |
| `9 ; 4 ; state [; percent]` | 进度（Dock、任务栏、启动器） | W G K | 60 秒没有更新，或到达 100 % 后 5 秒，自动清除。 |
| `52 ; kinds ; base64` / `?` | 写入或读取剪贴板 | W G K（W：仅写入） | Linux 上 `p` 指 primary 选区。不带数据或数据为空的 `52;c` 清空剪贴板。写入遵循 `osc52`，读取遵循 `osc52_read`（默认 `ask`，会打开确认页）。 |
| `1337 ; Copy=:base64` | 写入剪贴板（iTerm2） | G | 与 OSC 52 写入使用同一策略；值为空或 `?` 时不写入。 |
| `5522 ; …` | kitty 剪贴板协议 | G K | `write`、`wdata`、`walias` 与 `read`，类型列表（`.`）与会话授权。D-6d：省略无法映射或超出名称上限的类型；只有 Windows 替换无效 UTF-8，PNG 使用注册格式而不转换为 DIB。X11 程序提供一小时后过期；Wayland 需要 XWayland。各平台写入、读取与列出的 MIME 类型见[终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)。 |
| `133 ; A/B/C/D/L/N/P/I` | Shell 提示符标记 | W G K | 点击提示符会移动光标：提示符设置了 `click_events` 时发送 SGR 按下事件，否则按 `[terminal] click_to_move` 发送方向键（`special_key=1` 时发送 kitty 的按键），备用屏幕上不移动。`redraw=0` 关闭行数缩小时保留光标行的处理，Windows 上不做该处理。启用长命令通知时，`C;cmdline_url=` 为其提供命令。 |
| `9 ; 12` | 提示符开始（ConEmu） | G | |
| `1337 ; File=…` | 内联图片 | W | |
| `1337 ; SetUserVar=name=base64` | 用户变量 | W K | 在 `list_panes` 中提供给 agent。 |
| `7501 ; state=…` / `?` | 程序状态（working、blocked、done、error、idle） | K | 显示为标签页标记，并出现在 `list_panes` 中；`state=clear` 移除它。Claude Code 会发送它。 |
| DECSET 1005、1006、1015、1016 | 鼠标编码 | W G K（1015：G K） | 1016 报告从零开始的像素坐标，与 Ghostty 和 kitty 一致；WezTerm 会加 1。1005 的列与行最大为 2015。 |
| DECSET 5522 | kitty 剪贴板协议的粘贴事件 | G K | DECRST 与 RIS 重置它；DECRQM 报告它。 |

## 不支持的序列及原因

| 序列 | 原因 |
|---|---|
| `66` 文字尺寸、`72` 拖放、`5113` 文件传输、`30001`/`30101` 颜色栈 | 只有 kitty 实现，常见程序不依赖它们。文件传输会按请求写入文件。 |
| `21` 的 `visual_bell`、`transparent_background_color1..8` | SonicTerm 没有对应设置；查询回复空值。 |
| `1337` 的 `ReportCellSize`、`UnicodeVersion` | 只有 WezTerm 实现。单元格尺寸请使用 `CSI 16 t`。 |
| `1337 ; Copy=;base64` | WezTerm 解析它但从不执行；请使用 `Copy=:`。 |
| 其他 `1337` 键（`SetMark`、`RemoteHost`、`SetBadgeFormat`、`RequestAttention` 等） | WezTerm、Ghostty、kitty 都不处理它们。 |
| `13`–`16`、`18`、`113`–`116`、`118`、`5`、`6`、`105`、`106`、`3`、`46`、`50`、`51`、`60`、`61` | 指针、Tektronix、特殊颜色、X 属性、日志与字体；三者都忽略。 |
| ConEmu `9;1`–`9;3`、`9;5`–`9;8`、`9;10`、`9;11`；`777` 的其他子命令 | 三者都不处理。 |
| `3008`、`176`、`440`、`633`、`697` 及其他厂商编号 | 三者都忽略。 |
| “鼠标离开窗口”报告 | 只有 kitty 实现。 |

## Windows

在 Windows 上，程序输出先经过 ConPTY，ConPTY 不转发的序列不会到达 SonicTerm。安装版 SonicTerm（MSI）
自带打包的 ConPTY 运行时并始终使用它；打包运行时未通过检查时，窗格启动失败，而不会回退。只有没有
`conpty` 文件夹的源码构建才使用系统 ConPTY（选择规则见[终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)）。

Windows CI 上的真实 ConPTY 测试（[本地 gate](Local-Gate-zh-CN)）通过两种运行时分别输出示例形式，并按方向
记录字节是否原样到达。它只检查传输：SonicTerm 随后的处理由单元测试覆盖。

| 测试的形式 | 打包 ConPTY | 系统 ConPTY |
|---|---|---|
| OSC 4、10–12、17、19、21、22、104、110–119 的设置、重置与查询；7501；OSC 52 与 5522 的写入和读取；DECSET 5522 粘贴事件；9;9、7（`file://`、`kitty-shell-cwd://`）、9;4、777、OSC 1 | 原样到达，回复也是 | 被丢弃 |
| OSC 133 标记与选项、OSC 9;12、`1337 ; SetUserVar`、`CurrentDir` 与 `Copy=:` | 原样到达 | 原样到达 |
| DECSET 1005、1006、1015、1016 | 原样到达，报告也是 | 模式请求被丢弃，因此程序收不到这些报告 |
| DECRQM 5522 | 原样到达，回复也是 | 请求被丢弃 |
| 提示符点击事件 | 原样到达 | 原样到达 |
| 在提示符处缩小行数 | ConPTY 自行重绘屏幕，因此 SonicTerm 收到的是控制台的重绘，而不是程序输出的行 | 与打包版相同 |

不在测试中的形式，例如 OSC 0、2、8、9、99 与 `1337 ; File=`，没有记录的 Windows 结果。
