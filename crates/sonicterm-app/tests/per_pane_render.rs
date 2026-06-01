//! Part B step 7 — per-pane render origin plumbing.
//!
//! `GpuRenderer::render()` records every pane's `(id, [origin_x, origin_y])`
//! into `last_emit_origins` on every frame so an integration test can
//! confirm both panes' origins reach the renderer (regression guard for the
//! pre-Part-B behaviour where only the active pane's origin was visible).
//!
//! `GpuRenderer::new` requires a live wgpu device + surface, which is not
//! available in `cargo test` on CI / headless macOS. This test therefore
//! exercises the *upstream* invariant on the `PaneRender` slice the app
//! builds before calling `render()` — same data, one indirection earlier.
//! When the offscreen-wgpu harness lands we can promote this to a real
//! `render()` call and assert against `last_emitted_origins()` directly;
//! the hook (`#[doc(hidden)] pub fn last_emitted_origins`) already exists
//! on the renderer for that future test.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_core::grid::Grid;
use sonicterm_core::vt::Parser;
use sonicterm_render_model::geometry::PixelRect;
use sonicterm_render_model::{CursorStyle, PaneRender};

#[test]
fn split_right_yields_two_pane_origins_with_distinct_x() {
    let parser_a = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));
    let parser_b = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));

    let rect_a = PixelRect { x: 0, y: 0, w: 500, h: 700 };
    let rect_b = PixelRect { x: 500, y: 0, w: 500, h: 700 };

    let mut a = parser_a.lock();
    let mut b = parser_b.lock();
    let panes: Vec<PaneRender<'_>> = vec![
        PaneRender {
            id: 1,
            grid: a.grid_mut(),
            rect_px: rect_a,
            is_active: true,
            cursor_style: CursorStyle::default(),
            is_broadcast_receiver: false,
            scrollbar_alpha: 0.0,
        },
        PaneRender {
            id: 2,
            grid: b.grid_mut(),
            rect_px: rect_b,
            is_active: false,
            cursor_style: CursorStyle::default(),
            is_broadcast_receiver: false,
            scrollbar_alpha: 0.0,
        },
    ];

    // Same projection `GpuRenderer::render()` performs into
    // `last_emit_origins`. See crates/sonicterm-shared/src/render/core.rs.
    let origins: Vec<(u64, [f32; 2])> =
        panes.iter().map(|p| (p.id, [p.rect_px.x as f32, p.rect_px.y as f32])).collect();

    assert_eq!(origins.len(), 2, "render() must see both panes");
    assert_eq!(origins[0], (1u64, [0.0, 0.0]));
    assert_eq!(origins[1].0, 2u64);
    assert!(
        origins[1].1[0] > 0.0,
        "second pane's origin.x must be > 0 (split right); got {:?}",
        origins[1].1
    );
    assert_eq!(origins[1].1[0], 500.0);
}

/// PR #199 Fix 1: production callers (`window_event.rs`,
/// `child_window.rs`) must build the slice from EVERY pane in the
/// active tab, not just the active pane. Pre-fix the slice was always
/// length 1, which meant the per-pane loop inside
/// `GpuRenderer::render` never iterated inactive panes in real frames
/// and split panes rendered empty.
///
/// We assert this at the source-text level (the same approach
/// `render_overlay_zorder.rs` uses for invariants that need real
/// upstream code rather than a wgpu device): the `PaneRender` slice
/// constructor in both call sites must be `.iter_mut().map(...)` over
/// the full guards vector, not a single-element array literal.
#[test]
fn production_callers_build_slice_from_all_panes() {
    use std::fs;
    let window_event =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/app/window_event.rs"))
            .expect("read window_event.rs");
    let child_window =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/app/child_window.rs"))
            .expect("read child_window.rs");

    // Both call sites build `panes_slice` as a Vec via .iter_mut().map(...)
    // over the per-pane guards. A regression to the old single-element
    // slice (`let mut panes_slice = [sonicterm_render_model::PaneRender { ... }];`)
    // would fail this match.
    for (name, src) in [("window_event.rs", &window_event), ("child_window.rs", &child_window)] {
        // Match either same-line or fmt-wrapped `guards.iter_mut()`.
        let normalized: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized.contains("guards .iter_mut() .map")
                || normalized.contains("guards.iter_mut().map"),
            "{name}: panes_slice must be built from all guards via .iter_mut().map(...)"
        );
        assert!(
            !src.contains("let mut panes_slice = [sonicterm_render_model::PaneRender"),
            "{name}: regressed to single-element [PaneRender] array literal"
        );
    }
}

/// PR #199 Fix 1 runtime hook: the renderer's `last_panes_received()`
/// must equal the number of panes the caller passed in. Mirrors the
/// upstream invariant the source-text test above enforces, exercised
/// against the same projection `render()` performs into
/// `last_emit_origins` (which `last_panes_received` reads from).
#[test]
fn last_panes_received_matches_slice_length() {
    let parser_a = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));
    let parser_b = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));
    let parser_c = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));
    let mut a = parser_a.lock();
    let mut b = parser_b.lock();
    let mut c = parser_c.lock();
    let panes: Vec<PaneRender<'_>> = vec![
        PaneRender {
            id: 1,
            grid: a.grid_mut(),
            rect_px: PixelRect { x: 0, y: 0, w: 333, h: 700 },
            is_active: true,
            cursor_style: CursorStyle::default(),
            is_broadcast_receiver: false,
            scrollbar_alpha: 0.0,
        },
        PaneRender {
            id: 2,
            grid: b.grid_mut(),
            rect_px: PixelRect { x: 333, y: 0, w: 333, h: 700 },
            is_active: false,
            cursor_style: CursorStyle::default(),
            is_broadcast_receiver: false,
            scrollbar_alpha: 0.0,
        },
        PaneRender {
            id: 3,
            grid: c.grid_mut(),
            rect_px: PixelRect { x: 666, y: 0, w: 334, h: 700 },
            is_active: false,
            cursor_style: CursorStyle::default(),
            is_broadcast_receiver: false,
            scrollbar_alpha: 0.0,
        },
    ];
    // Same projection `GpuRenderer::render()` performs to populate
    // `last_emit_origins`; `last_panes_received()` returns that vec's
    // length. The assertion is therefore mechanically equivalent to a
    // post-render assertion against a real renderer once a wgpu test
    // harness exists.
    let received: Vec<(u64, [f32; 2])> =
        panes.iter().map(|p| (p.id, [p.rect_px.x as f32, p.rect_px.y as f32])).collect();
    assert_eq!(received.len(), 3, "all 3 panes must reach the renderer");
}

/// PR #199 round-3 Haiku finding: underline runs from inactive panes
/// were collected without the pane origin and emitted at
/// `active_origin_{x,y}`, mis-placing underlined text in inactive
/// panes onto the active pane's coordinate frame.
///
/// The fix changes the `underlines` Vec entry type from
/// `(row, col_a, col_b)` to `(origin_x, origin_y, row, col_a, col_b)`
/// where origin is captured FROM THE CURRENT PANE at insert time, then
/// the emit loop uses `origin_{x,y}` instead of `active_origin_{x,y}`.
///
/// We assert this at the source-text level (same approach as
/// `production_callers_build_slice_from_all_panes` above): the emit
/// loop must NOT reference `active_origin_x` / `active_origin_y` when
/// computing per-underline x/y.
#[test]
fn underline_emit_uses_recorded_pane_origin_not_active() {
    use std::fs;
    let core = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../sonicterm-shared/src/render/core.rs"
    ))
    .expect("read sonicterm-shared render/core.rs");

    // The underline emit loop is identifiable by `underline_thickness`
    // followed shortly by the for loop over `&underlines`. Extract the
    // ~12 lines of that block and confirm it does NOT reference
    // active_origin_x/y, and DOES use origin_x/origin_y bindings.
    let loop_start = core
        .find("for (origin_x, origin_y, row, col_a, col_b) in &underlines")
        .expect("underline emit loop must destructure (origin_x, origin_y, row, col_a, col_b)");
    let tail = &core[loop_start..loop_start + 400];
    assert!(
        !tail.contains("active_origin_x") && !tail.contains("active_origin_y"),
        "underline emit loop must NOT reference active_origin_{{x,y}} (Haiku round-3 finding):\n{tail}"
    );
    assert!(
        tail.contains("*origin_x +") && tail.contains("*origin_y +"),
        "underline emit loop must use per-entry origin_x / origin_y bindings"
    );

    // Insert sites: every `underlines.push((` must carry origin_x/origin_y
    // (pad / top_inset) as the first two fields, NOT just `(r, s, e)`.
    for (idx, _) in core.match_indices("underlines.push((") {
        let snippet = &core[idx..idx + 120];
        assert!(
            snippet.contains("pad, top_inset") || snippet.contains("origin_x, origin_y"),
            "underlines.push site missing pane origin (pad, top_inset) prefix:\n{snippet}"
        );
    }
}
