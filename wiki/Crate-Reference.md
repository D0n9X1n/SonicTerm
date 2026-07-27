# Crate Reference / Crate 参考

## English

> The short canonical map is
> [`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md).
> This page adds dependency and code-navigation detail for all 23 workspace crates.

The workspace version in the root `Cargo.toml` applies to every first-party
crate. `sonicterm-app` is the default workspace member, while the shipping
binaries are `sonicterm-mac` and `sonicterm-windows`.

## Dependency overview

```text
                           sonicterm-types
                      /          |          \
                  grid          cfg          app-core
                    ^            ^              |
                    |            |              |
                   vt            ui             |
                    ^             \             |
                    |              render-model |
                    |                    \       |
                   io                     gpu    |
                    \                     /      |
                     +------ sonicterm-app ------+
                              /         \
                            mac        windows

font-config + fontconfig + freetype + harfbuzz
                       \       |       /
                         sonicterm-font
                           /          \
                       engine        text
                          \          /
                           sonicterm-gpu

sonicterm-io -> sonicterm-mux (standalone, not used by app)

sonicterm-types -> sonicterm-resource -> sonicterm-app (retained-memory ledger)
```

The diagram omits some utility edges; each crate entry below lists its important
relationships.

## Contracts and terminal core

### `sonicterm-types`

**Role:** dependency-light shared contracts: cells, colors, actions, modifier
keys, geometry, glyph/window/hyperlink ids, shell quoting, and backend traits.

**Important API:** `Cell`, `CellFlags`, `Color`, `Action`, `Direction`,
`BroadcastScope`, `GlyphKey`, `WindowKey`, `PtyTransport`, `Painter`,
`ClipboardBackend`, `WindowBackend`.

**Read:** `src/{cell,action,glyph_key,geom}.rs`, `src/traits/`.

### `sonicterm-resource`

**Role:** process-local resource governor. Owns a sharded ledger over one
immutable process root, an owner hierarchy, RAII reservation tokens that
release their charge when dropped, level-triggered cancellation, and a bounded
reaper supervisor that admits work only when a slot is free.

**Important API:** `ResourceGovernor`, `Reservation`, `CommittedReservation`,
`CancelSource`, `CancelToken`, `ReaperSupervisor`, `ReaperLimits`, `ReapTask`,
`ReapSlot`, `ShutdownReport`, `Clock`/`TestClock`.

**Consumes:** `sonicterm-types`.

**Read:** `src/{ledger,owner,reservation,reaper,cancel}.rs`.

### `sonicterm-grid`

**Role:** primary/alternate screens, visible rows, bounded scrollback, cursor,
wide/combining cells, prompt regions, dirty rows, and line compression.

**Consumes:** `sonicterm-types`. **Consumed by:** VT, UI, app, render-model,
engine.

**Read:** `src/grid.rs`, `src/line.rs`, `src/hyperlink.rs`.

### `sonicterm-vt`

**Role:** vte-based ANSI/VT parser and performer. Converts control sequences to
grid mutations, terminal replies, and typed events.

**Consumes:** grid, types. **Consumed by:** app.

**Read:** `src/vt.rs`, plus `tests/autowrap` and `tests/control_sequences`.

### `sonicterm-io`

**Role:** local PTY/process transport, child cleanup, resize, shell selection,
foreground-process detection, and optional SSH implementation.

**Consumes:** types. **Consumed by:** app and mux.

**Feature:** `ssh` enables `russh`, Tokio, and async-trait. It is off by default.

**Read:** `src/pty.rs`, `src/ssh.rs`, `src/{proc_info,foreground_proc}.rs`.

## Configuration and UI

### `sonicterm-cfg`

**Role:** only parser for `sonicterm.toml`, theme/keymap TOML, dimensions, asset
lookup, URL scanning, and safe URL-open policy.

**Consumes:** types, logging. **Consumed by:** app, platform binaries, UI,
render-model.

**Read:** `src/{config,theme,keymap,assets,url_scan,url_open,dimension}.rs`.

### `sonicterm-ui`

**Role:** renderer-independent UI state and layout: tabs, command palette,
search, selection, copy/READONLY mode, pane trees, scrollbar, IME, broadcast,
notifications, and localization.

**Consumes:** types, cfg, grid, text. **Consumed by:** app and render-model.

**Read:** `src/{tabs,pane,command_palette,search,selection,copy_mode,ime,overlays,i18n}.rs`.

### `sonicterm-render-model`

**Role:** renderer-facing pane, geometry, and overlay bundles. It re-exports
grid/config/UI type identities through `boundary::{grid,cfg,ui}` so GPU has one
declared seam.

**Consumes:** types, grid, cfg, UI. **Consumed by:** app and GPU.

**Read:** `src/{pane_render,inputs,geometry,lib}.rs`.

## Text and fonts

### `sonicterm-font-config`

**Role:** configuration value model used by the absorbed font stack: text
styles, font attributes, weights, stretches, rasterizer selection, and font
policy. Its library crate name is `config`.

**Feature:** `distro-defaults` adjusts platform/distribution font defaults.

**Read:** `src/lib.rs`.

### `sonicterm-fontconfig`

**Role:** generated Fontconfig ABI and build/link shim for non-macOS font
discovery. `build.rs` probes system Fontconfig through pkg-config.

**Consumed by:** `sonicterm-font` on Unix platforms.

**Read:** `build.rs`, generated `src/lib.rs` at the ABI boundary.

### `sonicterm-freetype`

**Role:** generated FreeType ABI plus fixed-point helpers. `build.rs` compiles
vendored zlib, libpng, and FreeType and exports include/library paths.

**Consumed by:** font and HarfBuzz wrapper crates.

**Read:** `build.rs`, `bindings.h`, `src/{lib,types,fixed_point}.rs`; treat
`freetype2/`, `libpng/`, and `zlib/` as embedded upstream implementation.

### `sonicterm-harfbuzz`

**Role:** generated HarfBuzz ABI. `build.rs` compiles the embedded HarfBuzz C++
amalgamation against the FreeType build.

**Consumes:** FreeType wrapper. **Consumed by:** font.

**Read:** `build.rs`, `bindings.h`, generated `src/lib.rs`; treat `harfbuzz/` as
embedded upstream implementation.

### `sonicterm-font`

**Role:** safe font discovery, database and matching, HarfBuzz shaping,
fallback, FreeType/DirectWrite/HarfBuzz rasterization, COLR color glyphs, and
native-handle wrappers.

**Consumes:** font-config, fontconfig, freetype, harfbuzz. **Consumed by:** engine.

**Features:** optional vendor-family switches are present for font-stack
compatibility.

**Read:** `src/db.rs`, `src/locator/`, `src/shaper/`, `src/rasterizer/`, and
`src/{ftwrap,hbwrap,fcwrap,parser}.rs`.

### `sonicterm-engine`

**Role:** small renderer-facing font seam. `FontStack` converts font shaping and
raster output to cell metrics and atlas `RasterTile`s.

**Consumes:** font, font-config, grid, text, types. **Consumed by:** GPU.

**Read:** `src/fontstack.rs`.

### `sonicterm-text`

**Role:** CPU glyph atlas, row glyph cache, shaping records, and the
`GlyphInstance` handed to GPU.

**Consumes:** types plus headless image/font utilities. **Consumed by:** UI,
engine, GPU, app.

**Read:** `src/{glyph_atlas,row_glyph_cache,shape,lib}.rs`.

### `sonicterm-block-glyph`

**Role:** geometry and rasterization for box drawing, blocks, Powerline,
Braille, sextants, octants, and synthetic terminal symbols.

**Consumes:** tiny-skia and geometry helpers. **Consumed by:** GPU.

**Provenance:** adapted from WezTerm; see `LICENSE-WEZTERM`.

**Read:** `src/{lib,glue}.rs` and the public boundary of `src/customglyph.rs`.

## Renderer and application

### `sonicterm-gpu`

**Role:** wgpu device/surface owner, frame assembly, dirty-row damage, quad and
glyph emission, atlas upload, retained frame, software-adapter detection, and
Windows CPU rendering.

**Consumes:** types, text, render-model, engine, block-glyph. **Consumed by:** app.

**Read:** `src/core.rs`, `src/wezterm_pipeline.rs`, `src/atlas_upload.rs`,
`src/{row_quad_cache,chrome_text,cursor,color,software_windows}.rs`.

### `sonicterm-app-core`

**Role:** backend-free `AppIntent`, `AppEffect`, `AppState`, reducer, stable
effect ordering, and state machine.

**Consumes:** types plus small data utilities. **Consumed by:** app and platform
binaries.

**Current boundary:** reducer state mirrors many transitions, while live
window/tab/pane topology remains authoritative in `sonicterm-app`.

**Read:** `src/{app_state,intent,effect,reducer,state_machine}.rs`.

### `sonicterm-app`

**Role:** cross-platform winit orchestration. Owns live windows, renderers,
tabs, pane trees, PTYs/parsers, input routing, config reload, redraw scheduling,
overlays, tab transfer, and platform shell abstractions.

**Consumes:** app-core, terminal stack, cfg/UI/render-model, GPU, resource, logging.
**Consumed by:** macOS and Windows binaries.

**Feature:** `ssh` forwards to `sonicterm-io/ssh`; the live SSH session is not
fully wired into the GUI today.

**Read:** `src/app/mod.rs`, `src/app/{event_loop,window_event,spawn_pane,keymap_dispatch,key_encoding,tear_out}.rs`, `src/shell.rs`.

## Platform and future binaries

### `sonicterm-mac`

**Role:** macOS binary, AppKit setup, NSMenu, NSPasteboard/tab dragging, and
platform package entry.

**Consumes:** app-core, app, cfg, logging.

**Read:** `src/{main,menubar,os_drag_mac,tab_drag_os}.rs`.

### `sonicterm-windows`

**Role:** Windows binary, DPI/Win32 setup, muda menu, DWM backdrop, OLE tab
drag/drop, Windows software-present support, resources, and WiX packaging.
Local PTY/ConPTY transport remains behind `sonicterm-io`.

**Consumes:** app-core, app, cfg, logging, types.

**Read:** `src/{main,backdrop,menubar,os_drag_win,software_presenter}.rs`,
`build.rs`, `wix/main.wxs`.

### `sonicterm-logging`

**Role:** rolling file/stderr tracing, retention cleanup, panic hook, crash
dumps, signal/exit tracing, and the canonical process-exit funnel.

**Consumed by:** cfg, app, platform binaries.

**Read:** `src/{lib,config,sinks,cleanup,crash,exit_trace,path}.rs`.

### `sonicterm-mux`

**Role:** experimental standalone persistent-PTY multiplexer daemon. It has a
framed bincode protocol, raw-byte replay ring, and attach/input/resize/kill
operations.

**Consumes:** IO. **Consumed by:** no GUI/platform crate today. It is not
packaged by the release workflow.

**Read:** `src/{main,proto,frame,server}.rs`.

## Feature and shipping status

| Surface | Status |
| --- | --- |
| Local PTY terminal | shipping |
| macOS Apple Silicon + Intel DMG | shipping |
| Windows x64 MSI | shipping |
| SSH transport | optional implementation seam; GUI connection incomplete |
| `sonicterm-mux` | workspace/future daemon; not shipped |
| Linux keymap/fontconfig support | lower-layer support; no Linux GUI binary in workspace |

Every crate contains a local `CLAUDE.md` with its guardrails and local gate.

## 中文

> 简短规范映射位于
> [`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md)。
> 本页为全部 23 个 workspace crate 增加依赖与代码导航细节。

根 `Cargo.toml` 中的 workspace 版本适用于所有第一方 crate。`sonicterm-app` 是默认 member，
实际发布二进制是 `sonicterm-mac` 与 `sonicterm-windows`。

## 依赖概览

```text
                           sonicterm-types
                      /          |          \
                  grid          cfg          app-core
                    ^            ^              |
                    |            |              |
                   vt            ui             |
                    ^             \             |
                    |              render-model |
                    |                    \       |
                   io                     gpu    |
                    \                     /      |
                     +------ sonicterm-app ------+
                              /         \
                            mac        windows

font-config + fontconfig + freetype + harfbuzz
                       \       |       /
                         sonicterm-font
                           /          \
                       engine        text
                          \          /
                           sonicterm-gpu

sonicterm-io -> sonicterm-mux（独立，app 未使用）

sonicterm-types -> sonicterm-resource -> sonicterm-app（常驻内存账本）
```

图中省略部分工具依赖；下方每个 crate 会列出重要关系。

## 契约与终端核心

### `sonicterm-types`

**职责：** 依赖轻量的共享契约：cell、颜色、action、修饰键、几何、glyph/window/hyperlink id、
shell quoting 和后端 trait。

**重要 API：** `Cell`、`CellFlags`、`Color`、`Action`、`Direction`、`BroadcastScope`、
`GlyphKey`、`WindowKey`、`PtyTransport`、`Painter`、`ClipboardBackend`、`WindowBackend`。

**阅读：** `src/{cell,action,glyph_key,geom}.rs`、`src/traits/`。

### `sonicterm-resource`

**职责：** 进程内资源治理器。基于唯一不可变进程根的分片 ledger、owner 层级、
drop 时自动归还额度的 RAII reservation token、电平触发的取消机制，以及只在有空闲
slot 时才接收任务的有界 reaper supervisor。

**重要 API：** `ResourceGovernor`、`Reservation`、`CommittedReservation`、
`CancelSource`、`CancelToken`、`ReaperSupervisor`、`ReaperLimits`、`ReapTask`、
`ReapSlot`、`ShutdownReport`、`Clock`/`TestClock`。

**依赖：** types。

**阅读：** `src/{ledger,owner,reservation,reaper,cancel}.rs`。

### `sonicterm-grid`

**职责：** 主/备用屏幕、可见行、有界 scrollback、光标、宽/组合 cell、prompt region、脏行和 line 压缩。

**依赖：** types。**被依赖：** VT、UI、app、render-model、engine。

**阅读：** `src/grid.rs`、`src/line.rs`、`src/hyperlink.rs`。

### `sonicterm-vt`

**职责：** 基于 vte 的 ANSI/VT parser/performer，把控制序列转为 grid 修改、终端回复和类型化事件。

**依赖：** grid、types。**被依赖：** app。

**阅读：** `src/vt.rs`，以及 `tests/autowrap`、`tests/control_sequences`。

### `sonicterm-io`

**职责：** 本地 PTY/进程 transport、子进程清理、resize、shell 选择、前台进程检测和可选 SSH。

**依赖：** types。**被依赖：** app 与 mux。

**Feature：** `ssh` 启用 `russh`、Tokio、async-trait，默认关闭。

**阅读：** `src/pty.rs`、`src/ssh.rs`、`src/{proc_info,foreground_proc}.rs`。

## 配置与 UI

### `sonicterm-cfg`

**职责：** 唯一负责解析 `sonicterm.toml`、theme/keymap TOML、dimension、asset lookup、URL scan 和安全打开策略。

**依赖：** types、logging。**被依赖：** app、平台二进制、UI、render-model。

**阅读：** `src/{config,theme,keymap,assets,url_scan,url_open,dimension}.rs`。

### `sonicterm-ui`

**职责：** renderer 无关 UI state/layout：标签页、命令面板、搜索、选区、copy/READONLY、pane tree、
scrollbar、IME、broadcast、notification、localization。

**依赖：** types、cfg、grid、text。**被依赖：** app 与 render-model。

**阅读：** `src/{tabs,pane,command_palette,search,selection,copy_mode,ime,overlays,i18n}.rs`。

### `sonicterm-render-model`

**职责：** 面向 renderer 的 pane、geometry、overlay bundle；通过 `boundary::{grid,cfg,ui}` 重新导出
类型身份，使 GPU 只有一个声明接缝。

**依赖：** types、grid、cfg、UI。**被依赖：** app 与 GPU。

**阅读：** `src/{pane_render,inputs,geometry,lib}.rs`。

## 文本与字体

### `sonicterm-font-config`

**职责：** 被吸收字体栈使用的配置值模型：text style、font attribute、weight、stretch、rasterizer selection 和字体策略。
其 library crate 名为 `config`。

**Feature：** `distro-defaults` 调整平台/发行版字体默认值。

**阅读：** `src/lib.rs`。

### `sonicterm-fontconfig`

**职责：** 生成的 Fontconfig ABI 与 build/link shim，用于非 macOS 字体发现；`build.rs` 经 pkg-config 探测系统 Fontconfig。

**被依赖：** Unix 上的 `sonicterm-font`。

**阅读：** `build.rs`，以及 ABI 边界上的生成 `src/lib.rs`。

### `sonicterm-freetype`

**职责：** 生成的 FreeType ABI 与 fixed-point helper。`build.rs` 编译内嵌 zlib、libpng、FreeType 并导出 include/library 路径。

**被依赖：** font 与 HarfBuzz wrapper。

**阅读：** `build.rs`、`bindings.h`、`src/{lib,types,fixed_point}.rs`；把 `freetype2/`、`libpng/`、`zlib/`
视作内嵌上游实现。

### `sonicterm-harfbuzz`

**职责：** 生成的 HarfBuzz ABI；`build.rs` 针对 FreeType build 编译内嵌 HarfBuzz C++ amalgamation。

**依赖：** FreeType wrapper。**被依赖：** font。

**阅读：** `build.rs`、`bindings.h`、生成 `src/lib.rs`；把 `harfbuzz/` 视作内嵌上游实现。

### `sonicterm-font`

**职责：** 安全字体发现、database/matching、HarfBuzz shaping、fallback、FreeType/DirectWrite/HarfBuzz raster、
COLR color glyph 与原生 handle wrapper。

**依赖：** font-config、fontconfig、freetype、harfbuzz。**被依赖：** engine。

**Feature：** 保留若干 vendor-family switch 以兼容字体栈。

**阅读：** `src/db.rs`、`src/locator/`、`src/shaper/`、`src/rasterizer/`、
`src/{ftwrap,hbwrap,fcwrap,parser}.rs`。

### `sonicterm-engine`

**职责：** 小型 renderer-facing font seam。`FontStack` 把字体 shaping/raster 输出转换为 cell metric 和 atlas `RasterTile`。

**依赖：** font、font-config、grid、text、types。**被依赖：** GPU。

**阅读：** `src/fontstack.rs`。

### `sonicterm-text`

**职责：** CPU glyph atlas、row glyph cache、shaping record，以及交给 GPU 的 `GlyphInstance`。

**依赖：** types 与 headless image/font utility。**被依赖：** UI、engine、GPU、app。

**阅读：** `src/{glyph_atlas,row_glyph_cache,shape,lib}.rs`。

### `sonicterm-block-glyph`

**职责：** box drawing、block、Powerline、Braille、sextant、octant 和 synthetic terminal symbol 的几何与光栅化。

**依赖：** tiny-skia 与 geometry helper。**被依赖：** GPU。

**来源：** 从 WezTerm 适配；见 `LICENSE-WEZTERM`。

**阅读：** `src/{lib,glue}.rs` 与 `src/customglyph.rs` 公共边界。

## 渲染与应用

### `sonicterm-gpu`

**职责：** wgpu device/surface、帧组装、脏行 damage、quad/glyph 生成、atlas 上传、保留帧、软件 adapter 检测和 Windows CPU 渲染。

**依赖：** types、text、render-model、engine、block-glyph。**被依赖：** app。

**阅读：** `src/core.rs`、`src/wezterm_pipeline.rs`、`src/atlas_upload.rs`、
`src/{row_quad_cache,chrome_text,cursor,color,software_windows}.rs`。

### `sonicterm-app-core`

**职责：** 后端无关 `AppIntent`、`AppEffect`、`AppState`、reducer、稳定 effect 排序和 state machine。

**依赖：** types 与小型数据 utility。**被依赖：** app 和平台二进制。

**当前边界：** reducer state 镜像许多转换，实时 window/tab/pane topology 仍以 `sonicterm-app` 为准。

**阅读：** `src/{app_state,intent,effect,reducer,state_machine}.rs`。

### `sonicterm-app`

**职责：** 跨平台 winit 编排；拥有实时窗口、renderer、标签页、pane tree、PTY/parser、输入路由、配置重载、
重绘调度、overlay、tab transfer 和平台 shell abstraction。

**依赖：** app-core、终端栈、cfg/UI/render-model、GPU、resource、logging。**被依赖：** macOS 与 Windows 二进制。

**Feature：** `ssh` 转发到 `sonicterm-io/ssh`；实时 SSH session 当前尚未完整接入 GUI。

**阅读：** `src/app/mod.rs`、`src/app/{event_loop,window_event,spawn_pane,keymap_dispatch,key_encoding,tear_out}.rs`、
`src/shell.rs`。

## 平台与未来二进制

### `sonicterm-mac`

**职责：** macOS 二进制、AppKit 设置、NSMenu、NSPasteboard/tab drag 与平台 package 入口。

**依赖：** app-core、app、cfg、logging。

**阅读：** `src/{main,menubar,os_drag_mac,tab_drag_os}.rs`。

### `sonicterm-windows`

**职责：** Windows 二进制、DPI/Win32 设置、muda menu、DWM backdrop、OLE tab drag/drop、
Windows software-present support、resource 与 WiX packaging。本地 PTY/ConPTY transport 仍隐藏在 `sonicterm-io` 后。

**依赖：** app-core、app、cfg、logging、types。

**阅读：** `src/{main,backdrop,menubar,os_drag_win,software_presenter}.rs`、`build.rs`、`wix/main.wxs`。

### `sonicterm-logging`

**职责：** rolling file/stderr tracing、保留策略、panic hook、crash dump、signal/exit tracing 和规范 process-exit funnel。

**被依赖：** cfg、app、平台二进制。

**阅读：** `src/{lib,config,sinks,cleanup,crash,exit_trace,path}.rs`。

### `sonicterm-mux`

**职责：** 实验性独立持久 PTY multiplexer daemon，包含 framed bincode protocol、原始字节 replay ring 和
attach/input/resize/kill 操作。

**依赖：** IO。**被依赖：** 当前无 GUI/平台 crate；release workflow 不打包。

**阅读：** `src/{main,proto,frame,server}.rs`。

## Feature 与发布状态

| Surface | 状态 |
| --- | --- |
| 本地 PTY 终端 | 已发布 |
| macOS Apple Silicon + Intel DMG | 已发布 |
| Windows x64 MSI | 已发布 |
| SSH transport | 可选实现接缝；GUI 连接未完成 |
| `sonicterm-mux` | workspace/未来 daemon；未发布 |
| Linux keymap/fontconfig 支持 | 底层支持；workspace 中无 Linux GUI 二进制 |

每个 crate 都有本地 `CLAUDE.md`，记录 guardrail 与 local gate。
