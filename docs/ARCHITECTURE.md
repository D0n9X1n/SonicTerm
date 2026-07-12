# SonicTerm Architecture

Developer documentation: **Architecture** · [Modules](MODULES.md) · [Logging](LOGGING.md) · [Packaging](packaging/README.md)

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

## Rendering and redraw invariants

SonicTerm retains rendered pixels between frames, so damage calculation is part
of terminal correctness rather than a paint optimization:

- A dirty alternate-screen pane repaints its complete surface-clipped pane.
  Primary-screen panes retain narrow dirty-row damage.
- VT/grid mutations mark affected rows in the same frame, including scrolling,
  insert/delete line, reverse index, erase, resize, and wide-cell repair.
- Windows software rendering keeps the established full-surface presenter path;
  it is not coupled to retained GPU damage decisions.
- Pane VT workers never call native window APIs. After output coalescing they
  copy the pane's `WindowId` under a short mutex guard and send
  `UserEvent::RequestRedraw`; the winit event-loop thread resolves the live
  window and calls `request_redraw()` after the guard has been released.
- Tear-out and tab transfer update the pane's redraw target by `WindowId`, so a
  worker survives migration without retaining an `Arc<Window>` or calling
  AppKit/Win32 from the worker thread.

## Font and native boundaries

Font discovery, shaping, and rasterization remain split from renderer policy.
Generated FreeType/HarfBuzz/Fontconfig bindings stay in their wrapper crates;
`sonicterm-font` owns safe allocation and fallback behavior. Variable-font
metadata is optional: malformed, missing, or out-of-range variation metadata
falls back to base OS/2/default weight and width rather than aborting the app.

Native GPU presentation, real PTYs/SSH, AppKit/Win32 handles, generated C ABI
behavior, and installer signing are verified by build, integration, platform CI,
and release smoke checks rather than hollow unit tests.

## Release and verification boundary

The workspace version in root `Cargo.toml` is authoritative for all first-party
crates and internal requirements. Releases are created only by pushing an
owner-approved `v*` tag. The tag workflow builds the expected macOS DMG(s),
Windows MSI, generated release notes, and checksum manifest. Maintained local
packaging instructions live in `docs/packaging/`.

The local release gate is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-workspace-crates.sh
scripts/rust-logic-coverage.sh
bash scripts/test-release-notes.sh
cargo build --release -p sonicterm-mac
```

The deterministic Rust coverage threshold is 80%. Native exceptions use the
substitute checks above; exclusions must not hide difficult deterministic code.
Before merge, macOS and Windows PR checks must pass. Release sign-off also
includes a macOS launch with Vim/nvim alternate-screen exercise and a busy
multi-pane torn-out-window close check that confirms responsive surviving
windows and reaped pane processes.

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
