# wezterm-takeover-design v5 — finished product

**Branch**: `feat/wezterm-rewrite` (continues from HEAD `ae34947`).
**Track**: Complex (build-or-fix). This is the final shipping spec
for cosmic-text + glyphon removal. No "v1", no "later PR", no
fallback. The deliverable is a production binary.

## User directives (all in force)

1. **Path A** — token/time cost not a constraint, multi-session OK.
2. **No fallback** — cosmic-text + glyphon hard-delete.
3. **Mid-flight build breaks OK** — only the final
   `cargo build --release -p sonicterm-mac` needs to be green.
4. **Prefer wezterm everywhere** — wherever sonicterm and wezterm
   disagree, wezterm wins. Where wezterm has logic, use it; do not
   write parallel sonicterm logic that "could be replaced later".
5. **No legacy compatibility** — every sonicterm-specific behavior
   that conflicts with the wezterm-default path is dropped. Tests
   for deleted behavior are deleted. Surviving capability is
   covered by rewritten tests that assert wezterm-faithful
   behavior.
6. **No future-improvement language** — this spec describes the
   finished product. No "v1 acceptance", no "follow-up PR will…",
   no "if perf regresses we'll add a primer". If the work isn't
   ready to ship as production, the spec isn't done.
7. **Use wezterm completely** — port the box drawing / Powerline /
   Sextant / Octant / Braille custom-glyph geometry from wezterm-
   gui's `customglyph.rs`. Do not accept tofu for these glyphs.
8. **Vendor wezterm source directly when useful** — not every reuse
   has to go through a git dep. Where copying a `.rs` file into
   `sonicterm-*` with import edits is the cleanest path, do that.
   wezterm's license (MIT) permits redistribution; we attribute
   per § "README acknowledgement".
9. **README acknowledgement** — bump the existing README
   `Acknowledgements` section to reflect the new breadth of wezterm
   reuse (VT engine, font system, shaper, custom-glyph geometry).
   Write it warmly — Wez and contributors built the engine that
   makes this terminal possible.
10. **Word cap lifted.**

## Purpose

After this work ships:

1. No file in the workspace `use cosmic_text::*` or `use glyphon::*`.
   No identifier in source carries those names.
2. Neither dep appears in any `Cargo.toml`.
3. The grid hot path and all chrome strings route through
   `sonicterm-font` shape + rasterize + the ported `customglyph`
   block-sprite path.
4. Coordinate system: sonicterm-gpu cell pitch is **raster px**
   end-to-end. The historical `* scale_factor` multipliers at the
   shape boundary are removed.
5. `cargo build --release -p sonicterm-mac` (default features) is
   green.
6. Visible terminal matches `wezterm` for the same input stream
   within the hybrid boundary (winit outer shell preserved).
   Includes box drawing, Powerline, Sextant, Braille, ZWJ emoji,
   programming ligatures, CJK width, color emoji.

## What stays

- winit `ApplicationHandler` + multi-window shell + tab + tear-out
  (hard rule #1).
- `sonicterm-gpu` wgpu pipelines + atlas + quad/text geometry.
  Inputs swap, outputs don't. wgpu version (29) untouched.
- `assets/fonts/RecMonoSt.Helens-*.ttf` files on disk — kept for
  potential distribution bundling. Not referenced from code (the
  user has the font installed system-wide; sonicterm-font's locator
  finds it).

That is the complete list of sonicterm-side font/render policy
that survives.

## What dies (hard delete, no equivalent rewrite in sonicterm)

Source files / modules deleted entirely:
- `crates/sonicterm-text/src/swash_rasterizer.rs`
- `crates/sonicterm-text/src/prewarm.rs`
- `crates/sonicterm-text/src/metrics.rs` cosmic-text helpers
  (`natural_line_h_px`, `measure_cell`, `atlas_dim_for_scale`)
- `crates/sonicterm-text/src/shape.rs` cosmic-text path (file
  becomes the thin `shape_run_with_wezterm` adapter only)
- `crates/sonicterm-text/src/async_fallback.rs` cosmic-text-driven
  fallback loader (sonicterm-font owns fallback)
- `crates/sonicterm-text/src/row_glyph_cache.rs` if its only
  consumer is the deleted swash path; otherwise its cosmic-text
  imports are removed

Functions / constants / types deleted entirely (every call site
deleted; surrounding branches collapsed):
- `SwashRasterizer` + `Rasterizer` impl
- `SymbolFit` enum, `classify_symbol`, `apply_symbol_fit`,
  `apply_symbol_fit_v2`, `icon_fit_target_h`,
  `log_nf_icon_fit_decision`
- `is_powerline_char`, `POWERLINE_PUA_FIRST/LAST`,
  `anchor_powerline_rect`
- `PREBAKE_RANGES`, `prebake_box_and_powerline`
- `canonical_substitute`
- `MAX_FALLBACK_SLOTS`, `DEFAULT_RASTER_PX`
- `rgba_straight_to_bgra_premul`
- `load_bundled_fonts`
- `monochrome_render_config_for_test`,
  `platform_fallback_chain_for_test`, `lookup_id_in_db`
- `apply_wezterm_cluster_widths` (the hybrid overlay in `core.rs`)
- `glyphon::TextRenderer` + the 11 `*_buffer` allocations in
  `core.rs` constructor + every downstream `*.shape_until_scroll`
  / `*.render` callsite
- Identifiers carrying the legacy name:
  - `scale_glyphon_alpha` → `scale_chrome_text_alpha`
  - `glyphon_color_to_linear_rgba` → `chrome_color_to_linear_rgba`
  - `hex_to_glyphon` → `hex_to_chrome_color`
  - `glyphon::Color` parameters → local `ChromeColor` struct
- `cosmic-text` + `glyphon` from every manifest including root
  workspace
- Tests for deleted behavior, deleted (not rewritten):
  `canonical_substitute.rs`, `symbol_fit.rs`,
  `powerline_no_alias.rs`, `powerline_glyph_alignment.rs`,
  `prebake_atlas.rs`, `font_coverage_pua.rs`,
  `mono_alpha_byte_layout.rs`, `swash_lcd_config.rs`,
  `swash_rasterizer_shared.rs`, `lcd_glyph_not_marked_color.rs`,
  `lcd_only_on_windows.rs`, `nerd_font_pua_width.rs`,
  `nerd_font_range_corrections.rs`,
  `font_family_cache_invalidation.rs`, `shape_cjk_diag.rs`
- `DEFAULT_FILTER` in `sonicterm-logging/src/lib.rs` drops
  cosmic_text + glyphon. Test in
  `crates/sonicterm-logging/tests/default_filter.rs` updated.

## What sonicterm builds anew (ports + thin wrappers)

### G2/A — sonicterm-block-glyph (new crate, vendored from wezterm-gui)

The wezterm-gui custom-glyph geometry comes into the workspace
verbatim. Vendor source: `~/.cargo/git/checkouts/wezterm-…/577474d/
wezterm-gui/src/customglyph.rs` (6036 LOC) — copied directly into
`crates/sonicterm-block-glyph/src/lib.rs` with only the GUI-shell
imports rewritten to sonicterm-local equivalents. Wezterm is MIT;
we attribute per § "README acknowledgement". This is verbatim
reuse, not a port — the geometry math, the alpha tables, the Poly
construction, the `from_char` recognizer all copy 1:1.

Why a vendor copy and not a git dep: `customglyph.rs` imports
`crate::glyphcache::{GlyphCache, SizedBlockKey}`,
`crate::utilsprites::RenderMetrics`,
`::window::bitmaps::atlas::Sprite`, `::window::color::SrgbaPixel`,
`window::{BitmapImage, Image, Point, Rect, Size}`. The `window`
crate is wezterm's 22.5k-LOC GUI shell — hard rule #1 forbids
pulling it. A git dep on `wezterm-gui` is therefore impossible.
A separate published crate factoring out `customglyph` doesn't
exist upstream. Vendoring the file is the supported path: per
user directive ("可以直接使用 wezterm 的代码，不一定只能引入包")
the workspace owns the copy, edits the imports, and reships under
MIT with attribution.

Import substitution table (the only edits to the vendored file):

| wezterm-gui import | sonicterm-local replacement |
|---|---|
| `window::Image` (BGRA buffer) | `Vec<u8>` BGRA-premul buffer, width × height × 4, wrapped in a thin `Bitmap` newtype that exposes `clear_rect`, `draw_line` etc. as the customglyph code expects |
| `window::bitmaps::atlas::Sprite` (return type of `block_sprite`) | `sonicterm_text::glyph_atlas::RasterTile` |
| `window::color::SrgbaPixel` | local `BgraPixel(u8, u8, u8, u8)` with the same `rgba()`/`a()` accessors used in customglyph |
| `window::{Point, Rect, Size}` | local plain structs — same field names, same `new()` constructors |
| `wezterm-gui::utilsprites::RenderMetrics` | `sonicterm_engine::CellMetrics` (new) — fields `cell_size: Size`, `underline_height: i32`, `descender_row: i32` (the only three customglyph reads) |
| `wezterm-gui::glyphcache::GlyphCache` (`impl GlyphCache { fn block_sprite }`) | becomes a free function `pub fn block_sprite(...) -> RasterTile` in `sonicterm-block-glyph::lib.rs` — atlas dedupes via `GlyphKey`, no surrounding cache wrapper needed |
| `wezterm-gui::glyphcache::SizedBlockKey` | local `SizedBlockKey { block: BlockKey, cell_size: Size }` — same shape |
| `tiny_skia` | crates.io dep — same version wezterm-gui uses |
| `config::DimensionContext` | port the few-line struct from wezterm's `config` crate into `sonicterm_cfg::dimension` — fraction-of-cell sizing only |
| `sonicterm-font::units::{IntPixelLength, PixelLength}` | already reachable via existing `sonicterm-font` dep — use directly |

Public surface (unchanged from wezterm-gui):
- `pub enum BlockKey` + all variants
- `pub fn BlockKey::from_char(ch: char) -> Option<BlockKey>`
- `pub fn block_sprite(key: SizedBlockKey, metrics: &CellMetrics)
   -> RasterTile`
- All supporting types (`Block`, `Triangle`, `BlockCoord`,
  `BlockAlpha`, `LineScale`, `PolyAA`, `Poly`, `PolyCommand`,
  `PolyStyle`, `CellDiagonal`)

Expected diff size: ~6100 LOC added under
`crates/sonicterm-block-glyph/src/lib.rs`, of which only ~50 lines
are sonicterm edits (the import block + the `Image` → `Bitmap`
newtype + the `GlyphCache` method → free function rewrap). The
remaining ~6050 LOC is verbatim wezterm.

File header carries a copyright + MIT notice and a comment
pointing at the wezterm rev:

```rust
// Vendored from wezterm @ 577474d89ee61aef4a48145cdec82a638d874751,
// path: wezterm-gui/src/customglyph.rs. Original © Wez Furlong and
// the WezTerm contributors, MIT-licensed. See README §Acknowledgements
// and crates/sonicterm-block-glyph/LICENSE-WEZTERM for the full
// upstream license.
```

`crates/sonicterm-block-glyph/LICENSE-WEZTERM` ships the upstream
license text.

### G2/B — `sonicterm-text/src/shape.rs` rewritten

Becomes the thin `shape_run_with_wezterm` adapter only:
- Input: `&[(u16, Cell)]` style run + `&Rc<LoadedFont>`
- Output: `Vec<ShapedGlyph>` where `ShapedGlyph` carries
  `(lead_col, cluster_cells, font_slot, glyph_id, x_advance,
  y_offset)` — all values sourced from sonicterm-font shape output
  (`GlyphInfo`)
- ASCII fast path preserved (1 glyph per cell, no shape call) —
  same gate as today
- No cosmic-text imports

### G2/C — `flush_shape_run` in `core.rs`

- Calls the new `shape_run_with_wezterm`-only path; the
  `apply_wezterm_cluster_widths` overlay is gone
- Calls `block_glyph::BlockKey::from_char(cell.ch)` for the lead
  cell of each cluster; on `Some(key)`, atlas gets a
  `block_sprite(SizedBlockKey { key, cell_size }, &metrics)` tile
  instead of the wezterm shape output
- On `None`, atlas gets the wezterm-rasterized glyph via
  `WeztermRasterizer` (already in place)

### G3 — `sonicterm-gpu/src/chrome_text.rs` (new module)

Single helper that batches chrome strings into the existing atlas
+ text_pipeline:
- `chrome_text::draw(painter, position, text, color, font_size)`
- Internally: `shape_run_with_wezterm` → `WeztermRasterizer` →
  `GlyphAtlas::get_or_insert` → emit `GlyphInstance`s into the
  existing text pipeline buffer
- ~200 LOC
- Replaces all 11 `glyphon::TextRenderer` sites + the
  `tab_spans.rs` glyphon path + the drag-feedback colors

## Coordinate system rationalization — G1a, not G1b

Per user directive ("complete wezterm use, not whichever is
simpler"): port sonicterm-gpu to **raster px end-to-end**. wezterm
metrics drop in unscaled.

Sites that change (counted: 100 `scale_factor` refs in
`crates/sonicterm-gpu/src/core.rs` + 89 in 12 other files across
sonicterm-gpu/-app/-ui):
- Every `* scale_factor` multiplication at draw time → removed
- Every `/ scale_factor` divide at hit-test / resize time →
  removed (raster px in, raster px out)
- winit's physical-px input → already raster, used directly
- `config.width / scale_factor` → `config.width` directly (raster
  is the only system now)
- `build_snapped_cell_x(..., scale_factor)` → drop the parameter
- Pane resize math (`logical_w - padding`) → raster math
- Mouse hit-test (`px / scale_factor`) → raster px directly
- `set_scale_factor` keeps `scale_factor` as a stored value for
  font rasterization (`font_size * scale_factor` for the raster
  px target), but no draw-site division remains
- 13 files touched: `core.rs`, `geometry_emit.rs`, `row_quad_
  cache.rs`, `tab_drag.rs`, `tab_thumbnail.rs`, `app/misc.rs`,
  `app/child_window.rs`, `app/window_event.rs`, `app/os_drag.rs`,
  `app/scrollbar_visibility.rs`, `app/mod.rs`, `app/tear_out.rs`,
  `app/event_loop.rs`

This is a coordinate-system change, not a math change — every
multiplication has a corresponding division that also goes away.
Net behavior: identical rendering on Retina (and identical on
non-Retina), but `cell_w` / `cell_h` now mean raster px, so
wezterm `FontMetrics.cell_width` drops in unmodified.

## Default font config

`crates/sonicterm-cfg/src/defaults.rs`:
- font family default: `"Rec Mono ST.Helens"`
- fallback chain: `"JetBrains Mono"`, `"Symbols Nerd Font Mono"`,
  `"Noto Color Emoji"`

`crates/sonicterm-text/Cargo.toml` sonicterm-font dep gains features
`["vendor-jetbrains", "vendor-noto-emoji",
"vendor-nerd-font-symbols"]` so the fallback chain is built into
the binary even on machines without the user's preferred font
installed.

## Must-pass criteria

1. `cargo build --release -p sonicterm-mac` (default features only).
2. `./target/release/sonicterm-mac` launches, opens a pane,
   renders an interactive shell. §13 smoke per
   `crates/sonicterm-app/CLAUDE.md`.
3. `cargo test --workspace` is green for every test file not in
   the deleted set. New `*_wt.rs` tests cover the wezterm-faithful
   capability matrix:
   - `render_capability_matrix_wt.rs` — ASCII fast path, CJK
     width, ZWJ family emoji, ligatures, color emoji
   - `block_glyph_rendering.rs` — box-drawing + Powerline +
     Sextant + Octant + Braille shapes via ported `block_sprite`
   - `chrome_text_render.rs` — chrome batching produces same atlas
     instances as the old glyphon path produced (visual digest)
   - `wezterm_metrics_unit_parity.rs` — raster-px assertion
4. `grep -rnE 'use (cosmic_text|glyphon)|cosmic[-_]text|glyphon'
   crates/ Cargo.toml --include='*.rs' --include='*.toml'`
   returns **0**.
5. `bash scripts/check-visual-snapshots.sh` green. Baseline
   refresh allowed; the PR commits refreshed
   `crates/sonicterm-shared/tests/snapshots/*.hash` with a
   README row recording the wezterm-takeover diff. Side-by-side
   comparison render against `wezterm` for the same input is
   attached to the PR for box-drawing, Powerline, ZWJ emoji,
   ligature, and CJK rows.
6. **Retina cell-pitch assertion** (raster-px parity, blocks G1):
   - On `scale_factor = 2.0`, `cell_metrics().cell_width` (raster
     px) within ±0.5 raster-px of
     `LoadedFont::metrics_for_idx(0).cell_width`.
   - Headless test in `crates/sonicterm-text/tests/
     wezterm_metrics_unit_parity.rs`.
7. macOS GUI smoke per `crates/sonicterm-app/CLAUDE.md` §13 —
   focus-verified, window-local `screencapture -l`.
8. **Windows GUI smoke** per `docs/WINDOWS_TESTING.md` — same
   render targets as macOS smoke (CJK + ligature + nerd icon +
   box drawing + Powerline + tab bar + palette + search). This
   PR touches render hot files (`sonicterm-gpu/src/core.rs` +
   `text_pipeline.rs` + new `chrome_text.rs` + the metrics
   raster-px rewrite) and per `docs/HOT_FILES.md` requires
   sign-off from BOTH PMs before merge. Win-PM runs the smoke
   on their machine and posts screenshot paths as a PR comment.
9. **wezterm parity smoke** (G2/A regression-guard): a headless
   test in `crates/sonicterm-block-glyph/tests/wezterm_parity.rs`
   that produces a `block_sprite` BGRA buffer for ten characters
   from each block category (box 0x2500..=0x259F, Powerline
   0xE0A0..=0xE0D7, Sextant U+1FB00.., Braille U+2800..) and
   asserts the byte sequence matches a checked-in golden.

   **Golden generation**: one-time, manual, scripted under
   `crates/sonicterm-block-glyph/tools/regen-golden.sh`. The
   script creates a temporary scratch Rust project with
   `wezterm-gui = { git = "...", rev = "577474d..." }` in its
   `Cargo.toml`, calls upstream `block_sprite` for the
   designated characters with identical `RenderMetrics`, writes
   the BGRA bytes to `tests/wezterm_parity_golden/*.bin`, then
   `rm -rf`s the scratch dir. Golden files are checked into the
   repo. The script is run by the PM whenever the pinned
   wezterm rev moves (rare); it is NOT a build-time dep.
   `wezterm-gui` never enters the workspace's own dep graph.

## Test strategy

Tests for deleted behavior are deleted. Surviving tests have
their `cosmic_text` imports replaced with `wezterm_font::
FontConfiguration` via a shared fixture
(`crates/sonicterm-text/tests/common/wt_fixture.rs`).

New `*_wt.rs` tests are written as part of the deliverable; they
are not "future work". List enumerated in must-pass #3.

**Test floor**: recomputed once at PR-merge time. The new floor
is `1445 − len(deleted) + len(new)`. PR commit message carries
`framework-cap-override: test-floor adjusted (wezterm-takeover,
v5 spec)` with the deletion and addition lists. PROGRESS.md
records the new floor as the new baseline. The CLAUDE.md §2
floor constant is bumped to the new value in the same PR.

## Decomposition

Six groups. Mid-flight build breakage permitted between groups.
G6 is the single integration gate where everything must come
together.

- **G1** — Coordinate-system rationalization (G1a). Port
  sonicterm-gpu cell-pitch path to raster-px end-to-end. 13
  files. `cell_metrics_raster_px()` accessor lands. Gated by
  must-pass #6.
- **G2** — Shape + atlas hot path.
  - G2/A: Port `customglyph.rs` into new
    `crates/sonicterm-block-glyph/` crate with the substitution
    table above. Add `tiny_skia` to workspace deps. Add
    `sonicterm-block-glyph` to workspace members. ~2200 LOC.
    Gated by must-pass #8.
  - G2/B: Rewrite `shape.rs` to be the
    `shape_run_with_wezterm`-only adapter. Delete cosmic-text
    imports.
  - G2/C: Rewire `flush_shape_run` in `core.rs` to call the new
    shape path and dispatch on `BlockKey::from_char` for cluster
    lead cells.
  - G2/D: Delete `swash_rasterizer.rs`, `prewarm.rs`,
    `metrics.rs` cosmic-text helpers, `async_fallback.rs`, and
    every legacy call site (collapse surrounding branches).
  - G2/E: Delete `load_bundled_fonts`. `FontStack::try_new(dpi)`
    calls `FontConfiguration::new(None, dpi)` directly. Add
    sonicterm-font `vendor-*` Cargo features in
    `sonicterm-text/Cargo.toml`.
  - G2/F: Edit `crates/sonicterm-cfg/src/defaults.rs` font
    family.
- **G3** — Chrome `chrome_text` helper in
  `crates/sonicterm-gpu/src/chrome_text.rs`. Migrate every
  glyphon-touching site (architect surveys via `grep -rn glyphon
  crates/`). Includes the rename family (`scale_glyphon_alpha`
  → `scale_chrome_text_alpha`, etc.). Delete `glyphon` Cargo
  deps as part of the migration.
- **G4** — Tests:
  - Delete the test files listed under "What dies".
  - Rewrite surviving tests' `cosmic_text` imports against the
    new `wt_fixture.rs`.
  - Write the new `*_wt.rs` files listed in must-pass #3.
  - Write `wezterm_parity.rs` for the block_sprite golden.
- **G5** — `Cargo.toml` final cleanup. Remove every `cosmic-text`
  + `glyphon` dep from every manifest including root. Edit
  `DEFAULT_FILTER` + its test. Bump CLAUDE.md §2 test floor.
  Update README `Acknowledgements` section (see § "README
  acknowledgement" below for the canonical text).
- **G6** — Integration: verify must-pass #1–9 all green. macOS
  §13 GUI smoke. Windows §13 GUI smoke (win-PM, per
  HOT_FILES.md cross-PM protocol). Side-by-side wezterm
  comparison attached to PR. Update PROGRESS.md to reflect the
  new HEAD.

## README acknowledgement (canonical text — landed in G5)

Replace the existing `## Acknowledgements` block in `README.md`
with:

```markdown
## Acknowledgements

SonicTerm stands on the shoulders of [WezTerm](https://github.com/wezterm/wezterm),
the cross-platform GPU terminal Wez Furlong and contributors have
built and maintained for years. SonicTerm absorbs WezTerm-proven
semantics into Sonic-owned crates:

- **Terminal state machine** — VT/ANSI parsing, grid model,
  scrollback, alt-screen handling — via Sonic's `sonicterm-vt` and
  `sonicterm-grid` crates, with WezTerm-compatible behavior where
  adopted.
- **Font system** — discovery, fallback, BIDI, harfbuzz shaping,
  freetype rasterization — via the `sonicterm-font` crate (with
  `vendor-jetbrains`, `vendor-noto-emoji`,
  `vendor-nerd-font-symbols` features for built-in fallback
  coverage).
- **Custom-glyph geometry** — box drawing (U+2500..U+259F),
  Powerline (U+E0A0..U+E0D7), Sextant, Octant, Braille — absorbed
  from `wezterm-gui/src/customglyph.rs` into our local
  `sonicterm-block-glyph` crate. The geometry math, alpha tables,
  and PolyCommand DSL are WezTerm's; we kept them verbatim and
  rewrote only the GUI-shell glue so they sit on top of our wgpu
  atlas instead of WezTerm's `window` crate.

WezTerm is MIT-licensed. The full upstream license travels with
the vendored source at `crates/sonicterm-block-glyph/LICENSE-
WEZTERM`. Where SonicTerm and WezTerm disagree on rendering
behavior, WezTerm wins — its choices have years of cross-platform
production exposure behind them and produce the visual baseline we
target.

SonicTerm adds its own GPU rendering pipeline, tab bar, drag-to-
reorder tabs, tear-out-to-window, merge-into-tab, and the platform
shells on macOS and Windows on top of that engine.

Pinned WezTerm revision: [`577474d`](https://github.com/wezterm/wezterm/commit/577474d89ee61aef4a48145cdec82a638d874751).
```

Order: G1 → G2/A ∥ G2/B → G2/C → (G2/D ∥ G2/E ∥ G2/F ∥ G3 ∥ G4)
→ G5 → G6.

G2/A is independent of G2/B (different file trees) — parallel.
G2/C depends on both. G2/D..F and G3 and G4 are all file-
independent of each other after G2/C — parallel. G5 must run
after every consumer is migrated. G6 is final.

Architect verifies file-independence before approving any
parallel dispatch. Mid-flight build break permitted between
groups.

## Non-goals

- No `wezterm-gui` renderstate vendor for the runtime renderer
  (the `customglyph.rs` vendor under G2/A is geometry data only;
  the wgpu pipeline + atlas + quad helpers stay sonicterm's).
- No new wgpu pipeline; existing text_pipeline + quad reused.

## Cross-audit log

- 2026-06-04 round 1: CRITICAL on cell-metrics diagnosis →
  G1a/G1b split + must-pass #6.
- 2026-06-04 round 2: CRITICAL on swash-fallback feature-flag
  contradiction → user directive: hard delete.
- 2026-06-04 round 3: CRITICAL on swash_rasterizer.rs hard-
  delete breaking 27 prod sites → user directive: mid-flight
  break OK + delete all legacy.
- 2026-06-04 round 4: user clarified `load_bundled_fonts`
  also dies (RecMonoSt.Helens installed system-wide).
- 2026-06-04 round 5: user pulled back from shortcut path
  (Standard/no-test/G1b). Spec rewritten as finished product:
  G1a only, port `customglyph.rs` for box/Powerline/Sextant/
  Braille/Octant, tests rewritten as part of deliverable, no
  "future work" language.
- 2026-06-04 round 6: user clarified vendoring wezterm source
  into the workspace is permitted ("可以直接使用 wezterm 的代码,
  不一定只能引入包"). G2/A path simplified from a "port with
  rewrites" to a "vendor verbatim with import edits". User also
  approved Haiku audit as the sole spec gate ("只要 haiku 同意了,
  你就开干吧。不要我来决定了") — implementation dispatches
  immediately on audit PASS. README §Acknowledgements text added
  to spec, lands as part of G5.
- 2026-06-04 round 7: Haiku audit REJECTED on Non-goals deferring
  Windows §13 to a follow-up PR — this PR touches hot files
  (HOT_FILES.md) and the repo rule requires both-PM sign-off
  before merge. Fixed: must-pass #8 promoted to Windows GUI
  smoke gate; G6 explicitly requires both macOS and Windows
  smoke; Non-goals reframed. wezterm-parity-golden mechanism
  detailed (scripted one-time generation, scratch project,
  wezterm-gui never in workspace dep graph).
