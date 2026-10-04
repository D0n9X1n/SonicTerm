use std::time::{Duration, Instant};

use super::*;

/// A deadline crosses threads as nanoseconds since the shared origin; an instant earlier than
/// the origin saturates to 0 rather than wrapping.
#[test]
fn nanoseconds_round_trip_through_the_shared_origin() {
    let base = origin();
    let later = base + Duration::from_millis(150);
    assert_eq!(nanos_at(later), 150_000_000);
    assert_eq!(nanos_at(base), 0);
    if let Some(earlier) = base.checked_sub(Duration::from_millis(1)) {
        assert_eq!(nanos_at(earlier), 0, "before the origin reads as the origin");
    }
    assert!(nanos_at(Instant::now()) >= nanos_at(base));
}
