//! Cursor shape + blink tests.
//!
//! Covers the pure helpers in [`sonic_shared::cursor`] plus the
//! glyph-recolor used by the block cursor to invert the cell. GPU
//! state is deliberately out of scope here — `tests/render.rs` and
//! the `pty_dump` example exercise the on-screen pipeline.

use std::time::Duration;

use sonic_shared::cursor::{self, CursorShape};
use sonic_shared::quad::px_to_ndc;
use sonic_shared::render::recolor_cursor_glyphs;
use sonic_shared::text_pipeline::GlyphInstance;

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
