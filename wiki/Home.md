# SonicTerm Wiki / SonicTerm 百科

## English

SonicTerm is a native, GPU-accelerated terminal for macOS and Windows. This
wiki contains both the user guide and a source-backed explanation of how the
project works internally.

Memory is a first-class concern: every subsystem that can grow has a ceiling,
the ones that matter are process-wide rather than per-pane, and anything
SonicTerm discards that you could see is reported in the log at the default
level. [Memory](Memory) covers what is bounded and how to read it.

### User guide

- [Usage](Usage) — installation, panes, tabs, READONLY mode, and common actions
- [Configuration](Configuration) — `~/.sonicterm/sonicterm.toml` and reload behavior
- [Keybindings](Keybindings) — bundled keymaps, binding syntax, and actions
- [Themes](Themes) — bundled palettes and custom theme authoring
- [Logging](Logging) — logs, crash dumps, performance diagnostics, and bug reports
- [Memory](Memory) — what SonicTerm bounds, what it discards, and how to read it from the log

### How SonicTerm works

- [Architecture](Architecture) — system boundaries, ownership, dependencies, and invariants
- [Runtime Lifecycle](Runtime-Lifecycle) — startup, event dispatch, windows, tabs, panes, and shutdown
- [Terminal IO and VT](Terminal-IO-and-VT) — PTYs, optional SSH, VT parsing, grids, scrollback, and mux status
- [From Keypress to Pixel](From-Keypress-to-Pixel) — how one typed `A` travels through the child program, grid, font stack, and renderer
- [Rendering and Fonts](Rendering-and-Fonts) — damage, glyph shaping, atlases, wgpu, and software rendering
- [Platform Integration](Platform-Integration) — AppKit/macOS and Win32/Windows boundaries
- [Crate Reference](Crate-Reference) — all 23 workspace crates and their relationships
- [Development and Release](Development-and-Release) — tests, CI, packaging, and the `v*` release pipeline

This wiki is the canonical documentation surface: architecture and invariants
in [Architecture](Architecture) and
[Architecture Internals](Architecture-Internals), the crate map in
[Crate Reference](Crate-Reference), diagnostics in [Logging](Logging), and
build procedures in [Packaging](Packaging).

## 中文

SonicTerm 是一个面向 macOS 和 Windows 的原生 GPU 加速终端。本 Wiki 同时包含
用户手册，以及基于源码整理的项目内部工作原理说明。

内存是首要关注点：每个可能增长的子系统都有上限，其中最关键的是进程级而非按
面板计的限制，并且 SonicTerm 丢弃的任何可见内容都会在默认日志级别下记录。
[内存 / Memory](Memory) 说明了限制范围以及如何查看。

### 用户手册

- [用法 / Usage](Usage) — 安装、窗格、标签页、READONLY 模式和常用操作
- [配置 / Configuration](Configuration) — `~/.sonicterm/sonicterm.toml` 与配置重载
- [快捷键 / Keybindings](Keybindings) — 内置键位映射、绑定语法和 action
- [主题 / Themes](Themes) — 内置配色和自定义主题
- [日志 / Logging](Logging) — 日志、崩溃转储、性能诊断和问题报告
- [内存 / Memory](Memory) — SonicTerm 限制什么、丢弃什么，以及如何从日志中查看

### SonicTerm 如何工作

- [架构 / Architecture](Architecture) — 系统边界、状态所有权、依赖关系和不变量
- [运行时生命周期 / Runtime Lifecycle](Runtime-Lifecycle) — 启动、事件派发、窗口、标签页、窗格与退出
- [终端 IO 与 VT / Terminal IO and VT](Terminal-IO-and-VT) — PTY、可选 SSH、VT 解析、网格、回滚缓冲与 mux 状态
- [从按键到像素 / From Keypress to Pixel](From-Keypress-to-Pixel) — 一个输入的 `A` 如何经过子程序、网格、字体栈与渲染器
- [渲染与字体 / Rendering and Fonts](Rendering-and-Fonts) — 损坏区域、字形塑形、图集、wgpu 与软件渲染
- [平台集成 / Platform Integration](Platform-Integration) — AppKit/macOS 与 Win32/Windows 边界
- [Crate 参考 / Crate Reference](Crate-Reference) — 工作区全部 23 个 crate 及其关系
- [开发与发布 / Development and Release](Development-and-Release) — 测试、CI、打包与 `v*` 发布流水线

本 Wiki 即规范文档面：架构与不变量见 [Architecture](Architecture) 与
[Architecture Internals](Architecture-Internals)，crate 映射见
[Crate Reference](Crate-Reference)，诊断见 [Logging](Logging)，
构建步骤见 [Packaging](Packaging)。
