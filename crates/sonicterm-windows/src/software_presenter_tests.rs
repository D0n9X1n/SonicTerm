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
