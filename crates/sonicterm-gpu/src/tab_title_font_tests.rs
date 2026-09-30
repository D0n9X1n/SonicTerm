use super::*;
use sonicterm_render_model::boundary::ui::tabs::Tab;

/// Serializes the tracked test font stacks, which share process-wide font state.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The width the bar stores for its first tab after `font` measures it, with the bar held.
fn stored_width(font: &TabTitleFont, tabs: &mut TabBar, now: Instant) -> f32 {
    font.measure(tabs, false, true, now);
    tabs.tabs()[0].content_width_px().expect("a width was stored")
}

#[test]
fn a_font_or_scale_change_changes_the_stored_tab_width() {
    // A font change and a scale change, each applied through the transition the renderer runs,
    // give the next measurement a new key, so the title is shaped again and the bar stores the
    // new width even while it holds its widths. The stacks come from the bundled test font at
    // the title size the renderer derives, at 72 dpi for scale 1, as the renderer builds them.
    let _lock = font_fixture_lock();
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("cargo build"));
    let now = Instant::now();
    let title_stack = |body_size: f32| {
        Some(crate::lib_tests::tracked_font_stack(f64::from(tab_title_font_size(body_size))))
    };
    let mut font = TabTitleFont::new("Rec Mono St.Helens", 14.0, 1.0, 1.0, title_stack(14.0));
    let base = stored_width(&font, &mut tabs, now);

    font.set_font("Rec Mono St.Helens", 28.0, 1.0, title_stack(28.0));
    let larger_font = stored_width(&font, &mut tabs, now);
    assert!(larger_font > base * 1.5, "a larger font kept the width: {base} -> {larger_font}");

    font.set_scale_factor(2.0, 144);
    let larger_scale = stored_width(&font, &mut tabs, now);
    assert!(
        larger_scale > larger_font * 1.5,
        "a larger scale kept the width: {larger_font} -> {larger_scale}"
    );
}
