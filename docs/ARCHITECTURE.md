# Sonic — Architecture (post-v1.0-RC)

This document captures the 10-leaf-crate layout that landed in PRs
#145, #151–#158, and #160. Use this when you need to know which
crate owns a given concern or where to put a new module.

## Crate dependency graph

```
                       sonic-types
                            ▲
        ┌─────────┬─────────┼─────────┬──────────┐
        │         │         │         │          │
   sonic-vt  sonic-grid  sonic-cfg  sonic-io  sonic-text
        │         │         │         │          │
        └─────────┴────┬────┴─────────┘          │
                       │                          │
                  sonic-core ◀── (deprecated façade)
                       ▲                          │
                       │                          │
                  sonic-render-model              │
                       ▲                          │
                       │                          │
                  sonic-ui ─────────────▶ sonic-gpu
                       ▲                          ▲
                       │                          │
                  sonic-shared ◀── (thin façade) ─┘
                       ▲
                       │
                  sonic-app
                       ▲
              ┌────────┴────────┐
              │                  │
         sonic-mac          sonic-windows
              │
         sonic-mux (post-v1.0 daemon, optional)
```

## Crate-by-crate

| Crate | Depends on | What's in it |
|---|---|---|
| `sonic-types` | (none, std-only) | Cell, Pos, Action enum, GlyphKey, HyperlinkId, geometry primitives |
| `sonic-vt` | sonic-types | `vt::Parser`, vte Performer, SWAR ASCII fast-path (#138) |
| `sonic-grid` | sonic-types | `Grid`, scrollback, wide chars, alt screen, dirty bitset (#130), `HyperlinkRegistry` |
| `sonic-cfg` | sonic-types | `Config`, `Theme`, `Keymap`, `url_open::validate` |
| `sonic-io` | sonic-types | `PtyHandle`, `proc_info`, Windows `foreground_proc`, optional `ssh` |
| `sonic-text` | sonic-types | shape LRU cache, swash rasterizer, glyph atlas (LRU eviction), row-glyph cache |
| `sonic-render-model` | sonic-types | renderer-agnostic geometry / inputs / `Painter` trait — what to draw |
| `sonic-gpu` | sonic-types, sonic-text | wgpu pipelines: quad, text, atlas upload |
| `sonic-ui` | sonic-types, sonic-cfg, sonic-grid, sonic-render-model | tabs, tabbar_view, pane, selection, search, command_palette, cursor, IME, i18n, prefs |
| `sonic-core` | sonic-{vt,grid,cfg,io} | **deprecated façade** — re-exports leaf modules under their historical paths |
| `sonic-shared` | sonic-ui, sonic-gpu, sonic-app | **thin façade** — re-exports + `render/{core,color,metrics,tab_spans,cursor,drag_chip}.rs` |
| `sonic-app` | everything above | winit ApplicationHandler split across `app/{mod,window_event,event_loop,spawn_pane,keymap_dispatch,key_encoding,input,redraw,overlays,tab_state,tear_out,child_window,prefs_window,config_apply,search_handle,misc}.rs`; menu, os_drag, tab_drag, config_watch |
| `sonic-mac` | sonic-app (via sonic-shared) | macOS binary, ~30 LOC main |
| `sonic-windows` | sonic-app (via sonic-shared) | Windows binary, ~30 LOC main |
| `sonic-mux` | sonic-io, sonic-grid | persistent PTY session daemon |

## Why this shape?

1. **Compile-time wins** — leaf crates rebuild independently. Touching
   `sonic-ui` no longer recompiles the VT parser.
2. **Test isolation** — `cargo test -p sonic-vt` runs in seconds
   instead of pulling in wgpu.
3. **Honest dependencies** — `sonic-render-model` codifies the
   renderer/UI boundary: UI code can produce a frame model without
   linking wgpu, which is what the headless snapshot harness exploits.
4. **Backwards compatibility** — the `sonic-core` and `sonic-shared`
   façades let pre-#152 imports compile unchanged during the
   transition.

## Where to put a new module

- New value type → `sonic-types`
- New VT/ANSI behavior → `sonic-vt`
- New grid mutation → `sonic-grid`
- New config field / theme key → `sonic-cfg`
- New PTY backend (e.g. local-mux protocol) → `sonic-io`
- New shaping / atlas tweak → `sonic-text`
- New wgpu pipeline → `sonic-gpu`
- New widget / overlay / palette → `sonic-ui`
- New bindable action → variant in `sonic-cfg::keymap::Action` AND
  dispatcher arm in `sonic-app::app` (`keymap_dispatch.rs`)
- New winit-level glue / window event handling → `sonic-app`
- Platform-specific code (NSWindow / Win32) → `sonic-mac` /
  `sonic-windows` (and only there — keep `sonic-app` cross-platform)

See `CLAUDE.md` §1 for the canonical crate inventory.
