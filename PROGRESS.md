# WezTerm Rewrite Progress — feat/wezterm-rewrite

**Read this file first if you've been compacted.** Source of truth.

## Status (2026-06-05)

✅ **MAIN BUILD GREEN**: `cargo build --release -p sonicterm-mac`
   PASSES at HEAD.
✅ **PRODUCTION SOURCE FREE OF cosmic_text + glyphon**: 0 `use
   cosmic_text` or `use glyphon` anywhere in `crates/*/src/`. Both
   deps fully removed from every `Cargo.toml`.
✅ **sonicterm-font drives every glyph in the binary**: shape +
   rasterize + fallback. Block-drawing / Powerline / Sextant /
   Braille / Octant geometry vendored from `wezterm-gui/src/
   customglyph.rs` into `crates/sonicterm-block-glyph/`.
✅ **Chrome (tab bar / palette / search / IME / drag chip / footer)**
   routes through new `sonicterm-gpu::chrome_text` helper. glyphon's
   `TextRenderer` deleted along with its 11 `*_buffer` allocations.
✅ **G1a coordinate-system rewrite**: sonicterm-gpu cell pitch is
   raster-px end-to-end. wezterm `FontMetrics` drops in unscaled.

## Architecture

```
                            sonicterm-font
                           /            \
       FontConfiguration                LoadedFont
              │                              │
              ▼                              ▼
        FontStack         ─────►   shape_run_with_wezterm
              │                              │
              ▼                              ▼
        cell_metrics_raster_px       WtShapedGlyph
              │                              │
              ▼                              ▼
                                  WeztermRasterizer
                                         │
                                         ▼
  sonicterm-block-glyph::BlockKey ──►  RasterTile
        (custom-glyph procedural)       │
                                         ▼
                                   GlyphAtlas
                                         │
                                         ▼
                            sonicterm-gpu text_pipeline + quad
                                         │
                                         ▼
                           winit surface (UNCHANGED)
```

## What landed (commits)

| Commit | Task | Scope |
|---|---|---|
| `c7515b1` | spec + plan | Haiku-approved spec v5 + 20-task plan |
| `5687b88` | T1 | `FontStack::cell_metrics_raster_px()` + Retina parity test |
| `93accea` | T5+T8 | sonicterm-block-glyph skeleton; shape.rs thin adapter |
| `dae4ffc` | T2 | core.rs scale_factor 100 → 7 sites |
| `57a22d3` | T3+T4+T6 | gpu/app raster-px + block-glyph glue types |
| `8835409` | T7 | customglyph.rs vendored verbatim (6100 LOC) |
| `7ad5a47` | T9 | flush_shape_run wezterm-only rewire |
| `(this commit)` | T10+T13+T14+T15 (partial)+G5 | bulk delete legacy + chrome migration + glyphon dep removal + build green |

## Spec + plan

- Spec: `docs/specs/2026-06-04-wezterm-takeover-design.md` (v5, 7
  cross-audit rounds, Haiku-approved)
- Plan: `docs/plans/2026-06-04-wezterm-takeover-plan.md` (20 tasks,
  Haiku-approved)

## Followups (not blocking the production binary)

These items are deferred to follow-up PRs in the same branch family.
None blocks `cargo build --release -p sonicterm-mac` shipping today.

1. **PaneEngine → GridFacade wiring (full)**. `PaneRender::grid` is
   currently typed `&'a mut sonicterm_grid::grid::Grid` (the legacy
   parser-grid path). The spec calls for `&'a mut
   sonicterm_engine::GridFacade<'a>`, which requires `PaneState` to
   own a `WeztermPaneEngine` and feed it from the PTY bytes. Engine
   wrapper is in place (`crates/sonicterm-engine/src/lib.rs`); the
   pane-level integration is the next architectural step.
2. **T18 wezterm parity goldens**. Test scaffold landed but goldens
   not yet generated. `tools/regen-golden.sh` script needs to be
   written + run against a scratch project that pulls `wezterm-gui`.
3. **`*_wt.rs` capability matrix tests** (per spec must-pass #3).
   Test floor needs to be re-established with wezterm-only coverage:
   ASCII fast path, CJK width, ZWJ family emoji, ligatures, nerd-
   font PUA, line-height + baseline, color emoji.
4. **`scripts/check-visual-snapshots.sh` baseline refresh**. With
   the renderer flowing through sonicterm-font end-to-end, the dHash
   baselines will move. Spec must-pass #5 + #6 cover the refresh
   protocol (Retina cell-pitch assertion green before baselines bump).
5. **macOS + Windows §13 GUI smoke** (must-pass #7 + #8). Per
   `docs/HOT_FILES.md` both-PM sign-off required before merge.
6. **Test floor restoration**. Many sonicterm-text/-gpu test files
   were deleted because they tested deleted-policy behavior (swash
   resample, SymbolFit, canonical_substitute, etc.). Replacements
   land alongside item 3 above. Commit message will carry
   `framework-cap-override: test-floor adjusted (wezterm-takeover)`.

## Branches

- `main` @ `e1646ec` — baseline (untouched by this work)
- `feat/wezterm-rewrite` — **active**, contains everything above

## License / attribution

`README.md` § Acknowledgements (v2) describes the WezTerm reuse
(VT engine, font system, customglyph geometry vendor).
`crates/sonicterm-block-glyph/LICENSE-WEZTERM` ships upstream
license verbatim alongside the vendored source. Pinned wezterm
revision: `577474d89ee61aef4a48145cdec82a638d874751`.

## Resume protocol

1. `cat PROGRESS.md` (this file)
2. `git log --oneline -10` to see latest commits
3. `git status --short` for uncommitted work
4. `cargo build --release -p sonicterm-mac` to verify production
   binary still builds
5. Re-read `docs/specs/2026-06-04-wezterm-takeover-design.md` for
   the authoritative scope
6. Use `TaskList`
