use super::TearOutTiming;
use std::time::{Duration, Instant};

#[test]
fn tear_out_first_render_total_is_measured_from_start() {
    let start = Instant::now();
    let timing = TearOutTiming::new("main", start);
    let total = timing.total_until_first_render_ms(start + Duration::from_millis(42));

    assert!((41.0..=43.0).contains(&total));
}

