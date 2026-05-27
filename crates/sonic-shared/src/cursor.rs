//! Cursor shape + blink helpers.
//!
//! Kept GPU-free so unit tests can exercise the math without standing up
//! a wgpu surface. The render path calls [`blink_alpha`] once per frame
//! and uses [`phase_bucket`] to keep the [`crate::render::FrameKey`]
//! fast-path coherent — two frames with the same bucket render the same
//! cursor opacity, so a no-typing/no-output idle session still pegs the
//! cache and skips work between visible phase transitions.

use std::time::Duration;

pub use sonic_core::config::CursorShape;

/// One full blink cycle (off → on → off) in milliseconds. Matches the
/// 600ms cadence specified for v0.6 (close enough to wezterm's default
/// to feel familiar). The blink no longer drives the redraw schedule —
/// see `GpuRenderer::next_blink_redraw_at` — so this period only
/// affects the alpha computed on real redraws.
pub const BLINK_PERIOD_MS: u64 = 600;

/// Historical knob: number of distinct alpha steps per cycle. Since
/// the blink no longer drives the redraw schedule (the renderer skips
/// blink-only frames — see `GpuRenderer::next_blink_redraw_at`), this
/// only quantises the alpha computed on real redraws. Kept for the
/// public `phase_bucket` helper and existing tests.
pub const PHASE_BUCKETS: u8 = 16;

/// Minimum alpha during the trough of the blink. A non-zero floor keeps
/// the cursor *visible at all times* even mid-blink so the user never
/// loses their place — this is the wezterm behaviour, not the "fully
/// disappear" behaviour of legacy xterm.
const BLINK_MIN_ALPHA: f32 = 0.35;
const BLINK_MAX_ALPHA: f32 = 1.0;

/// Compute the current cursor-cell alpha for a blinking cursor.
///
/// When `enabled` is `false`, returns [`BLINK_MAX_ALPHA`] (solid).
/// Otherwise produces a smooth triangular wave with period
/// [`BLINK_PERIOD_MS`], bounded to `[BLINK_MIN_ALPHA, BLINK_MAX_ALPHA]`.
///
/// Triangular (not sine) so the output is exactly representable in
/// tests and the per-bucket alpha steps are evenly spaced — easier to
/// reason about when debugging "did my cursor blink this frame?".
pub fn blink_alpha(elapsed: Duration, enabled: bool) -> f32 {
    if !enabled {
        return BLINK_MAX_ALPHA;
    }
    let half = BLINK_PERIOD_MS / 2;
    let t = (elapsed.as_millis() as u64) % BLINK_PERIOD_MS;
    // Ramp 0..half goes max→min, half..period goes min→max.
    let frac =
        if t < half { t as f32 / half as f32 } else { 1.0 - ((t - half) as f32 / half as f32) };
    // frac is in [0,1] going up then down → invert so it starts at max.
    let down = 1.0 - frac;
    BLINK_MIN_ALPHA + (BLINK_MAX_ALPHA - BLINK_MIN_ALPHA) * down
}

/// Quantise the current phase into one of [`PHASE_BUCKETS`] discrete
/// values. Two frames with the same `enabled` setting and the same
/// bucket index will produce the same cursor pixels, so the FrameKey
/// can ride this bucket instead of the raw wall-clock and still skip
/// redundant work in steady state.
///
/// Always returns `0` when blinking is disabled so the bucket never
/// participates in the cache key on a non-blinking cursor.
pub fn phase_bucket(elapsed: Duration, enabled: bool) -> u8 {
    if !enabled {
        return 0;
    }
    let t = (elapsed.as_millis() as u64) % BLINK_PERIOD_MS;
    let idx = (t * PHASE_BUCKETS as u64) / BLINK_PERIOD_MS;
    (idx as u8) % PHASE_BUCKETS
}

/// Wall-clock interval the app loop should aim for between blink-only
/// redraws. Computed from [`BLINK_PERIOD_MS`] and [`PHASE_BUCKETS`] so
/// bumping one constant updates the schedule automatically.
pub fn redraw_interval() -> Duration {
    Duration::from_millis(BLINK_PERIOD_MS / PHASE_BUCKETS as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(
                (BLINK_MIN_ALPHA - 1e-4..=BLINK_MAX_ALPHA + 1e-4).contains(&a),
                "a={a} ms={ms}"
            );
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
        // Every bucket should be reachable inside one cycle.
        assert_eq!(seen.len(), PHASE_BUCKETS as usize);
    }

    #[test]
    fn redraw_interval_matches_constants() {
        let iv = redraw_interval();
        assert_eq!(iv, Duration::from_millis(BLINK_PERIOD_MS / PHASE_BUCKETS as u64));
        // Stays under one display frame at 60Hz at the default 16
        // buckets.
        assert!(iv <= Duration::from_millis(40));
    }

    #[test]
    fn cursor_shape_default_is_block() {
        assert_eq!(CursorShape::default(), CursorShape::Block);
    }
}
