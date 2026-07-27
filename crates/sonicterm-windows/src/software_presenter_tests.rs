use super::*;

#[test]
fn force_prefers_software_presenter() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Force);
    assert!(pref.should_use(false));
    assert!(pref.forces_opaque_window());
}

#[test]
fn auto_follows_detection() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Auto);
    assert!(pref.should_use(true));
    assert!(!pref.should_use(false));
    assert!(!pref.forces_opaque_window());
}

#[test]
fn off_never_uses_software_presenter() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Off);
    assert!(!pref.should_use(true));
    assert!(!pref.should_use(false));
}

/// The app layer and the Windows presenter each decide the software path
/// from their own copy of the `(mode, detected)` pair. A mode honored by one
/// layer and ignored by the other is a half-degraded renderer, so the two
/// decisions must stay identical over the whole input domain.
#[test]
fn presenter_and_app_degrade_decisions_agree_across_all_modes() {
    for mode in [SoftwareRenderMode::Auto, SoftwareRenderMode::Force, SoftwareRenderMode::Off] {
        for detected in [false, true] {
            let presenter =
                WindowsSoftwarePresenterPreference::from_config(mode).should_use(detected);
            let app = sonicterm_app::app::should_degrade_for_software_render(mode, detected);
            assert_eq!(
                presenter, app,
                "presenter and app disagree for mode {mode:?} with detected={detected}"
            );
        }
    }
}

/// Pins the truth table both layers are required to implement: `Auto` defers
/// to detection, `Force` degrades regardless, `Off` never degrades. Guards
/// against the two implementations drifting together into a shared mistake,
/// which the agreement test alone would not catch.
#[test]
fn degrade_decision_truth_table_is_pinned() {
    let expected = [
        (SoftwareRenderMode::Auto, false, false),
        (SoftwareRenderMode::Auto, true, true),
        (SoftwareRenderMode::Force, false, true),
        (SoftwareRenderMode::Force, true, true),
        (SoftwareRenderMode::Off, false, false),
        (SoftwareRenderMode::Off, true, false),
    ];
    for (mode, detected, want) in expected {
        assert_eq!(
            WindowsSoftwarePresenterPreference::from_config(mode).should_use(detected),
            want,
            "presenter should_use({mode:?}, {detected})"
        );
        assert_eq!(
            sonicterm_app::app::should_degrade_for_software_render(mode, detected),
            want,
            "app should_degrade_for_software_render({mode:?}, {detected})"
        );
    }
}

/// Characterizes the deliberate asymmetry between the two questions the
/// presenter preference answers. `should_use` reports whether the software
/// path applies at all and follows detection under `Auto`; only `Force`
/// additionally overrides the configured DWM backdrop to opaque. `Auto`
/// therefore leaves a user's translucent backdrop intact even when it
/// software-presents, and `forces_opaque_window` is independent of detection.
#[test]
fn only_force_overrides_the_configured_backdrop() {
    for detected in [false, true] {
        let auto = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Auto);
        assert_eq!(auto.should_use(detected), detected);
        assert!(!auto.forces_opaque_window());

        let force = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Force);
        assert!(force.should_use(detected));
        assert!(force.forces_opaque_window());

        let off = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Off);
        assert!(!off.should_use(detected));
        assert!(!off.forces_opaque_window());
    }
}
