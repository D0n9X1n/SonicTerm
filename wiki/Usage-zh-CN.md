# 用法

[English](Usage)

### 安装与首次启动

从 [GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases) 下载当前平台的安装包：

- macOS Apple Silicon：`SonicTerm-<tag>-mac-aarch64.dmg`
- macOS Intel：`SonicTerm-<tag>-mac-x86_64.dmg`
- Windows x64：`SonicTerm-<tag>-windows-x86_64.msi`
- Linux x86_64：`SonicTerm-<tag>-linux-x86_64.deb` 或
  `SonicTerm-<tag>-linux-x86_64.tar.gz`

macOS 上打开 DMG，把 `SonicTerm.app` 移到 Applications。发布构建使用 ad-hoc
签名，但没有 Apple Developer ID 签名，也没有 notarize。如果首次启动被 macOS
阻止，请在 Finder 右键菜单中选择 **Open**。Apple Silicon 安装包要求 macOS 14.0+，
Intel 安装包要求 macOS 15.0+。Cairo 及其原生依赖已随包提供，用户无需安装 Homebrew。

Windows 上运行 MSI。它会按机器安装到 Program Files，并添加开始菜单快捷方式。
安装程序还会把 SonicTerm 注册为支持脚本文件的可选 handler，但不会修改当前默认应用。

用以下命令安装 Debian package：

```sh
sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb
```

Linux package 面向 x86_64，并保证最多需要 glibc 2.35 ABI。`.deb` 会安装已链接的
依赖和桌面 metadata。使用便携归档时，请先解压，再从 payload 目录运行
`sonicterm`，确保相邻的 `assets/` 目录仍可读取。主机需要自行提供运行库；X11
需要 `libxkbcommon-x11.so.0`。X11 与 Wayland 都受支持。

首次正常启动会创建 `~/.sonicterm/`、写入 `sonicterm.toml`，并生成可编辑的主题
和 keymap 示例。本地打包与发布资产的详细说明见 [打包](Packaging-zh-CN)。

### 常用工作流

命令面板是查找 action 最快的方法：

- macOS：`Cmd+Shift+P`
- Windows 和 Linux：`Alt+Shift+P`

搜索 **关于 SonicTerm** 或 **版本**，选择 **关于 SonicTerm** 结果后按 Enter，
会关闭面板并在该窗口已有的绿色通知中显示 `SonicTerm <version>`。版本号来自当前运行的构建。通知会替换当前气泡，并在五秒后
自动关闭，也可点击其关闭按钮。READONLY 模式下也可使用；不会在线查询发布版本，
也不会向 shell 发送输入。

常用默认快捷键如下：

| 功能 | macOS | Windows 和 Linux |
| --- | --- | --- |
| 新建标签页 | `Cmd+T` | `Alt+T` 或 `Ctrl+T` |
| 关闭当前 pane 或标签页 | `Cmd+W` | `Alt+W` |
| 向右 / 向下分屏 | `Cmd+D` / `Cmd+Shift+D` | `Alt+D` / `Alt+Shift+D` |
| 切换 pane 焦点 | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| 搜索 | `Cmd+F` | `Alt+F` |
| READONLY 模式 | `Cmd+[` | `Alt+[` |
| 快速选择 URL | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| 广播到当前标签页 | `Cmd+Shift+B` | `Alt+Shift+B` |
| 重载配置 | `Cmd+R` | `Alt+R` |

每个 pane 都有独立的子 PTY。一个标签页可以包含分屏树。标签页可以排序、在
SonicTerm 窗口之间拖动，也可以拖出成为新窗口。现有 pane 与 PTY 会一起移动，
shell 不会重启。关闭分屏会关闭对应 PTY；关闭最后一个 pane 会关闭标签页。
对已放大的 pane 分屏会退出放大状态、恢复分屏布局并聚焦新 pane；主窗口与拖出的窗口行为一致。
分屏被拒绝时，放大状态和焦点保持不变；存活子窗口中的拒绝不会把分屏操作转移到主窗口。

广播模式会把源 pane 的输入复制到当前标签页或所有标签页中的其它 pane。所有参与 pane
（固定的源 pane 与每个符合条件的接收 pane）在主窗口和拖出窗口中都有四边细红框。
顶部边线与其它边线一样为 2 个物理像素；不使用遮挡终端的横幅或警告文本。
即使只有源 pane，广播启用时也会标记；关闭广播或关闭源 pane 会清除高亮。
源 pane 不参与镜像投递，因此不会收到两份输入。请谨慎使用：输入会发送到每个接收 pane，
按键编码遵循各 pane 协商的终端模式。

完整默认快捷键、action 名称和自定义格式见 [快捷键](Keybindings-zh-CN)。

### 搜索保留的输出

搜索覆盖活动窗格保留的回滚历史与当前屏幕，不只覆盖可见行。修改查询后优先选中当前视口中
的第一个匹配；若没有，则选中视口下方的下一个匹配，或上方最后一个匹配。计数使用完整匹配
列表：上方四个、当前一个、下方两个时显示 `5/7`。

搜索读取每个单元格保存的完整文本，包括组合符号和其它零宽字符。子串模式把查询和每个单元格的
文本都按 NFC 比较，因此预组合的 `é` 与 `e` 后接 U+0301 可以互相找到。规范化按单元格进行：
能与基字母组合的符号不能单独搜索，该基字母也不能单独搜索；没有预组合形式的符号（例如 `q`
后接 U+0301）仍可单独搜索。正则模式直接匹配原始字符，不规范化模式或文本；包含组合符号的
匹配会高亮承载该符号的单元格，从同一单元格开始的多个匹配只计一次。

键入、输入法提交和粘贴只更新选中结果，不自动滚动。Enter 或下箭头向后查找，Shift+Enter
或上箭头向前查找，首尾循环。选中结果在屏外时，第一次导航先显示它，不跳过它；结果已可见
时不重新居中视口。搜索粘贴只进入查询，不发送给 shell 或广播窗格，并去除控制字符。
已经因保留上限被淘汰的历史和其它窗格不在搜索范围内。

### 窗口名称与编号

终端窗口在进程内从 1 开始编号，例如 `#1 SonicTerm`。在命令面板中选择
**重命名窗口**，可设置为 `#2 Work`。只需编辑自定义名称：Enter 去除
首尾空白并保存，留空恢复默认编号标题，Escape 取消。支持 Unicode 和输入法；去除
首尾空白后最多 128 个 Unicode 标量值。控制字符、换行及超长输入会被拒绝并显示原因。
已配置的粘贴快捷键只向编辑框插入文字，不会发送给 shell。

同一进程内编号永不复用或重新分配。新建窗口与拆出的标签页获得新编号和空名称；
把标签页移入现有窗口，或隐藏后恢复保留的窗口，都不会改变窗口身份。预热辅助窗口
在被采用前不编号。名称与编号不跨重启保存；不同进程都可能从 1 开始。标签页名称、
shell 命令、OSC 标题、工作目录、焦点变化及配置重载均不会重命名窗口。编辑器始终
针对打开它的窗口，窗口关闭时取消。READONLY 模式下仍可使用命令面板和重命名窗口，
终端输入及不安全操作仍被阻止。

原生标题用于适用的系统窗口列表、预览及切换器。Windows 包括任务栏预览和 Alt+Tab；
macOS 包括窗口列表、Dock 窗口菜单及 Mission Control，不包括应用级 Cmd+Tab。
Linux 取决于 X11/Wayland 桌面。系统可能隐藏或截断标题；应用分组、图标、应用 ID
及 Dock 应用名称保持不变。

### 选择与复制文字

拖动可以按 cell 选择。双击选择单词，三击选择整行。双击或三击后继续拖动时，会按
完整单词或整行扩展。松开鼠标不会自动复制。

支持鼠标的终端程序可以请求左键和 drag motion。在这类 TUI 中，请从
**Shift-drag** 开始，以绕过 mouse reporting，让 SonicTerm 在本地选择文字。
Gesture owner 在第一次按下鼠标时确定，并保持到松开。

选好后使用当前平台的复制快捷键。在 alternate screen 中，显式复制成功后会清除
该选区并移除高亮。剪贴板写入失败时，只要选区仍有效，它就会保留，方便重试。
Primary screen 中复制成功后，选区仍保留。若选中 cell 只是按完全相同的字符、style、
hyperlink、宽字符结构和组合字符重新绘制，选区会保留；实际 cell identity 改变时，
SonicTerm 会在复制前清除它。终端程序也可通过 OSC 52 的 `c` target 写入最多 512 KiB
的 UTF-8 文字。剪贴板读取/查询、格式错误的 Base64、其它 selection target 和超限写入
都会被忽略。

READONLY 模式会在查看历史记录时阻止终端输入。方向键或 `h/j/k/l` 移动阅读光标；
`w/b`、`0/$`、`g` / `G` 分别按单词、行和 buffer 移动。按 `Escape`
退出。READONLY 不创建文字选区。搜索、切换标签页、切换 pane 焦点、检查更新和保存
当前字体设置、命令面板及重命名窗口仍可使用。完整控制与允许列表见 [快捷键](Keybindings-zh-CN)。

### rmux 与 tmux 集成

**推荐的 RMUX 基础配置（0.10.0）：** Windows、macOS 与 Linux 共用以下配置，再按
下文选择剪贴板策略。除非有明确需求，否则保留默认按键和鼠标绑定。

| 运行 RMUX 的主机 | 建议的用户配置文件 |
| --- | --- |
| Windows | `%USERPROFILE%\.rmux.conf` |
| macOS | `~/.rmux.conf` |
| Linux | `~/.config/rmux/rmux.conf` |

这些路径受支持，但不是完整的搜索顺序。其它已有 RMUX 配置或 tmux 配置后备也可能
提供设置。启动新服务时可用 `rmux -f <path>` 明确指定文件；不要假设改动文件就会
重新配置正在运行的服务。

```tmux
set -g default-terminal "tmux-256color"
set -as terminal-features ",xterm-256color:RGB:osc7"
set -g set-titles on
set -g mouse on
set -s extended-keys on
set -s extended-keys-format csi-u
set -s set-clipboard external
set -s copy-command ''
```

SonicTerm 向外层 PTY 提供 `TERM=xterm-256color` 与 `COLORTERM=truecolor`，由 RMUX
在 pane 内报告 `tmux-256color`；不要在 shell profile 中覆盖 `TERM`，也不要为了开启
功能而冒充其它终端。Unix 主机可用 `infocmp tmux-256color` 检查程序能否找到对应
terminfo；若缺失，应安装匹配的 terminfo，而不是修改外层终端身份。

`xterm-256color` 能力项描述的是 SonicTerm，不是内层 pane。RMUX 已识别它的扩展按键
能力。`set-titles on` 配合 `osc7` 可启用活动 pane 工作目录转发。

每个 pane 内的 shell 必须在工作目录变化时发出 OSC 7。rmux 会记录该报告，并在上述
两项设置都生效时把活动 pane 的路径发给 SonicTerm。`#{pane_current_path}` 是 rmux
format 使用的进程检查元数据；shell 没有报告时，rmux 不会用它代替 OSC 7。修改
`terminal-features` 后，请重新加载配置并 detach/reattach，让外层 client 重新解析能力，
然后显示一次新 prompt。

转发使相对路径使用准确窗格，并让主/子窗口的普通新标签页和分屏继承其 CWD。继承只接受
空主机、`localhost` 或准确本机主机名；原生绝对路径解码后 UTF-8 不超过 4,096 字节。
显式 CWD 优先，新窗口不继承。SonicTerm 可据此解析 `src/main.rs`、`./file` 和 bare name。
Windows 与 Linux 上，指向文字时按住 `Ctrl`；可打开目标会显示下划线，随后可以点击。
OSC 7 缺失、格式错误或声明远端 host 时，SonicTerm 仍会 fail closed：它不会从进程 CWD、
rmux status 元数据、其它 pane 或命名用户 home 猜测目录。绝对路径不依赖 OSC 7。

前台为 `rmux`、`tmux` 或 `screen` 时，即使已知 CWD，也优先显示非空原始 OSC 标题；
手动标题仍优先。其它进程保持普通的 CWD 优先自动标题。OSC 8 保留 URI 分号；OSC 133
`B` 结束提示符但不计时，`C` 开始执行，`A`/`D` 保持区域行为。这是有限范围的 shell 集成，
不表示完整 WezTerm 对等能力。

**不同主机的键盘路径：** 基础配置使用 `extended-keys on`，不是 `always`；内层应用还
需请求扩展按键报告。`csi-u` 选择的是 RMUX 发给内层应用的编码，不是 SonicTerm 的
全局编码。Windows 上 RMUX 读取原生控制台输入，SonicTerm 遵循 ConPTY 的 Win32 输入
请求；macOS 和 Linux 上 RMUX 通过 `modifyOtherKeys` 向外层终端请求扩展输入。
SonicTerm 中协商后的非零 Kitty flags 仍具有更高优先级。不要为了修饰键问题而全局
强制启用 Windows 控制台 VT 输入。外层协议规则见[终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)。

外层终端、multiplexer 和内层 TUI 是三个独立的输入与剪贴板层。第一次按下鼠标时
取得所有权的层，会一直持有完整 gesture 直到松开：

| Gesture 或复制路径 | 所有者 | 结果 |
| --- | --- | --- |
| 内层程序请求 mouse tracking 时的无修饰键 drag | 通过 rmux/tmux 交给内层程序 | 程序选区与程序控制的边缘滚动 |
| 内层未请求 mouse tracking 且 multiplexer mouse mode 已开启时的无修饰键 drag | rmux/tmux | Multiplexer copy-mode 选区 |
| mouse-down 前已按住 `Shift` | SonicTerm | 对当前已绘制 cell 建立本地终端选区 |
| Multiplexer copy 命令 | rmux/tmux | Multiplexer buffer 加已配置的系统/OSC 52 复制 |
| 内层程序发出 OSC 52 write | 内层程序，由 multiplexer 转发 | SonicTerm 写入原生剪贴板 |

若要让 rmux 采用兼容 tmux 的行为，应保留标准条件式 pane 绑定，不要强制所有 drag
都进入 copy mode：

```tmux
set -g mouse on
bind -n MouseDown1Pane { select-pane -t=; send -M }
bind -n MouseDrag1Pane { if -F '#{||:#{pane_in_mode},#{mouse_any_flag}}' { send -M } { copy-mode -M } }
```

这些绑定先选择 pane；内层 TUI 请求鼠标报告时转发，否则进入复制模式。只有内层 TUI 能在
边缘拖动时显示更多虚拟会话记录。无条件把 `MouseDrag1Pane` 绑定为 `copy-mode -M` 会让
multiplexer 接管滚轮与拖动，并可能滚入应用 live alternate screen 外的历史。

**剪贴板推荐：** 保留 `set-clipboard external` 与空 `copy-command`，让 RMUX 自己
发起的复制通过 OSC 52 到达 SonicTerm。这样不需要本机剪贴板工具；只要每层外部终端
支持转发，SSH 也可使用。`external` 会忽略 pane 内程序发出的剪贴板写入。若信任这些
程序，并希望它们自己的复制操作到达 SonicTerm，可选择：

```tmux
set -s set-clipboard on
```

这个选项允许 pane 输出替换你的剪贴板，但不表示默认还需 `allow-passthrough on`；
允许原始转义直通是另一项独立的信任决定。

对于**本机 RMUX 复制模式的管道动作**，可选用与 RMUX 所在主机相符的命令替换空
`copy-command`：

| 主机/会话 | 所需可执行程序 | `copy-command` 值 |
| --- | --- | --- |
| Windows | Windows PowerShell | 使用下方显式 UTF-8 命令 |
| macOS | `pbcopy` | `'pbcopy'` |
| Linux Wayland | wl-clipboard 提供的 `wl-copy` | `'wl-copy'` |
| Linux X11 | `xclip` | `'xclip -selection clipboard'` |

```tmux
# Windows：先将 RMUX 的原始 UTF-8 stdin 解码，再写入剪贴板。
set -s copy-command 'powershell -NoProfile -NonInteractive -Command "[Console]::InputEncoding=[Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())"'
# macOS：选择此项而不是上方 Windows 命令。
# set -s copy-command 'pbcopy'
# Linux Wayland：需要 wl-copy 及当前 compositor 的访问权限。
# set -s copy-command 'wl-copy'
# Linux X11：需要 xclip 及当前 DISPLAY 的访问权限。
# set -s copy-command 'xclip -selection clipboard'
```

管道命令运行在 RMUX 主机上，因此远端的 `pbcopy` 或 `wl-copy` 并不自动写入连接端
机器的剪贴板。SSH/无图形界面会话优先采用 OSC 52。Windows 上 `clip.exe` 或裸
`$input | Set-Clipboard` 可能通过控制台代码页解码 UTF-8，破坏框线字符、CJK、重音字符
和 emoji。

`copy-command` 用于没有显式命令的 `copy-pipe*` 动作；普通 `copy-selection` 不执行它。
OSC 52 与管道命令是独立效果，配置命令不会关闭 OSC 52。如果明确只需要本机命令，
另设 `set-clipboard off`，这会关闭该剪贴板转发。所有管道命令配置都必须可信。

排查方法：

- 若高亮只在按住鼠标时出现、松开即消失，先确认 press 归哪一层；支持鼠标的内层 TUI
  可能正在绘制自己的临时选区。
- 若 copy mode 滚出内层 TUI，请恢复条件式 `MouseDrag1Pane` 绑定，让内层程序持有 mouse
  tracking 与边缘滚动。
- 若可以选择但原生剪贴板不变，请用 `set-clipboard on` 开启可信 OSC 52 relay；若复制由
  multiplexer 持有，则配置 UTF-8 `copy-command`。
- mouse-down 前按住 `Shift` 可使用 SonicTerm 本地选区后备。它只能看到已绘制 cell，
  因此不能驱动内层程序的虚拟滚动。

**加载与检查：** 用 `rmux source-file <path-to-config>` 重新加载，再检查实际服务器选项。
若使用命名服务，请在每条命令中加入 `-L <name>`。

```sh
rmux show-options -g default-terminal
rmux show-options -g mouse
rmux show-options -s extended-keys
rmux show-options -s extended-keys-format
rmux show-options -s set-clipboard
rmux show-options -s copy-command
```

修改外层终端能力后请 detach/reattach；已有 pane 进程保留原环境，因此修改
`default-terminal` 后应在新 pane 中检查。验证 Shift+Enter 与 Enter、CJK 文本选择/复制、
目标 TUI 内滚轮行为，不要只依赖选项读回。选项和源码说明按 RMUX 0.10.0 核对；这些配置
示例不表示每个 tmux 版本都行为相同，也不表示已在每种主机上完成原生运行验证。

协议边界见[终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)，UTF-8 与剪贴板策略契约见
[RMUX 剪贴板指南](https://github.com/Helvesec/rmux/blob/dfd68c774ca0f4212139a21d37d09c90f75f8bd7/docs/human-friendly-config.md#copying-text)。

### 打开 URL 与本地目标

鼠标指向目标时，macOS 按住 `Cmd`，Windows 和 Linux 按住 `Ctrl`。有效目标会显示
下划线；点击即可打开。OSC 8 link 和普通文字中的 `http://`、`https://`、
`mailto:`、`file://` URI 优先于原始文件系统检测。无关终端输出和同值重绘不会让
未变化的目标闪烁；pointed row、target、CWD、viewport 或可打开 identity 改变时，
授权会被撤销并重新 probe。

普通悬停会以主题黄色提示为检测到的 URL 和 OSC 8 标签添加下划线；按住打开修饰键后改用
操作强调色。普通悬停不改变字形前景色；解析器暂时繁忙时不会移除未变化的提示。
悬停检测遇到锁忙会请求稍后的完整快照重绘，避免移动指针或改变 Cmd/Ctrl 后的反馈等待无关
终端输出。此规则适用于主窗口、子窗口以及 GPU 和软件渲染；点击仍须通过新的目标验证。
OSC 8 覆盖范围沿连续标签跨越自动换行，包括宽字符，但不会跨硬换行或间隔
连接另一次出现的链接。最多绘制八个可见片段；标签过长时仍保留指针所在片段。
正文圆括号或方括号中的 URL 也会被检测到，外层括号不会进入目标地址或下划线范围。
纯文本 URL 同样会跨已记录的终端右边界自动换行连接：指向任意片段都解析完整目标，并高亮
所有片段。重建要求完整逻辑行仍可见，且不超过 8 行和 4 KiB；不完整或超限的链保持不可操作，
不会打开截断的前缀。
应用插入硬换行的 HTTP(S) URL 也可连接，但协议前必须紧邻 `(` 或 `[`，且匹配的闭括号仍可见。
闭括号后直到下一个空白或行尾只能有普通句末标点；紧邻的 URL 文字会使边界有歧义并阻止重建。
完整 authority 与第一个路径斜杠必须出现在首次换行前；所有非末尾片段必须到达右边界，
续行必须使用一致且不超过 8 个 ASCII 空格的缩进。同样受 8 行和 4 KiB 限制。只移除缩进
与行边界，查询文字、百分号转义和连字符均原样保留。片段内空白、嵌套括号、不安全 cell、
混合换行类型及多个协议会阻止重建。已识别但不完整的片段不会回退到截断 URL。
没有外层括号的硬换行及本地路径不会连接；任意标签布局可通过 OSC 8 保留完整目标。

按住修饰键悬停时，本地目标只有通过当前文件系统探测验证后才显示预览。
待验证、不存在、有歧义或被拒绝的本地目标不显示预览，避免把目录列表字段显示为未经验证的路径。
预览不添加操作标签或错误信息，不更改剪贴板，也不授予打开权限。位置、转义、换行和隐藏
规则与 URL 预览相同。

在任意 URL 上按住同一修饰键，都会在指针旁预览目标，包括带标签的 OSC 8 链接、
标签与目标完全相同的链接，以及自动检测的纯文本 URL。按住修饰键并单击即可打开。
`&` 等查询分隔符原样保留，因此含多个查询参数的链接（包括仓库文件和行号链接）
可以打开。协议、长度、控制字符以及其他禁用字符的检查仍然有效。

终端下划线会跨越显式输出且具有相同下划线样式与颜色的空格。未设置下划线的单元格
仍会形成间隔；即使下划线模式处于开启状态，清除单元格也不会产生新的下划线。

预览不会访问网站，也不会授予打开权限。本地 file URI 同样需要路径验证；控制字符和方向格式字符
会转义后显示。失败原因在单击后的错误通知中说明，不会添加到预览。长目标自动换行，无法完整
容纳时明确显示省略号。松开修饰键或离开链接即隐藏；焦点、模态界面、pane、viewport
和内容变化会刷新或清除预览。GPU 与 Windows 软件渲染使用相同的预览覆盖层。

原始本地目标包括：

- `/usr/local/etc`、`C:/Users/name`、`C:\\Users\\name` 等原生绝对路径；
- `~/notes` 和 Windows `~\\notes` 等当前用户 home 路径；
- `src/main.rs` 和 Windows `src\\main.rs` 等带分隔符的相对路径；
- `./file`、`../file`、`../../file` 等显式相对路径；
- `sonicterm`、`.DS_Store`、`My Folder` 等上下文名称。

这些形式可以包含普通空格。相对形式和上下文名称要求准确 pane 通过 OSC 7 报告可信
本机绝对工作目录。OSC 7 缺失、格式错误或来自远端 host 时会 fail closed。
SonicTerm 不会改用进程工作目录、其它 pane 的目录或命名用户的 home。

后台 probe 最多检查 37 个候选，每个候选最多跨 8 个非空格部分。逻辑显示行重建同样有
4 KiB 和连续 8 行上限。SonicTerm 只跨已记录的终端右边界自动换行连接路径片段，并且要求
完整链仍在可见 viewport 内；所有片段共享同一授权与下划线。硬换行绝不会连接；第 9 行、
不可见边界或前驱已从 scrollback 淘汰时，整条链保持不可操作。

SonicTerm 会选择包含鼠标 cell 的最长、无歧义且可操作候选。对于 `src/main.rs,` 这类以正文
标点结尾的路径，会先探测标点属于文件名的合法字面候选。只有该字面文件不存在时，才会尝试
去掉末尾逗号、分号、句点、冒号、感叹号或问号的较短候选；此时下划线不包含正文标点。字面
候选被阻止或同长度候选有歧义时会 fail closed，不会回退。一个以原生绝对路径、当前用户 home
路径或点相对路径开头、内部不含空格的词，也允许首个符合条件的 Unicode Other Punctuation
字符分隔前面的路径和后面的正文。例如，`~/.claude.json，并将权限` 只有在完整字面文件确认
不存在、较短路径确认可操作后，才会解析为 `~/.claude.json`。可操作范围不含分隔符和正文，
但这些单元格仍参与安全验证。Unicode 文件名字符保持原样，路径语法字符不作为分隔符。
这不会增加带空格路径的正文切分规则，也不改变 `./`、`../` 对 OSC 7 的要求；当前用户 home
路径不要求 OSC 7。

`ll` 输出的完整独立单引号上下文名称，例如 `'My Folder'`，会按 `My Folder` 处理。显式路径也支持一对完整的单引号、双引号
或反引号，包括 `'C:\work\My Folder'` 这样的带空格路径。引号不属于可操作范围，但仍参与
cell 安全检查。引号内容按字面处理，不执行 shell 反转义或变量展开。不配对或混合引号、
拼接文字、首尾填充空格、`$`/`%` 展开语法、其它带引号的裸名称、`ls -F` 后缀
（`*`、`@`、`=`、`|`），以及含破损宽字符配对、组合附加字符、控制字符或已属于 OSC 8 的 cell
的原始路径都保持不可操作。有效宽字符配对保留准确文件名和单元格范围。

终端消息中的文件引用可以直接操作，包括 `Update(src/main.rs)` 或 `Read(./notes.txt)`
这类括号完整、名称为标识符的工具标题。内部路径不包含工具名称和外层圆括号。
带括号的路径与源文件位置引用也允许后接句末标点，例如 `(src/main.rs:97).`、
`[src/main.rs:97:4],`、`{src/main.rs};` 或 `Read(src/main.rs:97–100)!`。
可操作范围不包含外层括号和标点；括号内部的标点仍遵循字面文件名优先的探测规则。
成对结构先于外围正文识别：`(reports/flight.html)，内容` 和
`【reports/flight.html】，内容` 保留同一个内部目标。支持 `()`/`[]`/`{}`、ASCII
引号/反引号、`（）`/`【】`/`《》`/`「」`/`『』`、`“”`/`‘’`/`«»` 及标识符式工具调用。
外围分隔采用 Unicode 的 Other Punctuation 和 Dash Punctuation 类别，不维护逐语言的标点清单。
路径分隔符、错配闭括号、直接拼接，以及 `(src/main.rs).bak` 这类点号/冒号后缀续接，
仍因有歧义而不可操作。这不会解释原始 Markdown 或删除不可见字符。有效宽字符的两格都参与
安全验证，包括文件名内容。边界缺失或不安全时，不能回退到较短的内部片段。

`path=C:\work\file.exe` 和 `file="C:\My Folder\report.md"` 这类绝对路径字段的可操作范围
不含字段名和配对引号。相对赋值、拼接或未闭合引号不适用该规则。文件名内部的 `=` 保持字面含义。

`src/a.rs、b.rs` 这类无括号文件列表先保留完整字面文件名。只有确认它不存在，才允许指针所在
文件成员；被阻止或有歧义的字面候选不授权较短成员。第二个名称只在该 pane 的 CWD 中解析，
不会推测它属于 `src`。连字符不是列表分隔符。完整字面不存在的要求不会因候选上限被丢弃，
并在原生操作前重新检查。
在 `src/main.rs and focused tests/main.rs. Require stable` 这类正文中，分别指向两个
文件名即可独立解析。`and` 不是保留词：真实文件名中的空格与圆括号仍通过字面文件系统
候选消除歧义。缺失的上下文路径使用指向的文件名反馈，不使用未经验证的多词正文猜测。
未经验证的带根路径仅在最后一个组成部分具有扩展名、且前面的类文件名词不会使范围
产生歧义时保留空格；其它多词猜测需要验证。验证通过的文件始终保留完整路径。

`install.sh:889–919`、`src/main.rs:12` 和 `src/main.rs:12:4` 等源文件引用保留完整下划线，
但只解析文件名。行号与列号必须为正数；范围接受 `-` 或 `–`，且终点不能早于起点。
相对源文件名仍要求准确 pane 的可信 CWD。Windows、macOS 和 Linux 都在所在文件夹中
选中引用的文件。行号不启动编辑器，也不限制文件类型或内容。
`(src/main.rs:924, :934, :375).` 这样的分组引用共用一个显式路径，最多包含八个有效位置。
指向某个位置时选择对应元数据，指向文件名时选择第一个位置。整个分组统一显示下划线并
参与验证；分隔符和外围标点不触发导航。任何无效成员都会使整个分组失效。分组锚点使用
不带引号和空格的路径；单独的带引号引用支持空格。带空格锚点的分组位置引用整体不可操作，
包括锚点本身，不会回退到较短文件名。不加括号的分组必须位于片段开头或紧随另一个完整分组；
正文之后请用 `()`/`[]`/`{}` 明确锚点边界。普通文字之后的括号会按正文分隔处理，
不会作为带空格相对文件名的延续。

### PowerShell 目录链接

由 SonicTerm 启动的交互式 PowerShell 7.2 及以上版本，会给默认目录显示附上完整本地文件 URI。
名称折到多行时，每段保留相同目标；缩进和后面的目录项不带该链接。保留模式、日期、长度及名称
颜色。`ls` 仍是 `Get-ChildItem`，对象流水线不变；显式 `Format-Table` 使用原生视图。
纯文本和重定向输出不带终端装饰。

集成只在当前进程生效，嵌入 SonicTerm，不写 profile 或格式文件。自定义文件名 getter 或非标准
文件视图不会被覆盖；旧版 PowerShell 与受限语言环境保留原格式。已有输出不会重写。
本地目标仍需验证；点击可执行文件只在文件夹中选中，不执行它。

### 手工链接检查

在 Windows 的 SonicTerm 中，从仓库根目录运行 `./scripts/test-local-link-actions.ps1`。
它打印带编号、预期预览与点击结果的例子，并在独立临时目录创建无害文件；不会自动打开目标、
修改配置或写入剪贴板。测试后删除输出中注明的夹具目录。

用 `-Group Web`、`Osc8`、`Paths`、`Wrappers`、`Source`、`Negative`、`Wrapping`
或 `KnownGaps` 可逐组检查，默认是 `All`。已知缺口单独标注，不冒充已支持的行为。
检查换行时调整窗口宽度，并保持完整目标可见。反馈时提供分组、例子编号、预览和实际点击结果。
这份手工矩阵补充 scanner/app 回归测试，不代表对任意文本的穷尽证明。

### 本地目标行为

Windows/Linux 使用 Ctrl+单击，macOS 使用 Cmd+单击。

| 目标或状态 | 预期行为 |
| --- | --- |
| 存在的普通目录 | 在文件管理器中进入该目录。 |
| 存在的普通文件，包括脚本、可执行文件、安装包和快捷方式文件 | 打开所在文件夹并选中文件本身；不执行、不跟随快捷方式、不调用关联应用。 |
| macOS 应用或软件包目录 | 在 Finder 中选中软件包，不启动它。 |
| 列表或文字中存在的裸文件名 | 根据准确窗格的可信本地工作目录解析；仅给验证后的文件名加下划线并选择，保留文件名中的空格。 |
| 未验证的裸名称或普通文字 | 无预览、文件操作、错误通知或剪贴板写入；列表元数据不是文件路径。 |
| 不存在、待验证或有歧义的显式文件路径 | 按修饰键单击后显示路径及缺失、待验证或不明确的原因；第一次不导航、不复制。 |
| 被拒绝的显式路径 | 显示路径和拒绝原因，不绕过身份或本地性检查。 |
| 文件管理器操作第一次失败 | 显示尝试的路径、原因和再次单击复制的提示，保留剪贴板。 |
| 错误仍显示时再次按修饰键单击同一失败路径 | 复制该路径而非重试，并报告复制成功或失败。 |
| 错误已关闭、过期或被替换 | 后续单击开始新尝试，而非确认之前的错误。 |
| HTTP/HTTPS 或邮件 URL | 保持 URL 预览和浏览器或邮件导航行为。 |

显式路径包括本机绝对路径、`./`、`../`、`~/` 路径、带分隔符的相对路径、源位置引用，
以及本地 file URI 或本机路径 OSC 8 目标。显式路径保留文件名中的空格。对于未经验证、
以裸文件名为基础的源位置引用，失败提示排除周围正文；含空格裸文件名需要文件系统验证，
或通过显式路径/OSC 8 目标指定。裸名称只有通过文件系统验证后才成为文件路径目标。
文件扩展名、执行权限和文件内容不会阻止选中文件。符号链接、重解析点、特殊设备和不支持的
远端或网络路径仍受保护。各平台在调用前重新验证目标身份和类型。
所有平台都进入目录，或打开文件所在文件夹并选中文件，
不调用文件关联的应用。Windows 使用 `SHOpenFolderAndSelectItems`，Finder 使用
`/usr/bin/open -R -- <target>`，Linux 使用 `org.freedesktop.FileManager1.ShowItems`。
选择功能不可用或被拒绝时会在发起请求的窗口报告失败，不会回退到打开文件或仅打开父目录。
只有已验证的本地目标才调用原生文件操作。显式路径失败会显示单击触发的反馈；裸名称猜测不会。
操作第一次失败时，通知显示尝试的文件路径、原因，以及再次单击同一链接以复制的提示，
不会更改剪贴板。错误仍显示时，再次按住修饰键单击同一失败路径才复制，不重试打开，
随后报告复制成功或失败。通知关闭、过期、被替换或单击其他目标后，不确认之前的失败。
短通知按实际整形后的最长行收缩，保留换行并按 Unicode 字素换行；超过窗口空间的内容
明确以省略号标记。悬停不复制。原生错误只返回原始窗格仍属于的发起窗口。
不按修饰键的单击保留正常选择行为。目录仍使用各平台
现有的目录打开方式。基于路径的文件管理器请求在重新验证后仍存在通常的 pathname race。

OSC 8 中的本机绝对路径（包括 Windows `C://…`）、Windows 驱动器绝对路径 `file:c://…`
和本地 `file://` URI 使用相同的文件系统授权流程。本地 `#3`、`#L3` 和升序 `#L3-L7`
片段属于源文件行号信息，不属于文件名；选择操作仅使用文件路径。本机绝对路径 OSC 8
目标也遵循此约定。若文件名确实以 `#3` 或 `#L3` 结尾，请使用以 `%23` 表示井号的 file URI。
纯文本本机路径扫描仍保留原有的字面井号行为。file URI 转义只解码一次；本机路径中的百分号保持字面含义。远端 authority、
UNC/设备路径以及格式错误的目标会被拒绝，不会以显示标签替代。HTTP/HTTPS 和邮件链接
保持原有行为。URI 打开器本身只接受 `http`、`https` 和 `mailto`，并拒绝所有 `file:` URI，
因此 file 链接只能经由这一授权流程访问文件系统。

Windows 上打开 URI 与打开已验证本地目标使用同一条不经过 shell 的边界：`ShellExecuteExW`
以单个 NUL 结尾的 UTF-16 字符串接收 URI，不会有任何命令解释器解析它。环境变量替换保持关闭，因此
`https://example.com/%20space` 这类以百分号分隔的 URI，或包含 `%USERNAME%` 的 URI，会按
屏幕上显示的原样交给浏览器或邮件客户端，不会展开成环境变量的值。

设置 `terminal.clickable_bare_names = false` 可以关闭上下文名称。设置
`terminal.clickable_local_targets = false` 可以关闭所有原始本地目标，也包括本地 file URI
和本机路径 OSC 8 链接；网页和邮件 URI 链接不受影响。准确默认值和重载行为见 [配置](Configuration-zh-CN)。

### 以草稿方式打开脚本

安装后的 macOS build 可以出现在 Finder 为 `.sh`、`.command`、`.tool` 提供的
**Open With** 菜单中。Windows MSI 会把 SonicTerm 注册为 `.ps1`、`.cmd`、`.bat`、
`.sh` 的可选 handler。安装不会替换当前默认 handler。

打开受支持文件时，SonicTerm 会在文件父目录创建标签页，安全引用绝对路径，并把命令
放到提示符中，**不会发送 Enter 或其它 control byte**：

- POSIX `sh`、`bash`、`zsh`、`dash`、`ksh`：`.sh`、`.command`、`.tool`；
- PowerShell 或 `pwsh`：`.ps1`、`.cmd`、`.bat`；
- Command Prompt：`.cmd`、`.bat`，且路径不能包含 `%`、`!`、`"`。

请自行检查、修改、提交或清空草稿。未知 shell、不支持的 shell/script 组合、相对路径、
非 Unicode 路径、control character 或不安全的 Command Prompt 路径，仍会打开标签页，
但只显示 warning，不插入命令。

这是输入草稿功能，不是 sandbox。Shell startup file 会先运行，也可以读取 PTY 输入。
如果启动配置会读取并执行输入，即使没有 Enter，也可能执行或吞掉草稿。使用这类配置时，
不要把 SonicTerm 设为脚本 handler。对于这个 open action，SonicTerm 自身不会运行脚本
或解释器。

Windows 中，每次文件关联调用都会启动新的 SonicTerm 进程。macOS 会把后续 open request
发送到当前运行的应用，并添加标签页。

### 配置与排障

详细规则由以下页面维护，这里不重复：

- 偏好、默认值、重载与保存：[配置](Configuration-zh-CN)
- 快捷键、action 与 READONLY 控制：[快捷键](Keybindings-zh-CN)
- 主题 schema 与颜色：[主题](Themes-zh-CN)
- 日志、crash 文件与诊断：[日志](Logging-zh-CN)
