# SonicTerm — Architecture (post-v1.0-RC)

This document captures the 10-leaf-crate layout that landed in PRs
#145, #151–#158, and #160. Use this when you need to know which
crate owns a given concern or where to put a new module.

## Crate dependency graph

```
                       sonicterm-types
                            ▲
        ┌─────────┬─────────┼─────────┬──────────┐
        │         │         │         │          │
   sonicterm-vt  sonicterm-grid  sonicterm-cfg  sonicterm-io  sonicterm-text
                                                              │
                  sonicterm-render-model                       │
                       ▲                                       │
                       │                                       │
                  sonicterm-ui ─────────────────────────▶ sonicterm-gpu
                       ▲                                       ▲
                       │                                       │
                       └───────────────┬───────────────────────┘
                                       │
                                  sonicterm-app
                                       ▲
                              ┌────────┴────────┐
                              │                  │
                         sonicterm-mac          sonicterm-windows

                         sonicterm-mux (post-v1.0 daemon, optional)
```

## Crate-by-crate

| Crate | Depends on | What's in it |
|---|---|---|
| `sonicterm-types` | (none, std-only) | Cell, Pos, Action enum, GlyphKey, HyperlinkId, geometry primitives |
| `sonicterm-vt` | sonicterm-types | `vt::Parser`, vte Performer, SWAR ASCII fast-path (#138) |
| `sonicterm-grid` | sonicterm-types | `Grid`, scrollback, wide chars, alt screen, dirty bitset (#130), `HyperlinkRegistry` |
| `sonicterm-cfg` | sonicterm-types | `Config`, `Theme`, `Keymap`, `url_open::validate` |
| `sonicterm-io` | sonicterm-types | `PtyHandle`, `proc_info`, Windows `foreground_proc`, optional `ssh` |
| `sonicterm-text` | sonicterm-types | shape LRU cache, swash rasterizer, glyph atlas (LRU eviction), row-glyph cache |
| `sonicterm-render-model` | sonicterm-types | renderer-agnostic geometry / inputs / `Painter` trait — what to draw |
| `sonicterm-gpu` | sonicterm-types, sonicterm-text | wgpu pipelines: quad, text, atlas upload |
| `sonicterm-ui` | sonicterm-types, sonicterm-cfg, sonicterm-grid, sonicterm-render-model | tabs, tabbar_view, pane, selection, search, command_palette, cursor, IME, i18n |
| `sonicterm-app` | everything above | winit ApplicationHandler split across `app/{mod,window_event,event_loop,spawn_pane,keymap_dispatch,key_encoding,input,redraw,overlays,tab_state,tear_out,child_window,config_apply,search_handle,misc}.rs`; menu, os_drag, tab_drag, config_watch |
| `sonicterm-mac` | sonicterm-app | macOS binary, ~30 LOC main |
| `sonicterm-windows` | sonicterm-app | Windows binary, ~30 LOC main |
| `sonicterm-mux` | sonicterm-io, sonicterm-grid | persistent PTY session daemon |

## Why this shape?

1. **Compile-time wins** — leaf crates rebuild independently. Touching
   `sonicterm-ui` no longer recompiles the VT parser.
2. **Build isolation** — leaf crates build independently instead of pulling
   in wgpu unnecessarily.
3. **Honest dependencies** — `sonicterm-render-model` codifies the
   renderer/UI boundary: UI code can produce a frame model without
   linking wgpu, which is what the headless snapshot harness exploits.

## Where to put a new module

- New value type → `sonicterm-types`
- New VT/ANSI behavior → `sonicterm-vt`
- New grid mutation → `sonicterm-grid`
- New config field / theme key → `sonicterm-cfg`
- New PTY backend (e.g. local-mux protocol) → `sonicterm-io`
- New shaping / atlas tweak → `sonicterm-text`
- New wgpu pipeline → `sonicterm-gpu`
- New widget / overlay / palette → `sonicterm-ui`
- New bindable action → variant in `sonicterm-cfg::keymap::Action` AND
  dispatcher arm in `sonicterm-app::app` (`keymap_dispatch.rs`)
- New winit-level glue / window event handling → `sonicterm-app`
- Platform-specific code (NSWindow / Win32) → `sonicterm-mac` /
  `sonicterm-windows` (and only there — keep `sonicterm-app` cross-platform)

See `CLAUDE.md` §1 for the canonical crate inventory.
