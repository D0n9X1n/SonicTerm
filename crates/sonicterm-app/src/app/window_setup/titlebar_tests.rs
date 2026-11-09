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
