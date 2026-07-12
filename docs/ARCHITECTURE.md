# SonicTerm Architecture

SonicTerm is a native macOS + Windows terminal built around small Rust crates
with a strict data-flow boundary:

```text
platform shell -> sonicterm-app -> sonicterm-vt/grid -> render-model -> sonicterm-gpu
                                      ^                         |
                                      |                         v
                                sonicterm-io              sonicterm-ui
```

## Core flow

1. `sonicterm-mac` / `sonicterm-windows` load config, logging, assets, and the
   platform event loop.
2. `sonicterm-app` owns windows, tabs, panes, PTYs, command palette state,
   selection, search, drag/drop, and redraw scheduling.
3. `sonicterm-vt` parses terminal bytes into `sonicterm-grid`.
4. `sonicterm-render-model` carries renderer-agnostic pane/frame inputs.
5. `sonicterm-gpu` builds quads and glyph instances for wgpu presentation. When
   no usable GPU is present it falls back to a CPU rasterizer; on Windows the
   software path (`sonicterm-gpu/src/software_windows.rs` +
   `sonicterm-windows/src/software_presenter.rs`) repaints the whole surface
   deterministically each frame. Glyphs rasterize via DirectWrite by default on
   Windows (`sonicterm-font/src/rasterizer/directwrite.rs`), FreeType elsewhere
   and as the Windows fallback.

## Design rules

- The renderer never blocks on PTY locks during the event loop hot path.
- `sonicterm-gpu` reaches terminal-grid, config/theme, and UI-state types only
  through `sonicterm_render_model::boundary::{grid, cfg, ui}` — it does not depend
  on `sonicterm-grid`/`sonicterm-cfg`/`sonicterm-ui` directly. `render-model` is
  the single declared seam for the `vt/grid -> gpu` and `ui -> gpu` boundaries.
- Platform crates stay thin; cross-platform behavior belongs in `sonicterm-app`
  or lower crates.
- Public contracts live in `sonicterm-types`; changes there affect every crate.
- User-facing settings live in `sonicterm-cfg` and hot-reload where possible.
- WezTerm-proven terminal/font behavior is absorbed into Sonic-owned crates; do
  not add new dependencies on a `vendor/` tree.

## Assets

Runtime assets live under `assets/` and are packaged beside the binaries:

- `assets/themes/*.toml`
- `assets/keymaps/*.toml`
- `assets/fonts/*`
- `assets/icons/*`
- `assets/i18n/*`

macOS also exposes bundled fonts through `Contents/Resources/Fonts` and
`ATSApplicationFontsPath` so AppKit/CoreText can resolve `Rec Mono St.Helens`.
Windows MSI installs the same `assets/fonts/RecMonoSt.Helens-*.ttf` files next
to the executable.

The default theme is `wezterm`, a modified Gruvbox dark hard palette with
SonicTerm's near-black background. The default keymap is platform-specific and
WezTerm-compatible.
