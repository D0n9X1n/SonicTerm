use std::time::{Duration, Instant};

use sonic_ui::prefs::controls::{Rect, Toggle, WidgetId};
use sonic_ui::prefs::layout::{TOGGLE_KNOB, TOGGLE_KNOB_MARGIN};

#[test]
fn reduced_motion_snaps_toggle_thumb_to_end_position() {
    let start = Instant::now();
    let mut toggle =
        Toggle::new(WidgetId(1), "Reduced motion", Rect::new(10.0, 0.0, 44.0, 24.0), false);
    toggle.toggle();
    toggle.knob_anim_start = Some(start);

    let now = start + Duration::from_millis(Toggle::ANIM_MS / 2);
    let snapped = toggle.knob_x(TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);
    let interpolated = toggle.knob_x_animated(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    let reduced = toggle.knob_x_animated(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, true);

    assert_eq!(reduced, snapped);
    assert_ne!(interpolated, snapped);
}
