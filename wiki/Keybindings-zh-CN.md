# 快捷键

[English](Keybindings)

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

命令面板提示来自当前 keymap 中第一个匹配的绑定。macOS 显示 `⌘⇧P` 等修饰键符号；
Windows 使用 `Win+Shift+P` 等名称，Linux 使用 `Super+Shift+P`。Windows/Linux 上的
Control 和 Alt 显示为 `Ctrl`、`Alt`。字面 `+` 按键会保留，例如 `Alt++`。
这只改变提示文本，不改变按键匹配或现有搜索别名。

命令标签、占位符、空结果文本、重命名/颜色选择提示和底部提示随当前英文、中文或日文语言设置显示。
搜索匹配本地化标签、英文标签与别名，
以及当前快捷键提示。缺少翻译时回退到英文。语言或 keymap 重载会保留查询、光标，
以及仍然匹配的已选命令；重命名文本和颜色选择保持不变。具体绑定动作保留参数的字面值。

命令行分配 25 逻辑像素，带详情时再增加 16 像素，行间距为 8 像素，水平文字内边距为 12 像素。
标签与副标题组成垂直居中的文字块，内部间距为 4 像素。使用 13/12 像素字号时，
文字 em 框上下各保留相等的 6 像素边距。
页脚高 42 像素，左右内边距均为 18 像素；面板首选高度为 440 像素，仍受视口限制。
分类/不可用原因副标题与页脚复用比命令标签小一号的原生字体。副标题、快捷键提示和页脚文字
采用较淡颜色；命令标签字号不变。

查询为空时，命令按固定分类顺序排列，并保留各分类内的相对顺序。输入查询后仍按模糊匹配分数排序。
每行显示分类，不可用时显示原因。不可用命令仍可搜索；在该行按 Enter 不会执行动作，也不会关闭面板。
上下文来自面板附着的窗口，而不是其他窗口的标签页数量。缺少标签页、窗格、选区、方向相邻窗格，
以及 READONLY 限制都会显示原因。复制要求非空选区且所选单元格仍然匹配；解析器忙时暂时禁用复制，
而不阻塞 UI。同值重绘保留有效选区。

**关于 SonicTerm** 仅作为面板条目提供，没有默认快捷键。搜索 `about`、`SonicTerm`、
`version` 或本地化关键词（例如“关于”“版本”），选择 **关于 SonicTerm** 结果后按 Enter。
这会关闭面板，并在原窗口已有的绿色通知中显示 `SonicTerm <version>`，包括预发布后缀。通知会替换当前气泡，并在五秒后自动
关闭，也可点击其关闭按钮。READONLY 模式下也可使用，不会向终端发送输入。

**Go to Tab** 按标题或显示位置搜索附着窗口的实时标签页，READONLY 下也可用。选择跟随
标签页身份跨越重排和重命名，激活前再次验证。目标关闭或不再匹配时清空选择；移动选择或修改
查询前，Enter 不执行动作。同名/同位置的替代项不会继承选择。

当标签页无法按字体/缩放推导的可读宽度全部放入栏中时，标签栏显示包含活动标签页的区段，
并在右侧显示溢出控件。点击控件会在同一窗口打开 **所有标签页**。搜索标题或位置，
使用方向键和 Enter，或点击行进行切换。现有的上一个/下一个和编号快捷键仍然有效；
切换后所选标签页会显示在栏中。极窄窗口以较小的点击区域保留活动标签页和溢出控件。
选择器为空时会说明没有可用标签页。在 READONLY 中通过指针打开选择器后，主窗口和子窗口都
由选择器接收键盘输入；Escape 关闭选择器但不退出 READONLY。

只有按下和释放都指向同一条目且实时上下文仍允许执行时，面板行才会激活。
重排或关闭标签页不会把按住的点击重定向到替代标签页。点击外部在释放时关闭面板；
点击查询区域不会执行动作。滚轮移动选择，在两端不循环；模态面板拥有指针时不会把滚轮事件
发送到终端。IME 组合输入期间不激活行。面板打开前已开始的终端、标签页、滚动条或分隔线
手势继续接收其配对释放事件。

标签页拖放使用可见标签在完整列表中的位置，而不是区段内位置。拖到可见区段末尾空隙会在该处插入；
拖到溢出控件会追加到完整标签列表末尾。普通点击该控件打开选择器，不开始标签拖动。

### 重命名窗口

`rename_window` 在当前窗口打开**重命名窗口**，没有默认快捷键。只编辑自定义名称；
Enter 去除首尾空白并保存，留空重置，Escape 取消。标题为 `#N SonicTerm` 或
`#N Name`。支持 Unicode、输入法及已配置的粘贴快捷键；控制字符、
换行及去除首尾空白后超过 128 个 Unicode 标量值的名称会显示拒绝原因。编辑器始终
绑定原窗口，包括 READONLY 模式下也不会向 PTY 或广播目标发送内容。编号在进程内
唯一、永不复用且不持久保存；名称不随标签页或终端输出改变。系统列表可能隐藏或截断
标题；macOS Cmd+Tab 仍按应用切换。生命周期及各平台显示限制见[用法](Usage-zh-CN)。

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

命名按键使用 `enter`、`backspace`、`tab`、`escape`、`space`、`up`、`down`、
`left`、`right`、`home`、`end`、`pageup`、`pagedown`、`insert`、`delete`、
`menu`、`pause`、`printscreen`、`scrolllock`、`numlock`、`capslock`，以及
`f1` 到 `f35`。可打印按键使用其字符。带 Shift 的 ASCII 标点也会匹配未移位写法，
因此系统报告为 `{`、`}` 或 `+` 的事件可分别匹配 `shift+[`、`shift+]` 或
`shift+=`；移位后的字面写法仍可作为别名。

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
| 窗口与应用 | `new_window`、`rename_window`、`move_tab_to_new_window`、`toggle_fullscreen`、`quit_app` |
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

`enter_copy_mode` 会在终端光标处进入 READONLY 模式。它阻止向该窗口终端发送新的用户输入，
也不会创建选区。READONLY 窗口不会接收广播输入。
滚轮只滚动本地视图，不发送鼠标报告或方向键；备用屏幕没有本地滚动历史。
即使终端程序启用了鼠标跟踪，新的鼠标按下和未按住按键的移动也留在本地处理。
文件拖放会被直接消耗，不向终端或广播接收端发送路径。

进入 READONLY 前已接受的按键仍向原目标发送重复和释放事件；先前开始的鼠标操作
也保留原来的释放目标。终端回复和焦点报告仍会到达 PTY。以下本地控制仍可使用：

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

READONLY 还允许执行切换或激活标签页、切换 pane 焦点、打开搜索或命令面板、重命名窗口、检查更新、保存
当前字体设置的 keymap action。搜索框仍可编辑，也可使用已配置的粘贴快捷键；无论窗口可写还是处于
READONLY，搜索粘贴都不会发送给任何 PTY 或广播窗格。除本地文本框接管的输入外，未在上文列出的
其它已绑定 action 会被直接拦截，不执行，也不会发送给 PTY。

`enter_quick_select` 会用 `a` 到 `z` 标记当前屏幕上最多 26 个 URL。按对应字母
可复制 URL 并关闭 overlay。按 `Escape` 取消。

### 应用文本框

搜索、命令面板筛选及标签页/窗口重命名使用同一套单行编辑控制。只有应用文本框接管输入时，
以下精确组合键才生效：

| 按键 | 行为 |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | 移到开头 / 结尾 |
| `Ctrl+B` / `Ctrl+F` | 向左 / 向右移动一个 Unicode 字符 |
| `Ctrl+H` / `Ctrl+D` | 向后 / 向前删除一个 Unicode 字符 |
| `Ctrl+W` | 删除左侧空白，再删除前一个连续非空白片段 |
| `Ctrl+U` / `Ctrl+K` | 删除开头到光标 / 光标到结尾 |
| `Left`、`Right`、`Home`、`End`、`Delete` | 标准光标移动和向前删除 |

带修饰键的 Backspace 遵循相应平台的文本框编辑约定：

| 平台 | 按键 | 行为 |
| --- | --- | --- |
| macOS | `Option/Alt+Backspace` | 删除到 AppKit 判定的前一个单词边界 |
| macOS | `Cmd+Backspace` | 删除行首到光标的内容，保留光标右侧文本 |
| macOS | `Ctrl+Backspace` | 删除前一个字素的一个规范分解成分 |
| Windows/Linux | `Ctrl+Backspace` | 删除左侧空白，再删除前一个连续非空白片段 |

Option 删除使用 AppKit 的纯字符串单词边界 API，而不是根据 `Ctrl+W` 的纯空白边界猜测
macOS 的标点和语言规则。光标位于末尾时，`foo/bar!!!` 会变成 `foo/`，`保留你好` 会变成
`保留`。原生 UTF-16 边界会精确转换为 UTF-8 光标；若边界落在代理对内部或超出光标，
则拒绝操作，不删除文本。
分解删除会把 `é` 和 `e` 后接组合锐音符两种写法都变成 `e`，把 `ấ` 变成 `a` 加组合抑扬符
（`U+0061 U+0302`）。结果保留规范分解形式，不会重新合成，也不改动其余文本。与 AppKit
一样，从组合 emoji 删除最后一个图形后，会保留前面的图形及末尾 `U+200D` 连接符；
下一次分解删除才删除该连接符。

普通 Backspace 也接受单独的 Shift，因此输入大写字母后立即删除仍能删掉一个字符。
命令修饰键组合必须精确匹配。额外的 Shift、Alt、Control 或 Super 不会继承其它删除快捷键，
Windows/Super 键也不会继承 macOS Command 的编辑语义。IME 正在组字时保留输入所有权，
不会把这些操作应用到已提交的文本框内容。

没有 SonicTerm 文本框接管输入时，这些按键保留终端编码；SonicTerm 不会猜测目标是
shell、Vim 还是 tmux。默认旧式模式下，Option/Alt+Backspace 发送 Meta-DEL（`ESC` 后接
`0x7f`），Ctrl+Backspace 发送 `0x08`（DECBKM 会对调 Backspace 与 Control-Backspace），
未绑定的 Cmd/Super+Backspace 保留为独立编码的组合键。已协商的 Kitty、MOK 和 Win32 输入
继续遵守各自协议。由终端应用决定这些字节执行何种删除或是否删除，不会统一重映射成
`Ctrl+W` 或 `Ctrl+U`。`Ctrl+<字母>` 同样继续发送给 PTY。

可打印输入来自操作系统的 `KeyEvent.text`，因此 Unicode 键盘布局以及 Option/AltGr
组合生成的字符会按系统结果插入。Super 以及普通 Control、Alt 或 Ctrl+Alt 命令组合不会
变成文本框内容。只有 Option/Alt 或 AltGr 生成的字符不同于布局解析出的未修饰按键时，
才会把它视为组合输入；上表中明确列出的 Control 编辑组合仍优先执行。

方向键、功能键等具名非文本按键始终保留终端协议编码，即使 macOS 在原生事件中
附带了私用区字符。这些字符既不会插入应用文本框，也不会作为 Kitty 关联文本上报。
空格、组合输入以及真正的私用区字符输入仍按文本处理。

### 数字小键盘

默认 `[terminal].keypad_mode = "auto"` 时，操作系统解析为数字字符的物理小键盘数字键
保持普通数字文本，即使 `ESC =` 已启用应用小键盘模式。运算符、Enter 和非文本小键盘
导航保留协商的应用小键盘映射。操作系统逻辑数字文本不等于直接测量硬件 NumLock。

如果 shell 启用了应用小键盘模式，却不接受其运算符或 Enter 序列，可显式选择普通数字
输入并重载配置：

```toml
[terminal]
keypad_mode = "numeric"
```

此设置仅覆盖旧式输入的 DECKPAM：运算符使用普通文本规则，小键盘 Enter 使用 Return
的修饰键/newline 规则，导航遵循操作系统逻辑按键。数字键不变。依赖独立旧式小键盘映射
的程序应保留 `auto`。设置适用于主窗口、子窗口和广播目标，不改变终端已保存的模式。
已协商的 Kitty 编码保持不变。

### 加载失败

启动时，如果 TOML 无效或缺少 `[meta]`，SonicTerm 会回退到当前平台的内置 keymap。
重载时遇到同样错误，会继续使用内存中的当前 keymap。结构正确的 keymap 会按 binding
处理错误 action：SonicTerm 记录 warning，只跳过该 binding，并保留其它 binding。
重载成功后，命令面板中的快捷键提示也会更新。
