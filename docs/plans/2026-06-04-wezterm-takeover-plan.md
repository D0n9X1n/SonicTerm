# wezterm-takeover implementation plan

Spec: `docs/specs/2026-06-04-wezterm-takeover-design.md` (v5,
authoritative). Branch: `feat/wezterm-rewrite`. HEAD: `ae34947`.

Group order (spec): **G1 → G2/A ∥ G2/B → G2/C → (G2/D ∥ G2/E ∥
G2/F ∥ G3 ∥ G4) → G5 → G6**. Mid-flight build breakage permitted
between groups (user directive); only T20/G6 must green
`cargo build --release -p sonicterm-mac`.

All paths absolute. "Diff size" = net LOC (+added / −removed).

---

## Group G1 — Coordinate-system rationalization (raster-px end-to-end)

13-file rewrite. Counted: 100 `scale_factor` refs in `crates/
sonicterm-gpu/src/core.rs` + 89 across 12 other files (geometry_
emit.rs 12, row_quad_cache.rs 3, tab_drag.rs 4, tab_thumbnail.rs 9,
app/mod.rs 14, app/window_event.rs 15, app/child_window.rs 14,
app/tear_out.rs 10, app/os_drag.rs 3, app/misc.rs 2, app/event_
loop.rs 2, app/scrollbar_visibility.rs 1). Split: T1 (accessor +
parity test) → T2 (core.rs solo) ∥ T3 (gpu rest) ∥ T4 (app rest).

### T1 · G1 · `cell_metrics_raster_px()` accessor + parity test
- **Files**: `crates/sonicterm-engine/src/fontstack.rs`,
  `crates/sonicterm-engine/src/lib.rs`,
  `crates/sonicterm-text/tests/wezterm_metrics_unit_parity.rs` (new)
- **Depends on**: none
- **Parallelizable with**: T5, T8
- **Acceptance**: `FontStack::cell_metrics_raster_px() ->
  CellMetricsPx { cell_w, cell_h, underline_h, descender }` returns
  wezterm `FontMetrics` straight (no `* scale_factor`). New headless
  test asserts `cell_w` within ±0.5 raster-px of `LoadedFont::
  metrics_for_idx(0).cell_width` at `scale_factor = 2.0`
  (must-pass #6).
- **Diff**: +80 / −0
- **Subagent**: Sonnet

### T2 · G1 · `core.rs` scale_factor → raster-px (100 sites)
- **Files**: `crates/sonicterm-gpu/src/core.rs`
- **Depends on**: T1
- **Parallelizable with**: T3, T4
- **Acceptance**: `grep -n "scale_factor" crates/sonicterm-gpu/src/
  core.rs | wc -l` ≤ 5 (allowed: stored value, the `font_size *
  scale_factor` rasterizer target, `set_scale_factor` storage).
  Every `* scale_factor` / `/ scale_factor` at draw or hit-test
  gone. `build_snapped_cell_x` loses its `scale_factor` parameter.
  Compile allowed to break inside G1 (T3/T4 catch up).
- **Diff**: +80 / −180
- **Subagent**: Opus (densest cell-pitch judgment in the PR)

### T3 · G1 · gpu non-core scale_factor → raster-px
- **Files**: `crates/sonicterm-gpu/src/geometry_emit.rs`,
  `crates/sonicterm-gpu/src/row_quad_cache.rs`
- **Depends on**: T1
- **Parallelizable with**: T2, T4
- **Acceptance**: `grep -n "scale_factor" crates/sonicterm-gpu/
  src/{geometry_emit,row_quad_cache}.rs | wc -l` = 0. Function
  signatures that took `scale_factor` drop the parameter.
- **Diff**: +10 / −30
- **Subagent**: Haiku

### T4 · G1 · sonicterm-app scale_factor → raster-px (10 files)
- **Files**: `crates/sonicterm-app/src/tab_drag.rs`,
  `crates/sonicterm-app/src/tab_thumbnail.rs`,
  `crates/sonicterm-app/src/app/{mod,window_event,child_window,
  tear_out,os_drag,misc,event_loop,scrollbar_visibility}.rs`
- **Depends on**: T1
- **Parallelizable with**: T2, T3
- **Acceptance**: `grep -rn "scale_factor" crates/sonicterm-app/
  src/ | wc -l` ≤ 5 (allowed: winit `ScaleFactorChanged` handler +
  single `set_scale_factor` plumb-through). Mouse hit-test uses
  raster-px directly. `config.width / scale_factor` →
  `config.width`. Pane resize math uses raster-px.
- **Diff**: +60 / −150
- **Subagent**: Sonnet (hit-test + resize semantics judgment)

---

## Group G2/A — Vendor `customglyph.rs` (new crate `sonicterm-block-glyph`)

6036 LOC vendor + ~50 lines of edits + ~200 LOC glue. Source:
`~/.cargo/git/checkouts/wezterm-26cdb9e734a97642/577474d/wezterm-
gui/src/customglyph.rs`. Split: T5 (skeleton) → T6 (glue types) →
T7 (verbatim paste + import edits).

### T5 · G2A · crate skeleton + workspace wiring + LICENSE-WEZTERM
- **Files**: `crates/sonicterm-block-glyph/Cargo.toml` (new),
  `crates/sonicterm-block-glyph/src/lib.rs` (new, stub),
  `crates/sonicterm-block-glyph/LICENSE-WEZTERM` (new),
  `/Users/d0n9x1n/Workspace/fun-code/sonicterm/Cargo.toml`
- **Depends on**: none
- **Parallelizable with**: T1, T8
- **Acceptance**: `cargo metadata --no-deps --format-version=1 |
  grep sonicterm-block-glyph` returns the new member. `LICENSE-
  WEZTERM` carries upstream wezterm `LICENSE.md` verbatim. Root
  `Cargo.toml` adds `tiny_skia = "0.11"` to `[workspace.
  dependencies]` + adds `crates/sonicterm-block-glyph` to
  `members`. Stub `lib.rs` carries spec vendor copyright header.
- **Diff**: +1200 / −0 (license text dominates)
- **Subagent**: Haiku

### T6 · G2A · glue types (Bitmap, CellMetrics, BgraPixel, Point/Rect/Size, DimensionContext)
- **Files**: `crates/sonicterm-block-glyph/src/glue.rs` (new),
  `crates/sonicterm-engine/src/cell_metrics.rs` (new),
  `crates/sonicterm-engine/src/lib.rs`,
  `crates/sonicterm-cfg/src/dimension.rs` (new),
  `crates/sonicterm-cfg/src/lib.rs`
- **Depends on**: T5
- **Parallelizable with**: T8
- **Acceptance**: `glue.rs` defines `pub struct Bitmap { bgra:
  Vec<u8>, width: u32, height: u32 }` with `clear_rect` +
  `draw_line` (only customglyph entry points), `pub struct
  BgraPixel(u8,u8,u8,u8)` with `rgba()`/`a()`, `pub struct Point/
  Rect/Size` matching customglyph field names. `sonicterm_engine::
  CellMetrics { cell_size: Size, underline_height: i32,
  descender_row: i32 }` lands. `sonicterm_cfg::dimension::
  DimensionContext` ports fraction-of-cell sizing from wezterm
  `config` (≤30 LOC). `cargo build -p sonicterm-engine -p
  sonicterm-cfg` greens.
- **Diff**: +220 / −0
- **Subagent**: Opus (substitution-boundary newtype design)

### T7 · G2A · vendor `customglyph.rs` verbatim + import substitution
- **Files**: `crates/sonicterm-block-glyph/src/customglyph.rs`
  (new, 6036 LOC paste), `crates/sonicterm-block-glyph/src/lib.rs`
- **Depends on**: T6
- **Parallelizable with**: T8
- **Acceptance**: `wc -l crates/sonicterm-block-glyph/src/
  customglyph.rs` ≥ 6000. Substitution table from spec applied:
  `window::Image` → `crate::glue::Bitmap`; `window::color::
  SrgbaPixel` → `crate::glue::BgraPixel`; `window::{Point,Rect,
  Size}` → `crate::glue::*`; `crate::utilsprites::RenderMetrics`
  → `sonicterm_engine::CellMetrics`; `crate::glyphcache::
  {GlyphCache, SizedBlockKey}` → local `pub struct SizedBlockKey
  { block: BlockKey, cell_size: Size }` + `impl GlyphCache {
  block_sprite }` rewritten to free `pub fn block_sprite(key:
  SizedBlockKey, metrics: &CellMetrics) -> RasterTile`;
  `wezterm_font::units::*` direct; `config::DimensionContext` →
  `sonicterm_cfg::dimension::DimensionContext`. `cargo build -p
  sonicterm-block-glyph` greens. File header carries spec vendor
  attribution.
- **Diff**: +6100 / −0 (≈50 of +6100 are sonicterm edits)
- **Subagent**: Sonnet

---

## Group G2/B — `shape.rs` rewritten as wezterm-only adapter

### T8 · G2B · `shape.rs` → `shape_run_with_wezterm` thin adapter
- **Files**: `crates/sonicterm-text/src/shape.rs`,
  `crates/sonicterm-text/src/lib.rs`
- **Depends on**: none (parallel with G2/A per spec)
- **Parallelizable with**: T5, T6, T7
- **Acceptance**: `shape.rs` ≤ 200 LOC, re-exports
  `shape_run_with_wezterm` from `shape_wt.rs`, zero `use
  cosmic_text` lines, retains ASCII fast-path gate (1 glyph per
  cell, no shape call). `lib.rs` drops cosmic-text re-exports
  (`Attrs`, `Family`, `FontSystem`). `grep -n "cosmic_text"
  crates/sonicterm-text/src/{shape,lib}.rs` returns 0.
- **Diff**: +120 / −600
- **Subagent**: Sonnet

---

## Group G2/C — `flush_shape_run` rewire in `core.rs`

### T9 · G2C · rewire `flush_shape_run` + delete `apply_wezterm_cluster_widths`
- **Files**: `crates/sonicterm-gpu/src/core.rs`
- **Depends on**: T2, T7, T8
- **Parallelizable with**: none (single file, dense)
- **Acceptance**: `flush_shape_run` (currently L4871) drives
  through `shape_run_with_wezterm` only — no `apply_wezterm_
  cluster_widths` call survives (delete function at L6159+ and
  test seam at L6075+). For each cluster lead cell, call
  `sonicterm_block_glyph::BlockKey::from_char(cell.ch)`; on
  `Some(key)`, atlas pulls `block_sprite(SizedBlockKey { block:
  key, cell_size }, &metrics)` via `WeztermRasterizer` fallback;
  on `None`, normal wezterm shape → `WeztermRasterizer`. `grep -n
  "apply_wezterm_cluster_widths\|ShapedGlyph " crates/sonicterm-
  gpu/src/core.rs | wc -l` returns 0. Build may still fail (G3
  owes chrome rewrite); `cargo check -p sonicterm-gpu --lib`
  greens.
- **Diff**: +220 / −350
- **Subagent**: Opus (hottest renderer loop in the binary)

---

## Group G2/D — Delete sonicterm-text legacy modules

### T10 · G2D · hard-delete the cosmic-text/swash family
- **Files DELETED**: `crates/sonicterm-text/src/swash_
  rasterizer.rs`, `prewarm.rs`, `async_fallback.rs`,
  `row_glyph_cache.rs`, `block_element_geometry.rs`,
  `box_drawing_geometry.rs`, `resample.rs` (if unused post-block_
  element/box_drawing — verify)
- **Files EDITED**: `crates/sonicterm-text/src/metrics.rs` (drop
  `natural_line_h_px`, `measure_cell`, `atlas_dim_for_scale`),
  `crates/sonicterm-text/src/lib.rs` (drop removed `pub mod` +
  every `pub use` of a deleted identifier)
- **Depends on**: T8, T9 (no remaining call site)
- **Parallelizable with**: T11, T12, T13, T15, T16, T18
- **Acceptance**: `grep -rn "SwashRasterizer\|SymbolFit\|classify_
  symbol\|apply_symbol_fit\|icon_fit_target_h\|log_nf_icon_fit_
  decision\|is_powerline_char\|POWERLINE_PUA_FIRST\|anchor_
  powerline_rect\|PREBAKE_RANGES\|prebake_box_and_powerline\|
  canonical_substitute\|MAX_FALLBACK_SLOTS\|DEFAULT_RASTER_PX\|
  rgba_straight_to_bgra_premul\|load_bundled_fonts\|monochrome_
  render_config_for_test\|platform_fallback_chain_for_test\|
  lookup_id_in_db" crates/ --include='*.rs'` returns 0. `grep
  -rn "use cosmic_text" crates/sonicterm-text/src/` returns 0.
  `cargo check -p sonicterm-text --lib` greens.
- **Diff**: +30 / −3200
- **Subagent**: Sonnet (collapse surrounding branches)

---

## Group G2/E — FontStack uses `FontConfiguration::new` directly

### T11 · G2E · drop `load_bundled_fonts` + sonicterm-font vendor features
- **Files**: `crates/sonicterm-engine/src/fontstack.rs`,
  `crates/sonicterm-text/Cargo.toml`
- **Depends on**: T10
- **Parallelizable with**: T10, T12, T13, T15, T16, T18
- **Acceptance**: `FontStack::try_new(dpi)` calls `wezterm_font::
  FontConfiguration::new(None, dpi)` directly (no `load_bundled_
  fonts` shim). `sonicterm-font` line carries `features = ["vendor-
  jetbrains", "vendor-noto-emoji", "vendor-nerd-font-symbols"]`.
  `cargo tree -p sonicterm-text | grep sonicterm-font` shows the
  three features. `grep -rn load_bundled_fonts crates/
  --include='*.rs'` returns 0.
- **Diff**: +5 / −60
- **Subagent**: Haiku

---

## Group G2/F — Default font family

### T12 · G2F · `cfg/defaults.rs` font family + fallback chain
- **Files**: `crates/sonicterm-cfg/src/defaults.rs`
- **Depends on**: none in this phase
- **Parallelizable with**: T10, T11, T13, T15, T16, T18
- **Acceptance**: default `font.family = "Rec Mono ST.Helens"`;
  `font.fallback = ["JetBrains Mono", "Symbols Nerd Font Mono",
  "Noto Color Emoji"]`. `cargo test -p sonicterm-cfg` greens.
- **Diff**: +6 / −4
- **Subagent**: Haiku

---

## Group G3 — `chrome_text` helper + rename family + callsite migration

Glyphon callsite survey (architect ran `grep -rn glyphon crates/`):
- `sonicterm-gpu/src/core.rs`: 11 `Buffer` allocations (search_,
  quick_select_, palette_{query,rows,footer}_, cheatsheet_{query,
  rows,footer}_, ime_, broadcast_, drag_chip_) + 2 `TextRenderer::
  new` + 12 `shape_until_scroll` + ~30 `*_buffer.set_text` + ~15
  `hex_to_glyphon` + ~25 `glyphon_color_to_linear_rgba` + 3
  `scale_glyphon_alpha` + `text_renderer_overlay.render()` + per-
  frame `prepare` walks.
- `sonicterm-gpu/src/color.rs`: `glyphon_color_to_linear_rgba`,
  `hex_to_glyphon`, `GColor` type alias.
- `sonicterm-gpu/src/text_pipeline.rs`, `sonicterm-gpu/src/lib.rs`:
  doc comments referencing glyphon (cosmetic edits).
- `sonicterm-ui/src/tab_spans.rs`: `glyphon::{Attrs, Color as
  GColor}` import + `(String, GColor, Attrs)` span tuple shape.
- Manifests (handled in T19/G5): root `Cargo.toml`, `sonicterm-
  {app,gpu,text,ui}/Cargo.toml`.

Split: T13 (helper + ChromeColor + rename) → T14 (callsite migration).

### T13 · G3 · `chrome_text.rs` + `ChromeColor` + rename family
- **Files**: `crates/sonicterm-gpu/src/chrome_text.rs` (new),
  `crates/sonicterm-gpu/src/color.rs`,
  `crates/sonicterm-gpu/src/lib.rs`,
  `crates/sonicterm-gpu/src/text_pipeline.rs`
- **Depends on**: T1, T6
- **Parallelizable with**: T10, T11, T12, T15, T16, T18
- **Acceptance**: `pub fn chrome_text::draw(painter, position,
  text, color: ChromeColor, font_size: f32)` lands. Internally:
  `shape_run_with_wezterm` → `WeztermRasterizer` → `GlyphAtlas::
  get_or_insert` → emits `GlyphInstance`s into existing text
  pipeline buffer. `ChromeColor { r,g,b,a: u8 }` with `rgba()`/
  `a()`. Renames in `color.rs`: `glyphon_color_to_linear_rgba` →
  `chrome_color_to_linear_rgba`; `hex_to_glyphon` → `hex_to_
  chrome_color`; old names removed. `pub fn scale_chrome_text_
  alpha(c: ChromeColor, factor: f32) -> ChromeColor` lands
  (rename of `scale_glyphon_alpha`). Doc-comment glyphon refs
  in `lib.rs` + `text_pipeline.rs` rewritten to `chrome_text`.
  Module compiles in isolation; T14 has the migration.
- **Diff**: +280 / −80
- **Subagent**: Opus (chrome path fanout-heavy)

### T14 · G3 · migrate every glyphon callsite to `chrome_text`
- **Files**: `crates/sonicterm-gpu/src/core.rs`,
  `crates/sonicterm-ui/src/tab_spans.rs`,
  `crates/sonicterm-ui/src/lib.rs`
- **Depends on**: T13
- **Parallelizable with**: T10, T11, T12, T15, T16, T18
- **Acceptance**: `grep -n "glyphon\|GColor\|TextRenderer\|
  Buffer::new\|shape_until_scroll" crates/sonicterm-gpu/src/
  core.rs crates/sonicterm-ui/src/tab_spans.rs` returns 0. All
  11 `*_buffer: Buffer` struct fields removed; replaced by
  call-site `chrome_text::draw(...)` invocations. Two
  `TextRenderer::new` lines + owning struct fields removed.
  `tab_spans.rs` span tuple becomes `(String, ChromeColor,
  ChromeAttrs)` where `ChromeAttrs` is a thin struct (family +
  weight + style). Drag chip ghost path uses `scale_chrome_text_
  alpha`. `cargo check -p sonicterm-gpu -p sonicterm-ui` greens
  (T19/G5 owes dep cleanup but compilation succeeds).
- **Diff**: +400 / −900
- **Subagent**: Opus (largest file-fanout migration in PR)

---

## Group G4 — Tests (deletions, fixture, new wezterm-faithful files)

### T15 · G4 · delete tests for deleted behavior (16 files)
- **Files DELETED**: `crates/sonicterm-text/tests/{canonical_
  substitute,symbol_fit,powerline_no_alias,powerline_glyph_
  alignment,prebake_atlas,font_coverage_pua,mono_alpha_byte_
  layout,swash_lcd_config,swash_rasterizer_shared,lcd_glyph_not_
  marked_color,lcd_only_on_windows,nerd_font_pua_width,nerd_font_
  range_corrections,font_family_cache_invalidation,shape_cjk_
  diag,async_fallback_renderer_wire}.rs`
- **Depends on**: T10
- **Parallelizable with**: T11, T12, T13, T16, T18
- **Acceptance**: 16 files removed. `find crates/sonicterm-text/
  tests -type f -name '*.rs' | wc -l` decreases by 16. No
  surviving test references a deleted identifier. Note: the
  inclusion of `async_fallback_renderer_wire.rs` per Haiku
  round-7 audit — the test wires the deleted `async_fallback`
  module against the deleted `SwashRasterizer`; rewriting it has
  no target.
- **Diff**: +0 / −1180
- **Subagent**: Haiku

### T16 · G4 · `wt_fixture.rs` + rewrite surviving test imports
- **Files**:
  `crates/sonicterm-text/tests/common/mod.rs` (new),
  `crates/sonicterm-text/tests/common/wt_fixture.rs` (new),
  surviving tests with `use cosmic_text` (post-T15):
  `crates/sonicterm-text/tests/{cell_height_line_gap,cjk_advance,
  render_cjk,render_capability_matrix,shape,shape_lru_shared,
  text_shaping_shared}.rs`
- **Depends on**: T8, T11
- **Parallelizable with**: T11, T12, T13, T15, T18
- **Acceptance**: `wt_fixture.rs` exposes `pub fn wt_font_stack()
  -> Rc<FontStack>` + `pub fn shape_text(text: &str) -> Vec<
  WtShapedGlyph>`. `grep -rln "use cosmic_text" crates/sonicterm-
  text/tests/` returns 0. Each rewritten test compiles + passes
  asserting wezterm-faithful behavior (advance widths, cluster
  counts).
- **Diff**: +180 / −250
- **Subagent**: Sonnet

### T17 · G4 · new `*_wt.rs` capability tests (3 files)
- **Files**: `crates/sonicterm-text/tests/render_capability_
  matrix_wt.rs` (new — ASCII fast path, CJK width, ZWJ family
  emoji, ligatures, color emoji), `crates/sonicterm-text/tests/
  block_glyph_rendering.rs` (new — box-drawing + Powerline +
  Sextant + Octant + Braille via ported `block_sprite`),
  `crates/sonicterm-gpu/tests/chrome_text_render.rs` (new —
  chrome batching matches old glyphon atlas instances via visual
  digest). (`wezterm_metrics_unit_parity.rs` landed in T1.)
- **Depends on**: T7, T13, T16
- **Parallelizable with**: T11, T12, T15, T18
- **Acceptance**: 3 new test files. Each compiles + asserts per
  spec must-pass #3. `cargo test -p sonicterm-text --test
  render_capability_matrix_wt --test block_glyph_rendering` and
  `cargo test -p sonicterm-gpu --test chrome_text_render` all
  green by end of T20.
- **Diff**: +500 / −0
- **Subagent**: Sonnet

### T18 · G4 · `wezterm_parity.rs` golden + `regen-golden.sh`
- **Files**:
  `crates/sonicterm-block-glyph/tests/wezterm_parity.rs` (new),
  `crates/sonicterm-block-glyph/tools/regen-golden.sh` (new, +x),
  `crates/sonicterm-block-glyph/tests/wezterm_parity_golden/`
  (new dir, populated once + committed)
- **Depends on**: T7
- **Parallelizable with**: T11, T12, T13, T15, T16
- **Acceptance**: `wezterm_parity.rs` produces a `block_sprite`
  BGRA buffer for 40 designated characters (10 each from box
  0x2500..=0x259F, Powerline 0xE0A0..=0xE0D7, Sextant 0x1FB00..,
  Braille 0x2800..) and asserts byte equality against `tests/
  wezterm_parity_golden/*.bin`. Script is one-shot: creates
  `$(mktemp -d)/regen`, writes `Cargo.toml` with `wezterm-gui =
  { git = "...", rev = "577474d..." }`, calls upstream `block_
  sprite` with identical `RenderMetrics`, writes BGRA bytes to
  repo-relative `tests/wezterm_parity_golden/*.bin`, `rm -rf`s
  scratch. `cargo tree -p sonicterm-block-glyph | grep -c
  wezterm-gui` = 0 (never enters workspace dep graph).
- **Diff**: +280 / −0 (plus ~40 binary goldens, ~30 KB)
- **Subagent**: Sonnet

---

## Group G5 — Manifest + filter + floor + README

### T19 · G5 · final dep cleanup + DEFAULT_FILTER + test floor + README
- **Files**:
  `/Users/d0n9x1n/Workspace/fun-code/sonicterm/Cargo.toml`
  (drop `glyphon = "0.11"`, `cosmic-text = "0.18"` from
  `[workspace.dependencies]`),
  `crates/sonicterm-{app,gpu,text,ui}/Cargo.toml` (drop
  `glyphon.workspace = true` / `cosmic-text.workspace = true`),
  `crates/sonicterm-logging/src/lib.rs` (rewrite
  `DEFAULT_FILTER` — drop `cosmic_text=warn,glyphon=warn`),
  `crates/sonicterm-logging/tests/default_filter.rs` (drop the
  two needles from required-directives check),
  `/Users/d0n9x1n/Workspace/fun-code/sonicterm/CLAUDE.md`
  (§2 test floor recomputed at PR-prep time as `1445 − len(
  deleted) + len(new)`),
  `/Users/d0n9x1n/Workspace/fun-code/sonicterm/README.md`
  (replace `## Acknowledgements` block with canonical spec
  text),
  `/Users/d0n9x1n/Workspace/fun-code/sonicterm/PROGRESS.md`
  (new HEAD + new test-floor baseline)
- **Depends on**: T14, T11
- **Parallelizable with**: none (manifest-touching)
- **Acceptance**: `grep -rnE 'use (cosmic_text|glyphon)|cosmic
  [-_]text|glyphon' crates/ Cargo.toml --include='*.rs'
  --include='*.toml'` returns 0 (must-pass #4). `cargo tree |
  grep -cE 'glyphon|cosmic-text'` returns 0. `cargo test -p
  sonicterm-logging --test default_filter` greens. CLAUDE.md §2
  floor bumped. README §Acknowledgements matches spec verbatim.
  PROGRESS.md HEAD updated.
- **Diff**: +60 / −30
- **Subagent**: Sonnet (test-floor recompute needs actual
  deletion/addition counts from PR commit history)

---

## Group G6 — Integration gate (only build-green requirement)

### T20 · G6 · integration verification + macOS + Windows §13 smoke
- **Files**: none modified — verification + PR comments only.
  Exception: refreshed `crates/sonicterm-shared/tests/snapshots/
  *.hash` if `UPDATE_SNAPSHOTS=1` triggers refresh (with README
  row appended).
- **Depends on**: T17, T18, T19
- **Parallelizable with**: none (final gate)
- **Acceptance** — every must-pass criterion from spec green:
  1. `cargo build --release -p sonicterm-mac` (default features
     only) succeeds.
  2. `./target/release/sonicterm-mac` launches; opens pane; §13
     GUI smoke per `crates/sonicterm-app/CLAUDE.md` (focus-
     verified, window-local `screencapture -l`).
  3. `cargo test --workspace` green at recomputed floor.
  4. `grep -rnE 'use (cosmic_text|glyphon)|cosmic[-_]text|
     glyphon' crates/ Cargo.toml --include='*.rs'
     --include='*.toml'` returns 0.
  5. `bash scripts/check-visual-snapshots.sh` green (baseline
     refresh allowed; commit + README row land here).
  6. Retina cell-pitch assertion via T1 test in workspace sweep.
  7. macOS §13 smoke per `crates/sonicterm-app/CLAUDE.md`
     (mac-PM).
  8. Windows §13 smoke per `docs/WINDOWS_TESTING.md` (win-PM,
     per `docs/HOT_FILES.md`); screenshot paths posted as PR
     comment before merge.
  9. `cargo test -p sonicterm-block-glyph --test wezterm_parity`
     green (T18 golden).
  - Side-by-side wezterm comparison render attached to PR for
    box-drawing, Powerline, ZWJ emoji, ligature, CJK rows.
  - CLAUDE.md §2 local gate passes (fmt, clippy, deny,
    landmines, contract docs, ownership, snapshots, bench).
  - PR body starts with `touches:` line; carries one `dev:*`
    label; commit message contains `framework-cap-override:
    test-floor adjusted (wezterm-takeover, v5 spec)` +
    deletion/addition lists.
- **Diff**: 0 source LOC (snapshots + PROGRESS.md updates only)
- **Subagent**: Opus (cross-PM protocol, snapshot diff judgment,
  final sign-off)

---

## Summary

20 tasks. Sequence: T1 → T2∥T3∥T4 (G1 fanout) → T5→T6→T7 ∥ T8
(G2A serial + G2B parallel) → T9 (G2C, blocks on T2+T7+T8) →
T10∥T11∥T12∥T13→T14∥T15∥T16∥T17∥T18 (G2D/E/F + G3 + G4 wide
parallel) → T19 (G5 manifest, serial) → T20 (G6 integration).
Net workspace LOC: ≈ +6100 vendor (T7) + ≈ +1700 new sonicterm −
≈ 6700 deleted = net +1100. Build permitted red between groups;
only T20 must green `cargo build --release -p sonicterm-mac` and
the full must-pass matrix.
