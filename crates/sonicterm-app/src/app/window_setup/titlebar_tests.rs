//! Titlebar mode selection and the double-click preference mapping, on every host.

use super::*;

/// Only an integrated mode on macOS or Windows gets the band; native never does.
#[test]
fn only_integrated_mode_on_macos_or_windows_gets_a_band() {
    assert!(!integrated(TitlebarMode::Native));
    assert_eq!(
        integrated(TitlebarMode::Integrated),
        cfg!(any(target_os = "macos", target_os = "windows"))
    );
    assert_eq!(creation_band_estimate(TitlebarMode::Native), 0.0);
}

/// Windows' caption buttons are on the right, so its band moves the right list left; every other
/// platform keeps the left edge, which leaves the band's layout as it was.
#[test]
fn the_button_edge_follows_the_platform() {
    use sonicterm_ui::window_band::ButtonEdge;
    let expected = if cfg!(target_os = "windows") { ButtonEdge::Right } else { ButtonEdge::Left };
    assert_eq!(button_edge(), expected);
}

/// The macOS preference maps to its action, and an absent or unknown value is zoom, the system
/// default.
#[test]
fn the_double_click_preference_maps_to_its_action() {
    assert_eq!(double_click_action(Some("Minimize")), DoubleClickAction::Minimize);
    assert_eq!(double_click_action(Some("None")), DoubleClickAction::None);
    assert_eq!(double_click_action(Some("Maximize")), DoubleClickAction::Maximize);
    assert_eq!(double_click_action(None), DoubleClickAction::Maximize);
    assert_eq!(double_click_action(Some("Fill")), DoubleClickAction::Maximize);
}

/// A switching window still has the other mode until its native titlebar follows, and only macOS
/// can switch a live window's titlebar.
#[test]
fn the_other_mode_and_the_switchable_platform() {
    use sonicterm_cfg::config::TitlebarMode::{Integrated, Native};
    assert_eq!(other_mode(Integrated), Native);
    assert_eq!(other_mode(Native), Integrated);
    // A window waiting for full screen to end still shows the titlebar it had.
    assert_eq!(shown_mode(Integrated, true), Integrated);
    assert_eq!(shown_mode(Integrated, false), Native);
    assert_eq!(titlebar_switchable(), cfg!(target_os = "macos"));
}
