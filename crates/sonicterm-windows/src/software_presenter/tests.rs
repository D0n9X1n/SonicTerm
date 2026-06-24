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

