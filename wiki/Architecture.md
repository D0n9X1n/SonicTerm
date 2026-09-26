# Architecture

[简体中文](Architecture-zh-CN)

Start here for the system map. Follow one input in
[From Keypress to Pixel](From-Keypress-to-Pixel), then use
[Runtime Lifecycle](Runtime-Lifecycle) for state changes,
[Architecture Internals](Architecture-Internals) for correctness rules, and
[Crate Reference](Crate-Reference) for exact dependencies.

### System shape

SonicTerm separates terminal behavior from native windows and presentation.
A pseudo-terminal (PTY) is the operating-system channel between a pane and its
child process. PTY work runs away from the winit event-loop thread.

```mermaid
flowchart TD
    platform["sonicterm-mac / sonicterm-windows / sonicterm-linux"]
    shell["MacShell / WindowsShell / LinuxShell"]
    app["sonicterm-app<br/>live windows, tabs, panes, event routing"]
    core["sonicterm-app-core<br/>pure intents, state, effects"]
    io["sonicterm-io<br/>PTY and process I/O"]
    vt["sonicterm-vt<br/>ANSI/VT parser"]
    grid["sonicterm-grid<br/>cells, history, dirty rows"]
    model["sonicterm-render-model<br/>renderer-facing frame data"]
    font["sonicterm-font / engine / text<br/>discovery, shape, raster, caches"]
    gpu["sonicterm-gpu<br/>frame assembly and presentation"]
    screen(["native window surface"])
    resource["sonicterm-resource<br/>owner tree and ledger"]

    platform --> shell --> app
    app --> core
    app --> io --> vt --> grid --> model --> gpu --> screen
    font --> gpu
    app --> resource
```

The arrows show runtime flow, not every Cargo edge. `sonicterm-app` owns the
live topology. `sonicterm-app-core` owns a separate backend-free state machine.
`sonicterm-gpu` receives terminal and UI types through the render-model boundary.

### Crate boundaries

| Boundary | Owns | Excludes |
| --- | --- | --- |
| `sonicterm-types` | small values and backend-free trait contracts | winit, wgpu, native PTYs |
| `sonicterm-resource` | resource owners, ledger entries, reservations, close checks | retained payloads and seam-specific reclamation |
| `sonicterm-app-core` | `AppState`, `AppIntent`, `AppEffect`, reducers, effect ordering | native handles, blocking I/O, winit, wgpu |
| `sonicterm-io` | local PTY/process work | ANSI interpretation and UI state |
| `sonicterm-vt` / `sonicterm-grid` | terminal parsing, cells, scrollback, cursor state, dirty rows | native windows and GPU resources |
| `sonicterm-cfg` / `sonicterm-ui` | configuration, themes, keymaps, tabs, panes, search, selection, IME | native presentation calls |
| `sonicterm-render-model` | pane geometry and renderer-facing data types | wgpu policy and window ownership |
| `sonicterm-font-config` and font wrapper crates | font configuration and generated FFI boundaries | app and renderer topology |
| `sonicterm-font` / `sonicterm-engine` / `sonicterm-text` | discovery, shaping, rasterization, atlas data, row glyph caches | window lifecycle and PTY ownership |
| `sonicterm-gpu` | damage, row/background caches, frame assembly, wgpu, Windows CPU composition | app topology and child processes |
| `sonicterm-app` | winit handler, live topology, PTY wiring, redraw scheduling, config application | platform-only AppKit, Win32, X11, or Wayland setup |
| platform crates | executable startup, native menus, drag/drop, backdrop hooks, package metadata | reusable terminal behavior |

The renderer has one declared terminal/UI type seam. `sonicterm-gpu` depends on
`sonicterm-render-model`, not directly on `sonicterm-grid`, `sonicterm-cfg`, or
`sonicterm-ui`. It imports their unchanged type identities through
`sonicterm_render_model::boundary::{grid,cfg,ui}`.

### Authoritative runtime state

`App` owns `HashMap<WindowId, WindowState>` and `main_window_id`. The main window
and torn-out windows use the same `WindowState` representation.

Keyboard, IME, focus and modifier events resolve their source identity before
main/child presentation paths diverge. Shared input ownership does not replace
per-pane terminal protocol negotiation or native platform metadata. Native drop
ownership is also selected before window creation, using the installed backend's
capability rather than a second window registry.

Each `WindowState` owns:

- its optional `Arc<Window>` and `GpuRenderer`;
- `TabBar` and `Vec<TabState>`;
- `HashMap<PaneId, PaneState>`;
- selection, copy mode, IME, drag, notification, hover, and redraw state.

Each `TabState` owns a `PaneTree`, its active pane id, search state, and command
status. Each `PaneState` owns its parser, optional `PtyHandle`, redraw target,
terminal-mode atomics, inline images, and resource charges.

`AppStateMachine` holds backend-free compatibility observations, not live GUI
topology. `App::observe_intent` updates them and discards their effects;
`App::dispatch_intent` handles supported explicit-target work separately.
Missing, removed, or zero window keys never select main or frontmost. Native
input goes to its source window's active pane through the bounded PTY queue.
See [Runtime Lifecycle](Runtime-Lifecycle) for the state/intent/effect inventory
and the distinction between record-only effects and native completion.

### Boundary contracts

#### Intent and effect

The backend-free reducer keeps stable effect ordering. GUI observations do not
execute its batch; explicit-target operations cross the live app separately.
The exact order, cascade bound, and dormant queue are recorded in
[Runtime Lifecycle](Runtime-Lifecycle).

#### Terminal

A `PtyHandle` owns the child process boundary, input/output channels, and native
reader and writer threads. A pane VT worker owns parser advancement. It holds the
pane parser lock while applying a batch, then releases it before sending
`UserEvent::RequestRedraw(WindowId)`.

Worker threads do not resolve `WindowId` or call native window APIs. The winit
thread resolves the id against the live window map and calls `request_redraw()`.

#### Rendering

Both window roles use the app-local `VisibleFrameSources` collector. It validates
the active layout before cloning visible parser/image handles and viewport metadata.
It acquires all visible parser guards with `try_lock`, then briefly locks and copies
only those panes' image lists. Inactive-tab and zoom-hidden image stores cannot defer
the visible frame and are not cloned. The owned handles outlive the guards through
ordinary borrows, without a lifetime cast. The shared builder creates real
`PaneRender` grid borrows and moves each image snapshot once; parser guards remain
held through `GpuRenderer::render_with_outcome` and revision acknowledgement. Any visible lock
miss discards the whole collection before entering the existing contention retry.

`GpuRenderer::render_with_outcome` receives visible `PaneRender` records plus explicit UI
arguments. A metadata-only `FramePlan` selects identity, mode, damage, clips,
viewport slots, and expected revisions; it is not a copied-grid or threaded
renderer boundary. Production uses `PaneRender` and `WeztermPipeline`, not the
public compatibility `RenderInputs`/`Painter` seams. See
[Rendering and Fonts](Rendering-and-Fonts) for frame assembly and
[Architecture Internals](Architecture-Internals) for guard and revision rules.

#### Fonts

`sonicterm-engine::FontStack` adapts `sonicterm-font` to the renderer. HarfBuzz
shapes text. CoreText discovers fonts on macOS, GDI on Windows, and Fontconfig on
Linux. DirectWrite is the default Windows rasterizer. FreeType is the default on
macOS and Linux and the Windows fallback.

Generated FreeType and HarfBuzz bindings and hand-written Fontconfig declarations
stay inside their wrapper crates. The renderer receives safe shape results, glyph metrics, and raster
pixels rather than raw FFI handles.

#### Platforms

The three shipping binaries share `ShellRunner` through `MacShell`,
`WindowsShell`, and `LinuxShell`.

- macOS owns AppKit menu setup, native tab suppression, pasteboard handoff, and
  AppKit window hooks.
- Windows owns per-monitor-v2 DPI setup, `muda` menus, DWM backdrop work, OLE
  drag/drop, and GDI software presentation hooks.
- Linux owns X11/Wayland application identity, package assets, and font preflight.
  All platform binaries expose the shared native runtime smoke; Linux package
  layouts additionally run it on X11 and Wayland. Native menus, desktop
  notifications, material backdrops, and cross-process tab drag are absent there.

All reusable keyboard, terminal, pane, and renderer behavior stays in shared
crates. The UI text editor uses AppKit's string-only word-boundary API on macOS
for native Option deletion; it creates no native views or presentation objects.

#### Resources

`App` owns a process-local `ResourceGovernor`. The live GUI owner tree is
`Process → Window → AppPane`. Seam code owns and enforces the primary memory
caps. The pane owner has a derived governor backstop. The process and window
owners track totals without aggregate limits.

The governor records only charged classes. Renderer surfaces, glyph atlases, and
software frames are measured separately and are not in the governor total. See
[Memory](Memory) and [Runtime Lifecycle](Runtime-Lifecycle) for the accounting
and release rules.

### Ownership and concurrency rules

- Only the winit event-loop thread creates, resolves, or presents native windows.
- PTY reader, writer, VT, path-probe, and cleanup workers stay outside the
  event loop.
- Rendering never blocks on a parser. One unavailable required lock defers the
  whole frame.
- A tab transfer moves each live `PaneState` and `PtyHandle`. It changes the
  shared redraw `WindowId`; it does not clone or restart the shell.
- Dropping `PtyHandle` starts bounded process and I/O teardown. Existing-window
  transfers validate destination readiness and retain detached custody until
  attachment commits. Direct drag-merge uses this same boundary. Destination
  setup, topology, or charge-admission refusal restores source order, focus,
  tree/zoom, live PTYs, sizes, and charges. New-window tear-out prepares hidden
  native artifacts and transfers accounting before revealing the destination;
  source hiding or reaping happens only after commitment.
- Terminal mutations mark damage in the same frame. Cache invalidation follows
  font, scale, theme, surface, atlas, and topology changes.

The exact safety conditions are in
[Architecture Internals](Architecture-Internals).

### Dependency direction

| Group | Direction |
| --- | --- |
| contracts | `sonicterm-types` |
| accounting | `sonicterm-resource` feeds `sonicterm-app`; logging tests also use it, and it owns no terminal or frame payload |
| terminal | `sonicterm-io` supplies bytes; `sonicterm-vt` interprets them; `sonicterm-grid` stores the result |
| UI model | `sonicterm-cfg` and `sonicterm-grid` feed `sonicterm-ui`; all three feed `sonicterm-render-model` |
| fonts | `sonicterm-font-config` and native wrappers feed `sonicterm-font`; `sonicterm-font` and `sonicterm-text` independently feed `sonicterm-engine` |
| rendering | render model, engine, text, types, and block glyphs feed `sonicterm-gpu` |
| app | app core, terminal, UI, rendering, logging, and resource crates feed `sonicterm-app` |
| platform | `sonicterm-app` feeds `sonicterm-mac`, `sonicterm-windows`, and `sonicterm-linux` |

### Source map

| Topic | Primary paths |
| --- | --- |
| App state and topology | `crates/sonicterm-app/src/app/mod.rs` |
| Intents, effects, and reducer | `crates/sonicterm-app-core/src/{intent,effect,reducer,state_machine,app_state}.rs` |
| Shell boundary | `crates/sonicterm-app/src/shell.rs` |
| PTY and process boundary | `crates/sonicterm-io/src/pty.rs` |
| VT and grid | `crates/sonicterm-vt/src/vt.rs`, `crates/sonicterm-grid/src/grid.rs` |
| Render model | `crates/sonicterm-render-model/src/{pane_render,inputs,painter,lib}.rs` |
| Renderer | `crates/sonicterm-gpu/src/core.rs` |
| Font adapter | `crates/sonicterm-engine/src/fontstack.rs` |
| Resource governor and app charging | `crates/sonicterm-resource/src/`, `crates/sonicterm-app/src/app/retention.rs` |
| Platform entry points | `crates/sonicterm-{mac,windows,linux}/src/main.rs` |
