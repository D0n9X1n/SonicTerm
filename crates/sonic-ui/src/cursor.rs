//! Cursor shape + blink helpers.
//!
//! Kept GPU-free so unit tests can exercise the math without standing up
//! a wgpu surface. The render path calls [`blink_alpha`] once per frame
//! and uses [`phase_bucket`] to keep the [`crate::render::FrameKey`]
//! fast-path coherent — two frames with the same bucket render the same
//! cursor opacity, so a no-typing/no-output idle session still pegs the
//! cache and skips work between visible phase transitions.

use std::time::Duration;

pub use sonic_cfg::config::CursorShape;

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
#[doc(hidden)]
pub const BLINK_MIN_ALPHA: f32 = 0.35;
#[doc(hidden)]
pub const BLINK_MAX_ALPHA: f32 = 1.0;

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

// Unit tests live in `tests/src_cursor.rs`.
