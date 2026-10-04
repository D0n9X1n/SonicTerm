use std::time::{Duration, Instant};

use super::*;

/// A deadline crosses threads as whole microseconds since the shared origin and converts back to
/// the same instant; an instant earlier than the origin saturates to 0 rather than wrapping.
#[test]
fn microseconds_round_trip_through_the_shared_origin() {
    let base = origin();
    let later = base + Duration::from_millis(150);
    assert_eq!(micros_at(later), 150_000);
    assert_eq!(instant_at_micros(micros_at(later)), later);
    assert_eq!(micros_at(base + Duration::from_nanos(1_999)), 1, "truncated to whole microseconds");
    assert_eq!(micros_at(base), 0);
    if let Some(earlier) = base.checked_sub(Duration::from_millis(1)) {
        assert_eq!(micros_at(earlier), 0, "before the origin reads as the origin");
    }
    assert!(micros_at(Instant::now()) >= micros_at(base));
}
