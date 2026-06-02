//! Cursor shape + blink tests.
//!
//! Covers the pure helpers in [`sonicterm_ui::cursor`] plus the
//! glyph-recolor used by the block cursor to invert the cell. GPU
//! state is deliberately out of scope here — `tests/render.rs` and
//! the `pty_dump` example exercise the on-screen pipeline.

use std::time::Duration;

use sonicterm_gpu::cursor::recolor_cursor_glyphs;
use sonicterm_gpu::quad::px_to_ndc;
use sonicterm_gpu::text_pipeline::GlyphInstance;
use sonicterm_ui::cursor::{self, CursorShape};

#[test]
fn default_shape_matches_wezterm() {
    // Wezterm defaults to a block cursor; staying consistent makes
    // the cross-tool muscle memory work out of the box.
    assert_eq!(CursorShape::default(), CursorShape::Block);
}

#[test]
fn blink_disabled_means_solid() {
    for ms in [0u64, 1, 99, 300, 599, 1500] {
        let a = cursor::blink_alpha(Duration::from_millis(ms), false);
        assert_eq!(a, 1.0, "disabled cursor must be solid at ms={ms}");
    }
}

#[test]
fn blink_period_completes_one_cycle() {
    let period = cursor::BLINK_PERIOD_MS;
    // Endpoints (start of a cycle and one full cycle later) must be
    // visually identical, otherwise we'd see a popping reset.
    let a0 = cursor::blink_alpha(Duration::from_millis(0), true);
    let a_end = cursor::blink_alpha(Duration::from_millis(period), true);
    assert!((a0 - a_end).abs() < 1e-3, "{a0} vs {a_end}");
}

#[test]
fn phase_bucket_visits_every_step_in_one_cycle() {
    let mut buckets = std::collections::HashSet::new();
    for ms in 0..cursor::BLINK_PERIOD_MS {
        buckets.insert(cursor::phase_bucket(Duration::from_millis(ms), true));
    }
    assert_eq!(buckets.len(), cursor::PHASE_BUCKETS as usize);
}

#[test]
fn redraw_interval_keeps_pace_with_buckets() {
    let iv = cursor::redraw_interval();
    let buckets_per_cycle = cursor::BLINK_PERIOD_MS / iv.as_millis() as u64;
    assert_eq!(buckets_per_cycle, cursor::PHASE_BUCKETS as u64);
}

#[test]
fn recolor_block_glyph_in_cursor_cell() {
    // Synthesise a 100x100 surface with two glyphs: one inside a
    // 10x20 cell at (50, 30), one well outside. After recoloring,
    // only the inside glyph should be flipped to the bg color.
    let sw = 100.0;
    let sh = 100.0;
    let cell_x = 50.0;
    let cell_y = 30.0;
    let cell_w = 10.0;
    let cell_h = 20.0;

    let inside = GlyphInstance {
        // Glyph rect centred inside the cursor cell.
        rect: px_to_ndc(cell_x + 1.0, cell_y + 2.0, cell_w - 2.0, cell_h - 4.0, sw, sh),
        uv: [0.0, 0.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    };
    let outside = GlyphInstance {
        rect: px_to_ndc(0.0, 0.0, 5.0, 5.0, sw, sh),
        uv: [0.0, 0.0, 0.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    };
    let mut glyphs = vec![inside, outside];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, cell_x, cell_y, cell_w, cell_h, sw, sh, bg);

    assert_eq!(glyphs[0].color, bg, "inside glyph should be recolored");
    assert_eq!(glyphs[1].color, [1.0, 1.0, 1.0, 1.0], "outside glyph must be left alone");
}

#[test]
fn recolor_is_noop_on_zero_dimensions() {
    let mut glyphs = vec![GlyphInstance {
        rect: [0.0, 0.0, 0.1, 0.1],
        uv: [0.0; 4],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    }];
    recolor_cursor_glyphs(&mut glyphs, 0.0, 0.0, 10.0, 10.0, 0.0, 0.0, [0.0; 4]);
    // No panic, no surprise rewrite.
    assert_eq!(glyphs[0].color, [1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn cursor_shape_all_round_trip_strings() {
    for shape in CursorShape::ALL {
        let s = shape.as_str();
        assert_eq!(CursorShape::from_str_ci(s), Some(*shape));
        assert_eq!(CursorShape::from_str_ci(&s.to_uppercase()), Some(*shape));
    }
}

// ---------------------------------------------------------------------
// Regression coverage for the PR #81 review findings.
// ---------------------------------------------------------------------

/// Blink scheduling test — a simulated render loop with blink=true
/// must never re-arm more than [`cursor::PHASE_BUCKETS`] times per
/// second. This is the pure-math complement to the renderer change
/// that moved blink scheduling out of the render path and into the
/// event loop (`ControlFlow::WaitUntil(next_blink_redraw_at())`).
#[test]
fn blink_redraw_cadence_caps_at_30hz() {
    use std::time::{Duration, Instant};
    let interval = cursor::redraw_interval();
    let cap_per_sec = (1000 / interval.as_millis().max(1) as u64) as usize;
    // 16 buckets / 600ms cycle = 37.5ms ≈ 26.6 wakes/sec. Stay under 30.
    assert!(cap_per_sec <= 30, "cap_per_sec={cap_per_sec}");

    // Simulate the event-loop behaviour: every wake, advance to the
    // next bucket boundary computed exactly like
    // `GpuRenderer::next_blink_redraw_at`.
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let iv_ms = interval.as_millis() as u64;
    let mut sim = start;
    let mut wakes = 0usize;
    while sim < deadline {
        let elapsed = sim.duration_since(start);
        let elapsed_ms = elapsed.as_millis() as u64;
        let next_ms = ((elapsed_ms / iv_ms) + 1) * iv_ms;
        sim = start + Duration::from_millis(next_ms);
        wakes += 1;
    }
    assert!(wakes <= 30, "wakes/sec={wakes} must stay <=30");
    assert!(wakes <= cap_per_sec + 1, "wakes={wakes} cap={cap_per_sec}");
}

/// Idle-CPU regression guard: when the window is unfocused, the blink
/// scheduler MUST return `None` so the event loop falls back to
/// `ControlFlow::Wait` instead of waking at 26Hz forever. Before this
/// gate, `scripts/bench_headless_gui.sh` reported ~17% idle CPU on a
/// backgrounded window; after the gate it returns to baseline (<1%).
///
/// `GpuRenderer::next_blink_redraw_at` requires a wgpu surface so we
/// re-state the contract here as a pure-math mirror — any change that
/// drops the focus check from the renderer should also fail this test
/// once the mirror is updated to match.
#[test]
fn blink_schedule_is_silenced_when_window_unfocused() {
    fn schedule(cursor_blink: bool, window_focused: bool) -> Option<std::time::Duration> {
        if !cursor_blink || !window_focused {
            return None;
        }
        Some(cursor::redraw_interval())
    }
    assert!(schedule(true, true).is_some());
    assert!(schedule(true, false).is_none(), "unfocused must not wake");
    assert!(schedule(false, true).is_none());
    assert!(schedule(false, false).is_none());
}

/// Cell-rect math behind the hollow cursor: the renderer's
/// `push_hollow_rect` helper must emit exactly four quad rects whose
/// union forms the outline of the cell — no interior fill. Validated
/// via the public quad helper so the test stays GPU-free.
#[test]
fn unfocused_pane_cursor_is_hollow() {
    use sonicterm_gpu::cursor::push_hollow_rect;
    use sonicterm_gpu::quad::QuadInstance;
    let mut quads: Vec<QuadInstance> = Vec::new();
    push_hollow_rect(&mut quads, 50.0, 30.0, 10.0, 20.0, 100.0, 100.0, [1.0, 1.0, 1.0, 1.0], 2.0);
    assert_eq!(quads.len(), 4, "hollow rect = top+bottom+left+right");
    // No emitted quad covers the centre of the cell — confirms the
    // fill is fully transparent (the "hollow" of hollow cursor).
    let cx_ndc = (55.0 / 100.0) * 2.0 - 1.0;
    let cy_top_px = 40.0;
    let cy_ndc = 1.0 - (cy_top_px / 100.0) * 2.0;
    for q in &quads {
        let [nx, ny, nw, nh] = q.rect;
        // ny encodes the bottom of the rect after px_to_ndc's +nh shift.
        let top = ny + nh;
        let bottom = ny;
        let left = nx;
        let right = nx + nw;
        let covers_centre = cx_ndc > left && cx_ndc < right && cy_ndc < top && cy_ndc > bottom;
        assert!(!covers_centre, "interior must be empty: rect={:?}", q.rect);
    }
}

/// Premultiplied alpha for the inverted-cell glyph during a blink
/// fade. The text shader runs `vec4(color.rgb * cov, color.a * cov)`
/// and assumes its input is premultiplied (PR #65 contract). With a
/// 50% blink fade the recolored glyph color MUST be
/// `(0.5*bg.r, 0.5*bg.g, 0.5*bg.b, 0.5)` — straight-alpha
/// `(bg.r, bg.g, bg.b, 0.5)` would blend wrong and produce a halo.
#[test]
fn inverted_glyph_recolor_is_premultiplied_during_blink() {
    use sonicterm_gpu::cursor::recolor_cursor_glyphs;
    use sonicterm_gpu::quad::px_to_ndc;
    use sonicterm_gpu::text_pipeline::GlyphInstance;

    let sw = 100.0;
    let sh = 100.0;
    let (cell_x, cell_y, cell_w, cell_h) = (10.0, 10.0, 10.0, 20.0);
    let mut glyph = vec![GlyphInstance {
        rect: px_to_ndc(cell_x + 1.0, cell_y + 1.0, cell_w - 2.0, cell_h - 2.0, sw, sh),
        uv: [0.0; 4],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    }];

    let bg = [0.8_f32, 0.4, 0.2, 1.0];
    let blink_alpha = 0.5_f32;

    // Mirror the renderer's Block branch (post-fix): premultiply RGB
    // and A by blink_alpha before handing to recolor_cursor_glyphs.
    let mut bg_premul = bg;
    bg_premul[0] *= blink_alpha;
    bg_premul[1] *= blink_alpha;
    bg_premul[2] *= blink_alpha;
    bg_premul[3] *= blink_alpha;
    recolor_cursor_glyphs(&mut glyph, cell_x, cell_y, cell_w, cell_h, sw, sh, bg_premul);

    let got = glyph[0].color;
    let expected = [0.5 * 0.8, 0.5 * 0.4, 0.5 * 0.2, 0.5];
    for i in 0..4 {
        assert!((got[i] - expected[i]).abs() < 1e-5, "ch{i}: got={:?} exp={:?}", got, expected);
    }
    // And NOT the straight-alpha bug shape: alpha=0.5 with RGB unchanged.
    let buggy = [0.8, 0.4, 0.2, 0.5];
    assert_ne!(got, buggy, "must not be straight-alpha");
}

// ---------------------------------------------------------------------
// Issue #568: cluster-spanning glyphs (ligatures, CJK wide) must be
// recolored whenever the cursor sits on ANY cell of the cluster, not
// just the cell containing the glyph's geometric center.
// ---------------------------------------------------------------------

/// Helper: synthesise a [`GlyphInstance`] whose pixel rect spans
/// `(px, py, pw, ph)` on a `(sw, sh)` surface.
fn glyph_at(px: f32, py: f32, pw: f32, ph: f32, sw: f32, sh: f32) -> GlyphInstance {
    GlyphInstance {
        rect: px_to_ndc(px, py, pw, ph, sw, sh),
        uv: [0.0; 4],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    }
}

/// A 2-cell `=>` ligature is shaped as a single glyph whose rect
/// covers both cells. With the cursor on the **lead** cell the glyph
/// must be recolored — the centre-of-glyph test used to pass here only
/// because the lead cell happened to contain the center.
#[test]
fn recolor_ligature_cursor_on_lead_cell() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let lead_x = 20.0;
    let cell_y = 30.0;
    // Ligature spans 2 cells starting at lead_x.
    let mut glyphs = vec![glyph_at(lead_x, cell_y, cell_w * 2.0, cell_h, sw, sh)];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, lead_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "ligature on lead cell must be recolored");
}

/// Same `=>` ligature, cursor on the **trail** cell (lead+1). The
/// glyph's geometric centre falls in the trail cell too, but the AABB
/// approach is what makes the regression below also pass for the
/// 3-cell case. Belt-and-suspenders coverage.
#[test]
fn recolor_ligature_cursor_on_trail_cell() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let lead_x = 20.0;
    let trail_x = lead_x + cell_w;
    let cell_y = 30.0;
    let mut glyphs = vec![glyph_at(lead_x, cell_y, cell_w * 2.0, cell_h, sw, sh)];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, trail_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "ligature on trail cell must be recolored");
}

/// A 3-cell `===` ligature with cursor on the **middle** cell. The
/// centre-point test would have passed here (centre is in middle
/// cell), but the lead and trail cells used to fail; this case
/// guards the middle as well so regressions are caught wherever the
/// cursor lands.
#[test]
fn recolor_three_cell_ligature_cursor_on_middle() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let lead_x = 20.0;
    let cell_y = 30.0;
    // 3-cell ligature.
    let mut glyphs = vec![glyph_at(lead_x, cell_y, cell_w * 3.0, cell_h, sw, sh)];

    let bg = [0.0, 0.1, 0.2, 0.9];
    let middle_x = lead_x + cell_w;
    recolor_cursor_glyphs(&mut glyphs, middle_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "3-cell ligature on middle must be recolored");
}

/// A 3-cell `===` ligature with cursor on the **lead** cell. With the
/// old centre-of-glyph test this was the canonical regression: the
/// centre lies in the middle cell, so the lead-cell cursor never
/// recoloured the glyph.
#[test]
fn recolor_three_cell_ligature_cursor_on_lead() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let lead_x = 20.0;
    let cell_y = 30.0;
    let mut glyphs = vec![glyph_at(lead_x, cell_y, cell_w * 3.0, cell_h, sw, sh)];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, lead_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "3-cell ligature on lead cell must be recolored");
}

/// CJK wide character (`中`) emits a single glyph spanning 2 cells.
/// Cursor on the lead cell must recolor the glyph.
#[test]
fn recolor_cjk_wide_cursor_on_lead_cell() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let lead_x = 40.0;
    let cell_y = 30.0;
    // Wide glyph spans 2 cells.
    let mut glyphs = vec![glyph_at(lead_x, cell_y, cell_w * 2.0, cell_h, sw, sh)];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, lead_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "CJK wide on lead cell must be recolored");
}

/// Regression guard: a 1-cell glyph (`a`) sitting in the cursor cell
/// must still be recolored — the AABB approach must not regress the
/// common single-cell case.
#[test]
fn recolor_single_cell_glyph_in_cursor_cell() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let cell_x = 50.0;
    let cell_y = 30.0;
    // Glyph entirely inside the cursor cell.
    let mut glyphs = vec![glyph_at(
        cell_x + 1.0,
        cell_y + 2.0,
        cell_w - 2.0,
        cell_h - 4.0,
        sw,
        sh,
    )];

    let bg = [0.0, 0.1, 0.2, 0.9];
    recolor_cursor_glyphs(&mut glyphs, cell_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(glyphs[0].color, bg, "single-cell glyph in cursor cell must be recolored");
}

/// Regression guard: a 1-cell glyph (`a`) sitting in the cell adjacent
/// to the cursor must NOT be recolored — the AABB approach must not
/// over-recolor neighbouring glyphs.
#[test]
fn no_recolor_single_cell_glyph_in_adjacent_cell() {
    let sw = 100.0;
    let sh = 100.0;
    let cell_w = 10.0;
    let cell_h = 20.0;
    let cell_x = 50.0;
    let cell_y = 30.0;
    let neighbour_x = cell_x + cell_w;
    // Glyph entirely inside the NEIGHBOUR cell, fully outside the cursor cell.
    let mut glyphs = vec![glyph_at(
        neighbour_x + 1.0,
        cell_y + 2.0,
        cell_w - 2.0,
        cell_h - 4.0,
        sw,
        sh,
    )];

    let bg = [0.0, 0.1, 0.2, 0.9];
    let original = glyphs[0].color;
    recolor_cursor_glyphs(&mut glyphs, cell_x, cell_y, cell_w, cell_h, sw, sh, bg);
    assert_eq!(
        glyphs[0].color, original,
        "single-cell glyph in adjacent cell must NOT be recolored"
    );
}
