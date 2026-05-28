//! Regression for issue #173 slice-2c (Toggle animation):
//!
//! Flipping a Toggle on must animate the sliding thumb from the
//! off-position to the on-position over [`Toggle::ANIM_MS`] (120ms).
//! At the halfway point the thumb must be strictly between the two
//! end positions — not snapped to either side. If a future change
//! drops the animation (e.g. by clearing `knob_anim_start` too
//! eagerly) the midpoint assertion fails.

use std::time::{Duration, Instant};

use sonic_shared::prefs::controls::{Rect, Toggle, WidgetId};
use sonic_shared::prefs::layout::{TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, TOGGLE_W};

#[test]
fn toggle_knob_x_animated_midpoint_is_between_ends() {
    let track = Rect::new(100.0, 0.0, TOGGLE_W, 24.0);
    let mut toggle = Toggle::new(WidgetId(1), "test", track, false);

    // Endpoints (snapped) before any animation.
    let off_pos = toggle.knob_x(TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);

    // Flip on — sets `value = true` and stamps `knob_anim_start`.
    toggle.toggle();
    let on_pos = toggle.knob_x(TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);
    assert!(
        on_pos > off_pos,
        "sanity: on-pos ({on_pos}) must be to the right of off-pos ({off_pos})"
    );

    // Simulate 60ms elapsed (halfway through the 120ms animation) by
    // back-dating the start. The renderer reads `Instant::now()`
    // every frame, so passing an explicit `now` lets the test pin a
    // deterministic moment.
    let start = toggle.knob_anim_start.expect("toggle() must stamp knob_anim_start");
    let now = start + Duration::from_millis(Toggle::ANIM_MS / 2);

    let mid = toggle.knob_x_animated(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    assert!(
        mid > off_pos && mid < on_pos,
        "at the 60ms midpoint the thumb must be between off-pos ({off_pos}) and on-pos ({on_pos}); got {mid}"
    );

    // Linear lerp at t=0.5 should land within 0.5px of the geometric
    // midpoint (allowing for the discrete ANIM_MS / 2 division).
    let geom_mid = (off_pos + on_pos) / 2.0;
    assert!(
        (mid - geom_mid).abs() < 0.5,
        "midpoint thumb should be ~halfway between ends; expected {geom_mid}, got {mid}"
    );
}

#[test]
fn toggle_knob_x_animated_snaps_after_anim_completes() {
    let track = Rect::new(50.0, 0.0, TOGGLE_W, 24.0);
    let mut toggle = Toggle::new(WidgetId(2), "test", track, false);

    toggle.toggle();
    let on_pos = toggle.knob_x(TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);

    let start = toggle.knob_anim_start.unwrap();
    let now = start + Duration::from_millis(Toggle::ANIM_MS + 50);

    let pos = toggle.knob_x_animated(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    assert!(
        (pos - on_pos).abs() < 1e-4,
        "after ANIM_MS has elapsed, knob must snap to on-pos ({on_pos}); got {pos}"
    );
}

#[test]
fn toggle_knob_x_animated_no_animation_returns_snapped() {
    let track = Rect::new(0.0, 0.0, TOGGLE_W, 24.0);
    let toggle = Toggle::new(WidgetId(3), "test", track, true);

    // No flip has happened => knob_anim_start is None => the helper
    // returns the snapped end position regardless of `now`.
    let on_pos = toggle.knob_x(TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);
    let pos = toggle.knob_x_animated(Instant::now(), TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    assert!((pos - on_pos).abs() < 1e-4);
}
