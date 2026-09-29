# SonicTerm 百科

[English](Home)

SonicTerm 是面向 macOS、Windows 和 Linux 的原生 GPU 加速终端。
**先读[用法](Usage-zh-CN)**，完成安装并熟悉日常操作。

### 用户手册

- [用法](Usage-zh-CN) — 安装、新建标签页、分屏、检测终端消息中的文件引用和选取文字
- [终端复用器](Terminal-Multiplexers-zh-CN) — 设置 tmux、rmux、GNU screen 与 Zellij，让链接、路径、按键和剪贴板正常工作
- [配置](Configuration-zh-CN) — 修改 `~/.sonicterm/sonicterm.toml`、重载与保存
- [快捷键](Keybindings-zh-CN) — 查找快捷键或编写绑定
- [主题](Themes-zh-CN) — 选择或创建配色
- [日志](Logging-zh-CN) — 查找日志、排查问题和准备缺陷报告
- [内存](Memory-zh-CN) — 理解资源上限与常驻内存报告

### SonicTerm 如何工作

先用[架构](Architecture-zh-CN)了解全貌，再读[从按键到像素](From-Keypress-to-Pixel-zh-CN)，
跟随一个 `A` 完成往返。需要深入某个子系统时查阅下列参考。

- [架构](Architecture-zh-CN) — 系统结构与 crate 边界
- [从按键到像素](From-Keypress-to-Pixel-zh-CN) — 输入、子进程输出、网格、字形与像素
- [运行时生命周期](Runtime-Lifecycle-zh-CN) — 启动、所有权变化、标签页转移与退出
- [终端 IO 与 VT](Terminal-IO-and-VT-zh-CN) — PTY、解析器协议、shell 集成与网格规则
- [渲染模式](Rendering-Modes-zh-CN) — 适配器选择与帧节奏
- [渲染与字体](Rendering-and-Fonts-zh-CN) — 字体、图集、GPU/CPU 绘制与损伤区域
- [平台集成](Platform-Integration-zh-CN) — AppKit、Win32、X11 与 Wayland 边界
- [架构内部机制](Architecture-Internals-zh-CN) — 正确性、记账与生命周期不变量
- [Crate 参考](Crate-Reference-zh-CN) — 全部 23 个 crate、接口与依赖

### 构建与贡献

- [打包](Packaging-zh-CN) — 本地生成安装包并查看布局
- [开发与发布](Development-and-Release-zh-CN) — 本地 gate 表、coverage 证据与开发页面索引
- [仓库与工具链](Repository-and-Toolchain-zh-CN) — 仓库布局、工具链、构建入口、代码约定与原生依赖
- [本地 gate](Local-Gate-zh-CN) — gate runner 如何执行每个步骤
- [CI 与 Coverage](CI-and-Coverage-zh-CN) — pull-request 与 `main` CI、gate 盲区与工作流供应链
- [发布流程](Release-Process-zh-CN) — tag 驱动的 release、发布资产与已解决 issue 的来源证据
- [Wiki 发布](Wiki-Publication-zh-CN) — Wiki 源码规则与每次合并后的发布
- [代码所有权](Code-Ownership-zh-CN) — 各路径所属的平台区域，以及 agent 如何认领工作
- [首页](Home-zh-CN) — 返回本索引
