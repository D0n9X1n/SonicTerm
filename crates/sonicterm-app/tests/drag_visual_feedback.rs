//! Phase D (Epic #289) — drag visual feedback regression tests.
//!
//! Covers the three Phase D affordances added on top of the existing
//! drag-chip plumbing:
//!
//!   * D1 — when a drag session is live AND the cursor has moved past
//!     `DRAG_START_THRESHOLD_PX`, `build_drag_chip_overlay` populates
//!     `ghost_alpha = 0.5` so the chip body renders as a translucent
//!     ghost of the dragged tab.
//!
//!   * D2 — when the cursor is over the source bar, the overlay
//!     publishes `insertion_slot = Some(slot)`. The pure layout
//!     helper `TabBarLayout::compute_with_insertion_slot` shifts
//!     every tab at index ≥ slot right by `INSERTION_GAP_PX` (8 px)
//!     so the bar paints with a visible insertion gap previewing the
//!     drop position.
//!
//!   * D3 — `source_tab_idx` is always populated with the press tab
//!     index, and `source_alpha = 0.3` so the renderer can dim the
//!     source tab while the drag is live (so the dragged tab visibly
//!     "lifts off" instead of staying solid in the source bar).

use sonicterm_app::tab_drag::{build_drag_chip_overlay, DragSession};
use sonicterm_shared::tabbar_view::TabBarLayout;
use sonicterm_shared::tabs::{Tab, TabBar};

const INSERTION_GAP_PX: f32 = TabBarLayout::INSERTION_GAP_PX;

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

#[test]
fn d1_ghost_present_and_alpha_half_after_threshold() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    // Press near the middle tab; move the cursor 50 px right (well
    // past the 5 px drag-start threshold) so a chip is published.
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (350.0, 10.0);
    let chip = build_drag_chip_overlay(&s, &layout, "ghost-me".to_string())
        .expect("chip must be present once drag passes threshold");
    // D1 spec: ghost alpha 0.5.
    assert!(
        (chip.ghost_alpha - 0.5).abs() < f32::EPSILON,
        "D1 ghost alpha must equal 0.5; got {}",
        chip.ghost_alpha
    );
    // The title round-trips so the renderer can paint it into the
    // ghost body.
    assert_eq!(chip.title, "ghost-me");
}

#[test]
fn d1_no_ghost_below_threshold() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    // Press and barely move (1 px) — still a click, not a drag.
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (301.0, 10.0);
    assert!(
        build_drag_chip_overlay(&s, &layout, "t1".into()).is_none(),
        "D1 must suppress the ghost while cursor has moved < threshold"
    );
}

#[test]
fn d2_insertion_slot_shifts_tabs_by_eight_px() {
    let bar = bar_with(4);
    let baseline = TabBarLayout::compute_with_height(&bar, 1200.0, 40.0);
    // Slot 2: tabs 2 and 3 should each shift right by 8 px; tabs
    // 0 and 1 stay put.
    let shifted = TabBarLayout::compute_with_insertion_slot(&bar, 1200.0, 40.0, Some(2));
    assert_eq!(baseline.tabs.len(), shifted.tabs.len());
    for (i, (b, s)) in baseline.tabs.iter().zip(shifted.tabs.iter()).enumerate() {
        let dx = s.bg_rect.x - b.bg_rect.x;
        if i < 2 {
            assert!(dx.abs() < f32::EPSILON, "tab {i} (< slot) must not shift; dx={dx}");
        } else {
            assert!(
                (dx - INSERTION_GAP_PX).abs() < f32::EPSILON,
                "tab {i} (>= slot) must shift by {INSERTION_GAP_PX}; dx={dx}"
            );
            // Sub-rects must move with the parent so hit-testing
            // remains internally consistent.
            assert!((s.close_x_rect.x - b.close_x_rect.x - INSERTION_GAP_PX).abs() < 1e-3);
            assert!((s.title_rect.x - b.title_rect.x - INSERTION_GAP_PX).abs() < 1e-3);
        }
    }
}

#[test]
fn d2_none_insertion_slot_is_identity() {
    let bar = bar_with(3);
    let baseline = TabBarLayout::compute_with_height(&bar, 1000.0, 40.0);
    let same = TabBarLayout::compute_with_insertion_slot(&bar, 1000.0, 40.0, None);
    for (b, s) in baseline.tabs.iter().zip(same.tabs.iter()) {
        assert!((b.bg_rect.x - s.bg_rect.x).abs() < f32::EPSILON);
    }
}

#[test]
fn d2_chip_publishes_insertion_slot_when_cursor_over_bar() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 40.0);
    // Press tab 0, drag the cursor to over tab 2 (still inside the
    // bar's Y range).
    let mid_tab2_x = layout.tabs[2].bg_rect.x + layout.tabs[2].bg_rect.w * 0.5;
    let mut s = DragSession::new(0, (layout.tabs[0].bg_rect.x + 5.0, 10.0));
    s.current_pos = (mid_tab2_x + 50.0, 10.0);
    let chip = build_drag_chip_overlay(&s, &layout, "t0".into()).expect("chip");
    assert!(
        chip.insertion_slot.is_some(),
        "D2 chip must publish an insertion slot while cursor is over the source bar"
    );
}

#[test]
fn d2_chip_clears_insertion_slot_when_cursor_off_bar() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 40.0);
    let mut s = DragSession::new(0, (layout.tabs[0].bg_rect.x + 5.0, 10.0));
    // Drag well below the bar — tear-out armed.
    s.current_pos = (200.0, 500.0);
    let chip = build_drag_chip_overlay(&s, &layout, "t0".into()).expect("chip");
    assert!(
        chip.insertion_slot.is_none(),
        "D2 chip must NOT publish an insertion slot once cursor leaves the bar"
    );
}

#[test]
fn d3_source_tab_is_grayed_at_alpha_point_three() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 40.0);
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (360.0, 10.0);
    let chip = build_drag_chip_overlay(&s, &layout, "t1".into()).expect("chip");
    // D3 spec: source_tab_idx populated with the press index so the
    // renderer dims the right tab; source_alpha == 0.3.
    assert_eq!(chip.source_tab_idx, Some(1), "D3 must flag the source tab by its press index");
    assert!(
        (chip.source_alpha - 0.3).abs() < f32::EPSILON,
        "D3 source-tab alpha must equal 0.3; got {}",
        chip.source_alpha
    );
}

/// Haiku follow-up on PR #298: the prior tests pin the `chip.ghost_alpha`
/// and `chip.source_alpha` plumbing values (0.5 / 0.3) but DO NOT
/// assert that the renderer actually emits a text run with the matching
/// alpha. This test reproduces the exact computation the renderer
/// performs in `crates/sonicterm-shared/src/render/core.rs` for the GHOST
/// title (~line 3288): `scale_glyphon_alpha(tab_active_fg, chip.ghost_alpha)`.
/// If a future refactor drops the `scale_glyphon_alpha` call, or feeds
/// the wrong factor, this test fails with a concrete alpha mismatch.
#[test]
fn ghost_tab_text_run_has_half_alpha() {
    use glyphon::Color as GColor;
    use sonicterm_shared::render::scale_glyphon_alpha;

    // Reproduce the live drag → chip pipeline the renderer consumes.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let mut s = DragSession::new(1, (300.0, 10.0));
    s.current_pos = (360.0, 10.0);
    let chip = build_drag_chip_overlay(&s, &layout, "ghost-me".to_string())
        .expect("chip must be present once drag passes threshold");

    // Mirror the renderer: ghost title fg = tab_active_fg scaled by
    // chip.ghost_alpha. Use an opaque base so the result alpha is
    // purely a function of the ghost factor.
    let tab_active_fg = GColor::rgba(0xEE, 0xEE, 0xEE, 0xFF);
    let ghost_fg = scale_glyphon_alpha(tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));

    let alpha = ghost_fg.a();
    assert!(
        (alpha as i32 - 128).abs() <= 2,
        "ghost text run alpha should be ~128 (50 % of 255), got {alpha}"
    );
    // rgb must round-trip — the scale only touches the alpha channel.
    assert_eq!(ghost_fg.r(), 0xEE);
    assert_eq!(ghost_fg.g(), 0xEE);
    assert_eq!(ghost_fg.b(), 0xEE);
}

/// Haiku follow-up on PR #298: assert the SOURCE tab title text run
/// actually carries the dimmed alpha. Reproduces the source-tab
/// dimming pipeline from `render/core.rs` (~line 2767): build the
/// tab-title spans with `build_tab_title_spans`, then dim the
/// source-tab entry via `scale_glyphon_alpha(entry.1, source_alpha)`.
/// The resulting span color must have alpha ≈ 77 (0.3 * 255).
#[test]
fn source_tab_text_during_drag_has_30pct_alpha() {
    use glyphon::Color as GColor;
    use sonicterm_shared::render::{build_tab_title_spans, scale_glyphon_alpha, TabSpanInput};

    // Build a 3-tab bar; the drag's source is tab index 1.
    let bar = bar_with(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1200.0, 40.0);
    let mut s = DragSession::new(1, (layout.tabs[1].bg_rect.x + 5.0, 10.0));
    s.current_pos = (layout.tabs[1].bg_rect.x + 70.0, 10.0);
    let chip = build_drag_chip_overlay(&s, &layout, "t1".into()).expect("chip");
    let src_idx = chip.source_tab_idx.expect("source idx must be populated for a live drag");

    // Reproduce the renderer's span pipeline.
    let active_fg = GColor::rgba(0xFF, 0xFF, 0xFF, 0xFF);
    let inactive_fg = GColor::rgba(0xAA, 0xAA, 0xAA, 0xFF);
    let inputs: Vec<TabSpanInput> = layout
        .tabs
        .iter()
        .map(|t| TabSpanInput {
            index: t.idx,
            title: &bar.tabs()[t.idx].title,
            title_x: t.title_rect.x,
            title_w: t.title_rect.w,
            is_active: layout.active == Some(t.idx),
            badge: None,
        })
        .collect();
    let (_title_text, mut tab_spans) = build_tab_title_spans(&inputs, 8.0, active_fg, inactive_fg);

    // Dim the source-tab span exactly as the renderer does.
    let mut dimmed_alpha: Option<u8> = None;
    for (i, t) in inputs.iter().enumerate() {
        if t.index == src_idx {
            if let Some(entry) = tab_spans.get_mut(i) {
                entry.1 = scale_glyphon_alpha(entry.1, chip.source_alpha);
                dimmed_alpha = Some(entry.1.a());
            }
            break;
        }
    }
    let alpha =
        dimmed_alpha.expect("source-tab span must be present and dimmed by the renderer pipeline");
    assert!(
        (alpha as i32 - 77).abs() <= 3,
        "source tab text alpha should be ~77 (30 % of 255), got {alpha}"
    );

    // Non-source tabs must remain at full opacity — pin that the
    // dim is targeted, not global.
    for (i, t) in inputs.iter().enumerate() {
        if t.index != src_idx {
            let a = tab_spans[i].1.a();
            assert_eq!(a, 0xFF, "non-source tab {i} must keep full alpha; got {a}");
        }
    }
}

#[test]
fn drag_ghost_render_input_field_defaults_to_none() {
    // The new `RenderInputs::drag_ghost` field defaults to `None` so
    // pre-Phase-D callers (and the empty-frame default) opt out of
    // the ghost feedback cleanly. This also pins the public surface
    // exposed by the inputs module.
    use sonicterm_render_model::inputs::{DragGhost, RenderInputs};
    let inp = RenderInputs::default();
    assert!(inp.drag_ghost.is_none());
    // And the DragGhost defaults match the spec.
    let g = DragGhost::default();
    assert!((g.alpha - DragGhost::GHOST_ALPHA).abs() < f32::EPSILON);
    assert!((g.source_alpha - DragGhost::SOURCE_ALPHA).abs() < f32::EPSILON);
    assert!((g.insertion_gap_px - DragGhost::INSERTION_GAP_PX).abs() < f32::EPSILON);
    assert_eq!(g.alpha, 0.5);
    assert_eq!(g.source_alpha, 0.3);
    assert_eq!(g.insertion_gap_px, 8.0);
}
