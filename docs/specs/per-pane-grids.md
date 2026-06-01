# WIP plan: per-pane grids with per-pane sizing

Status: **DRAFT plan** — no code in this PR. A follow-up PR will execute the
plan below.

## Why

Today every pane in a tab shares one `(cols, rows)` derived from the whole
window content area (`Renderer::cells()`), regardless of the pane's own rect.
That is correct only for a single-pane tab. With splits the non-active panes'
grids are sized to the full window, so their VT state (line wrap, dirty
spans, scrollback growth) treats them as much larger than they actually
appear on screen — visible as wrong wrap columns in inactive panes after
a split, wrong reflow on window resize, and TUIs (htop/vim) inside an
inactive pane drawing past their visible border.

This plan splits sizing per pane: each `PaneState` gets resized to the
cells inside *its own* `PaneRect`, not the window content area.

All file paths and line numbers below are as-of branch base
`refactor/split-pane-per-pane-render` (HEAD = `c5e3ec5`).

---

## Part A — Per-pane resize

### Goal
Replace the "one (cols, rows) for all panes" model with per-pane sizing
driven by `PaneTree::layout(outer)`.

### Helper to add
`crates/sonicterm-app/src/app/mod.rs` (next to `resize_all_panes` at line 250):

```rust
/// Resize each pane in `panes` to the cells that fit inside its own
/// `PaneRect` (window-pixel logical rect produced by `PaneTree::layout`).
/// `cell_w`/`cell_h` are the logical cell metrics from the renderer
/// (`Renderer::cell_size()` — see Part B).
///
/// `pub` + `#[doc(hidden)]` so integration tests can drive it without
/// a live wgpu surface.
#[doc(hidden)]
pub fn resize_panes_to_rects(
    panes: &HashMap<u64, PaneState>,
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    cell_w: f32,
    cell_h: f32,
) {
    for (id, rect) in rects {
        let Some(pane) = panes.get(id) else { continue };
        let cols = ((rect.w / cell_w).floor() as u16).max(1);
        let rows = ((rect.h / cell_h).floor() as u16).max(1);
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
}
```

### Caller-by-caller migration of `resize_all_panes`

Every existing call passes whole-window `(cols, rows)` from
`Renderer::cells()` and must be replaced with `resize_panes_to_rects` fed
by the *active tab's* `PaneTree::layout`. The layout call already exists
in `window_event.rs:110-132`; factor it into a helper
`App::compute_active_pane_rects(&self) -> Vec<(u64, sonicterm_ui::pane::Rect)>`
on `crates/sonicterm-app/src/app/mod.rs` so each call site is one line.

For child windows (tear-outs) use `child.tab_states[child.tabs.active_index()]`
the same way; helper variant `compute_pane_rects_for(&ChildWindow)`.

Call sites to migrate:

| File | Line | Current scope | Replace with |
|---|---|---|---|
| `crates/sonicterm-app/src/app/window_event.rs` | 336–343 (inline `for pane in self.panes.values()`) | main window | `resize_panes_to_rects(&self.panes, &self.compute_active_pane_rects(), cell_w, cell_h)` |
| `crates/sonicterm-app/src/app/config_apply.rs` | 103 | main window (font live-reload) | same |
| `crates/sonicterm-app/src/app/config_apply.rs` | 116 | child window | child variant |
| `crates/sonicterm-app/src/app/config_apply.rs` | 162 | main, theme reload | same |
| `crates/sonicterm-app/src/app/config_apply.rs` | 167 | child, theme reload | child variant |
| `crates/sonicterm-app/src/app/config_apply.rs` | 287 | main, keymap reload | same |
| `crates/sonicterm-app/src/app/config_apply.rs` | 292 | child | child variant |
| `crates/sonicterm-app/src/app/config_apply.rs` | 309 | main, padding reload | same |
| `crates/sonicterm-app/src/app/config_apply.rs` | 315 | child | child variant |

The `resize_all_panes` symbol stays for now (one-`(cols,rows)`-for-all is
still the right semantics for the single-pane fast path used by the
integration tests; see Part C). Both helpers coexist; new code uses the
rects variant.

---

## Part B — Renderer signature

### Goal
Expose cell metrics so the app layer can compute per-pane `(cols, rows)`
without re-deriving them.

### Change
`crates/sonicterm-shared/src/render/core.rs`, after `cells()` at line 831, add:

```rust
/// Logical cell metrics (width, height) in CSS pixels. Pair with a
/// `PaneRect` from `PaneTree::layout` to compute how many cells fit
/// in that rect: `cols = (rect.w / cell_w).floor()`, similarly rows.
///
/// Returned values are guaranteed > 0 (the renderer asserts a positive
/// glyph advance at font load).
pub fn cell_size(&self) -> (f32, f32) {
    (self.cell_w, self.cell_h)
}
```

No change to `render()` signature (line 1120) — it already takes
`pane_rects: &[(u64, PaneRect)]`.

### Risk
None; pure additive read accessor on existing private fields.

---

## Part C — Test hook

### Goal
Let integration tests under `crates/sonicterm-app/tests/` exercise the
per-rect resize without a wgpu surface or a real shell.

### Already-public surface (reuse, no new hook needed)
- `sonicterm_app::app::PaneState::new(parser, pty)` — line 502, `#[doc(hidden)]`
- `sonicterm_app::app::resize_all_panes` — line 250, `#[doc(hidden)]`
- `sonicterm_ui::pane::Rect::new(x, y, w, h)` — `crates/sonicterm-ui/src/pane.rs:34`

Add `resize_panes_to_rects` (Part A) with the same `#[doc(hidden)] pub`
treatment so the new regression test can call it directly. No
`__test_support` shim (CLAUDE.md §5 — that pattern is banned).

---

## Part D — Regression test

### File
New: `crates/sonicterm-app/tests/per_pane_resize.rs` (mirrors the shape of
the existing `padding_live_reload.rs` and `font_live_reload.rs`).

### What it asserts
Given two synthetic panes whose `PaneRect`s split a 1000×700 logical
window vertically (a left half 500×700 and a right half 500×700) and
`(cell_w, cell_h) = (10.0, 20.0)`:

1. Each pane's grid resizes to `cols=50, rows=35` (not the whole-window
   `cols=100, rows=35` that today's `resize_all_panes` would produce).
2. The PTY-less pane (the second `PaneState::new(parser, None)` arm)
   does NOT panic when no `pty.resize` exists — same robustness invariant
   that `font_live_reload.rs:37` documents.
3. Calling `resize_panes_to_rects` with an empty `rects: &[]` is a no-op
   and does not touch any grid (used by the path where a tab has been
   removed mid-resize).
4. A rect smaller than one cell still results in `cols >= 1, rows >= 1`
   (the same `.max(1)` floor as `Renderer::cells()`).

### Skeleton

```rust
//! Regression test for per-pane grid sizing under split layouts.
//! Pre-fix, every pane was resized to the whole window's (cols, rows);
//! after the fix each pane sizes to its own PaneRect.
use std::sync::Arc;
use parking_lot::Mutex;
use sonicterm_app::app::{resize_panes_to_rects, PaneState};
use sonicterm_vt::vt::Parser;
use sonicterm_ui::pane::Rect;
use std::collections::HashMap;

#[test]
fn split_panes_size_to_their_own_rects() {
    let parser_a = Arc::new(Mutex::new(Parser::new(80, 24)));
    let parser_b = Arc::new(Mutex::new(Parser::new(80, 24)));
    let mut panes = HashMap::new();
    panes.insert(1u64, PaneState::new(parser_a.clone(), None));
    panes.insert(2u64, PaneState::new(parser_b.clone(), None));
    let rects = vec![
        (1u64, Rect::new(0.0,   0.0, 500.0, 700.0)),
        (2u64, Rect::new(500.0, 0.0, 500.0, 700.0)),
    ];
    resize_panes_to_rects(&panes, &rects, 10.0, 20.0);
    assert_eq!(parser_a.lock().grid().cols(), 50);
    assert_eq!(parser_a.lock().grid().rows(), 35);
    assert_eq!(parser_b.lock().grid().cols(), 50);
    assert_eq!(parser_b.lock().grid().rows(), 35);
}

#[test]
fn empty_rects_is_noop() { /* … */ }

#[test]
fn subcell_rect_floors_to_one() { /* … */ }
```

---

## Risk callouts

- **CLAUDE.md §4 try_lock land-mine.** `resize_panes_to_rects` uses
  `parser.lock()`, NOT `try_lock`. This is correct because the call
  sites are NOT on the render path — they're on the resize/config-reload
  paths, which already own the same `lock()` semantics as
  `resize_all_panes` today. Do NOT switch to `try_lock` here; a missed
  resize leaves the grid wrong size for the entire next frame burst.
  Re-confirm by tracing every call site is reachable only from
  `WindowEvent::Resized` or `config_apply.rs` (both are app-thread, not
  redraw-path).
- **CLAUDE.md §13 GUI smoke is MANDATORY.** This PR touches
  `crates/sonicterm-app/src/app/window_event.rs` and the resize path is
  user-visible — a wrong (cols, rows) shows up as text being truncated
  or as the cursor sitting in the wrong cell after a split. Run the
  full smoke from §13, then ALSO take a second screenshot after a
  `super+D` vertical split with a CJK + RED-BG payload typed into each
  pane. Both panes must show: (a) RED-BG red rectangle aligned to its
  pane's left edge, (b) no wrap past the pane's right edge, (c) cursor
  at column ≤ pane's cols.
- **No renderer behavior change.** Part B adds an accessor only;
  `render()`'s signature and behavior are unchanged. If a reviewer
  asks why the renderer wasn't refactored too: that's deliberate —
  this PR is sizing only. Renderer-side per-pane viewport clipping is
  a separate follow-up.
- **PR #197 just landed** (`fix(ui): split panes, tab hit region,
  tear-out window action routing`). Rebase before pushing the
  executor PR; conflicts most likely in `window_event.rs` around the
  pane_rects build (110–132) and the resize block (336–343).
- **Test floor.** Adding 3 new tests in `per_pane_resize.rs` bumps the
  floor from 824 → 827. Update CLAUDE.md §2 in the executor PR.

---

## Executor checklist (for the Opus background agent)

1. Branch off a fresh clone of `main` in `/tmp/<scratch>` (CLAUDE.md §6).
2. Implement Part B (accessor) — independent, no other code depends on it.
3. Implement Part A (helper + migrate 9 call sites + factor helper for rects).
4. Add Part D regression test.
5. Run the §2 local gate, the §11 capability matrix, the §13 GUI smoke
   (with the split-pane second screenshot above).
6. Bump CLAUDE.md §2 test floor to 827.
7. PR title: `refactor(render): per-pane grids with per-pane sizing`.
8. Final step: `cd / && rm -rf /tmp/<scratch>` (CLAUDE.md §12).
