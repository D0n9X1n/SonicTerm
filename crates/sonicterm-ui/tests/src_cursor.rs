//! Tests for cursor blink-alpha + phase-bucket math.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/cursor.rs`.
//! Named `src_cursor.rs` to distinguish from the existing
//! `crates/sonicterm-shared/tests/cursor.rs` integration test in a sibling crate.

use std::time::Duration;

use sonicterm_ui::cursor::{
    blink_alpha, phase_bucket, redraw_interval, CursorShape, BLINK_MAX_ALPHA, BLINK_MIN_ALPHA,
    BLINK_PERIOD_MS, PHASE_BUCKETS,
};

#[test]
fn disabled_blink_is_solid() {
    for ms in [0u64, 100, 299, 300, 599, 1000, 12_345] {
        assert_eq!(blink_alpha(Duration::from_millis(ms), false), BLINK_MAX_ALPHA);
        assert_eq!(phase_bucket(Duration::from_millis(ms), false), 0);
    }
}

#[test]
fn enabled_blink_starts_at_max_and_dips_to_min() {
    let a0 = blink_alpha(Duration::ZERO, true);
    let a_half = blink_alpha(Duration::from_millis(BLINK_PERIOD_MS / 2), true);
    assert!((a0 - BLINK_MAX_ALPHA).abs() < 1e-3, "a0={a0}");
    assert!((a_half - BLINK_MIN_ALPHA).abs() < 1e-3, "a_half={a_half}");
}

#[test]
fn alpha_is_bounded() {
    for ms in 0..=BLINK_PERIOD_MS * 3 {
        let a = blink_alpha(Duration::from_millis(ms), true);
        assert!((BLINK_MIN_ALPHA - 1e-4..=BLINK_MAX_ALPHA + 1e-4).contains(&a), "a={a} ms={ms}");
    }
}

#[test]
fn alpha_is_periodic() {
    for ms in 0..BLINK_PERIOD_MS {
        let a1 = blink_alpha(Duration::from_millis(ms), true);
        let a2 = blink_alpha(Duration::from_millis(ms + BLINK_PERIOD_MS), true);
        let a3 = blink_alpha(Duration::from_millis(ms + 5 * BLINK_PERIOD_MS), true);
        assert!((a1 - a2).abs() < 1e-5);
        assert!((a1 - a3).abs() < 1e-5);
    }
}

#[test]
fn phase_bucket_wraps_within_range() {
    for ms in 0..BLINK_PERIOD_MS * 4 {
        let b = phase_bucket(Duration::from_millis(ms), true);
        assert!(b < PHASE_BUCKETS);
    }
}

#[test]
fn phase_bucket_changes_within_a_cycle() {
    let mut seen = std::collections::HashSet::new();
    for ms in 0..BLINK_PERIOD_MS {
        seen.insert(phase_bucket(Duration::from_millis(ms), true));
    }
    assert_eq!(seen.len(), PHASE_BUCKETS as usize);
}

#[test]
fn redraw_interval_matches_constants() {
    let iv = redraw_interval();
    assert_eq!(iv, Duration::from_millis(BLINK_PERIOD_MS / PHASE_BUCKETS as u64));
    assert!(iv <= Duration::from_millis(40));
}

#[test]
fn cursor_shape_default_is_block() {
    assert_eq!(CursorShape::default(), CursorShape::Block);
}
