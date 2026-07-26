# SonicTerm Wiki / SonicTerm 百科

## English

SonicTerm is a native, GPU-accelerated terminal for macOS and Windows. This
wiki contains both the user guide and a source-backed explanation of how the
project works internally.

### User guide

- [Usage](Usage) — installation, panes, tabs, READONLY mode, and common actions
- [Configuration](Configuration) — `~/.sonicterm/sonicterm.toml` and reload behavior
- [Keybindings](Keybindings) — bundled keymaps, binding syntax, and actions
- [Themes](Themes) — bundled palettes and custom theme authoring
- [Logging](Logging) — logs, crash dumps, performance diagnostics, and bug reports

### How SonicTerm works

- [Architecture](Architecture) — system boundaries, ownership, dependencies, and invariants
- [Runtime Lifecycle](Runtime-Lifecycle) — startup, event dispatch, windows, tabs, panes, and shutdown
- [Terminal IO and VT](Terminal-IO-and-VT) — PTYs, optional SSH, VT parsing, grids, scrollback, and mux status
- [Rendering and Fonts](Rendering-and-Fonts) — damage, glyph shaping, atlases, wgpu, and software rendering
- [Platform Integration](Platform-Integration) — AppKit/macOS and Win32/Windows boundaries
- [Crate Reference](Crate-Reference) — all 23 workspace crates and their relationships
- [Development and Release](Development-and-Release) — tests, CI, packaging, and the `v*` release pipeline

### Project evolution

- [Architecture Evolution](Architecture-Evolution) — evidence-classified proposals for making the architecture easier to maintain

The tracked canonical developer documents remain
[`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md),
[`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md),
[`docs/LOGGING.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/LOGGING.md),
and [`docs/packaging/`](https://github.com/D0n9X1n/SonicTerm/tree/main/docs/packaging).
The architecture pages in this wiki explain those boundaries in more detail;
they do not replace the canonical invariants.

## 中文

SonicTerm 是一个面向 macOS 和 Windows 的原生 GPU 加速终端。本 Wiki 同时包含
用户手册，以及基于源码整理的项目内部工作原理说明。

### 用户手册

- [用法 / Usage](Usage) — 安装、窗格、标签页、READONLY 模式和常用操作
- [配置 / Configuration](Configuration) — `~/.sonicterm/sonicterm.toml` 与配置重载
- [快捷键 / Keybindings](Keybindings) — 内置键位映射、绑定语法和 action
- [主题 / Themes](Themes) — 内置配色和自定义主题
- [日志 / Logging](Logging) — 日志、崩溃转储、性能诊断和问题报告

### SonicTerm 如何工作

- [架构 / Architecture](Architecture) — 系统边界、状态所有权、依赖关系和不变量
- [运行时生命周期 / Runtime Lifecycle](Runtime-Lifecycle) — 启动、事件派发、窗口、标签页、窗格与退出
- [终端 IO 与 VT / Terminal IO and VT](Terminal-IO-and-VT) — PTY、可选 SSH、VT 解析、网格、回滚缓冲与 mux 状态
- [渲染与字体 / Rendering and Fonts](Rendering-and-Fonts) — 损坏区域、字形塑形、图集、wgpu 与软件渲染
- [平台集成 / Platform Integration](Platform-Integration) — AppKit/macOS 与 Win32/Windows 边界
- [Crate 参考 / Crate Reference](Crate-Reference) — 工作区全部 23 个 crate 及其关系
- [开发与发布 / Development and Release](Development-and-Release) — 测试、CI、打包与 `v*` 发布流水线

### 项目演进

- [架构演进 / Architecture Evolution](Architecture-Evolution) — 按证据类型分类的架构与可维护性改进建议

受版本控制的规范开发文档仍然是
[`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md)、
[`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md)、
[`docs/LOGGING.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/LOGGING.md)
和 [`docs/packaging/`](https://github.com/D0n9X1n/SonicTerm/tree/main/docs/packaging)。
本 Wiki 的架构页面用于更详细地解释这些边界，不取代其中的规范不变量。
