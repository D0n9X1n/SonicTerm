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

#[test]
fn dirty_rect_clips_to_surface() {
    assert_eq!(
        DirtyRect { x: 8, y: 9, w: 8, h: 8 }.clipped(10, 12),
        Some(DirtyRect { x: 8, y: 9, w: 2, h: 3 })
    );
    assert_eq!(DirtyRect { x: 10, y: 0, w: 1, h: 1 }.clipped(10, 12), None);
}

#[test]
fn fill_rect_updates_pixels_and_dirty_set() {
    let mut surface = SoftwareSurface::new(4, 3);
    surface.fill_rect_bgra(DirtyRect { x: 1, y: 1, w: 2, h: 1 }, [1, 2, 3, 4]);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 1, y: 1, w: 2, h: 1 }]);
    let stride = surface.width() as usize * 4;
    assert_eq!(&surface.pixels()[stride + 4..stride + 8], &[1, 2, 3, 4]);
    assert_eq!(&surface.pixels()[stride + 8..stride + 12], &[1, 2, 3, 4]);
    assert_eq!(&surface.pixels()[0..4], &[0, 0, 0, 0]);
}

#[test]
fn software_surface_size_is_checked_and_bounded() {
    assert_eq!(pixel_len(7680, 4320), Some(7680 * 4320 * 4));
    assert_eq!(pixel_len(8192, 4320), Some(8192 * 4320 * 4));
    assert_eq!(pixel_len(8192, 8192), None);
    assert_eq!(pixel_len(u32::MAX, u32::MAX), None);
}

#[test]
fn software_surface_shrink_releases_capacity() {
    let mut surface = SoftwareSurface::try_new(1024, 1024).expect("valid surface");
    let old_capacity = surface.pixels.capacity();

    assert!(surface.try_resize(2, 2));

    assert!(surface.pixels.capacity() < old_capacity / 2);
}

#[test]
fn software_surface_growth_uses_exact_validated_capacity() {
    let mut surface = SoftwareSurface::try_new(2, 2).expect("valid surface");

    assert!(surface.try_resize(100, 100));

    assert_eq!(surface.pixels.capacity(), 100 * 100 * 4);
}
