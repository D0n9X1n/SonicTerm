use super::*;

/// Disabling blink makes opacity and frame-key phase independent of elapsed time.
#[test]
fn disabled_blink_is_stable_and_opaque() {
    for elapsed in [0, 299, 599, 5_000] {
        let elapsed = Duration::from_millis(elapsed);
        assert_eq!(blink_alpha(elapsed, false), BLINK_MAX_ALPHA);
        assert_eq!(phase_bucket(elapsed, false), 0);
    }
}

/// The triangular alpha wave reaches both bounds and repeats at the declared period.
#[test]
fn blink_alpha_hits_bounds_and_repeats() {
    assert_eq!(blink_alpha(Duration::ZERO, true), BLINK_MAX_ALPHA);
    assert_eq!(blink_alpha(Duration::from_millis(BLINK_PERIOD_MS / 2), true), BLINK_MIN_ALPHA);
    assert_eq!(blink_alpha(Duration::from_millis(BLINK_PERIOD_MS), true), BLINK_MAX_ALPHA);
}

/// Phase buckets cover one cycle in order and wrap exactly at the blink period.
#[test]
fn phase_bucket_quantizes_one_full_cycle() {
    assert_eq!(phase_bucket(Duration::ZERO, true), 0);
    assert_eq!(phase_bucket(Duration::from_millis(BLINK_PERIOD_MS - 1), true), PHASE_BUCKETS - 1);
    assert_eq!(phase_bucket(Duration::from_millis(BLINK_PERIOD_MS), true), 0);
    assert_eq!(redraw_interval(), Duration::from_millis(BLINK_PERIOD_MS / PHASE_BUCKETS as u64));
}
