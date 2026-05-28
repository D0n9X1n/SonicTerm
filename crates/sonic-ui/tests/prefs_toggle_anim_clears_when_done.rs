use std::time::Duration;

use sonic_ui::prefs::controls::{Rect, Toggle, WidgetId};
use sonic_ui::prefs::layout::{TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, TOGGLE_W};

#[test]
fn toggle_animation_clears_after_done_frame() {
    let track = Rect::new(0.0, 0.0, TOGGLE_W, 24.0);
    let mut toggle = Toggle::new(WidgetId(1), "test", track, false);

    toggle.toggle();
    let start = toggle.knob_anim_start.expect("toggle starts animation");
    let now = start + Duration::from_millis(Toggle::ANIM_MS + 1);

    let _x = toggle.knob_x_animated(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    let (_, done) = toggle.knob_x_animated_with_done(now, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, false);
    toggle.clear_anim_if_done(done);

    assert!(toggle.knob_anim_start.is_none(), "completed toggle animation must be cleared");
}
