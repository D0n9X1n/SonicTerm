//! Verifies the macOS integrated-titlebar helper contract. The helper is
//! now an intentional no-op compatibility shim (the tab bar is bottom-
//! pinned, so there is no top strip to fuse with the native titlebar) —
//! see `sonicterm_app::app::with_integrated_titlebar`. This test asserts
//! that contract: the helper is callable from every window-creation site
//! AND leaves the platform-specific attributes untouched.

#[cfg(target_os = "macos")]
#[test]
fn integrated_titlebar_applied_on_macos() {
    use winit::window::Window;

    let base = Window::default_attributes().with_title("probe");
    let baseline = format!("{:?}", base);
    let out = sonicterm_app::app::with_integrated_titlebar(base);
    let dbg = format!("{:?}", out);

    // No-op contract: pass-through must not perturb the attributes.
    assert_eq!(
        baseline, dbg,
        "with_integrated_titlebar is now a documented no-op compat shim; \
         it must not modify WindowAttributes"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn integrated_titlebar_is_noop_off_macos() {
    use winit::window::Window;
    // Just confirm the helper is callable and returns without panic.
    let _ = sonicterm_app::app::with_integrated_titlebar(Window::default_attributes());
}

#[test]
fn backdrop_transparency_tracks_opaque_vs_material_backdrop() {
    use sonicterm_cfg::config::BackdropKind;
    use winit::window::Window;

    let opaque = sonicterm_app::app::with_backdrop_transparency(
        Window::default_attributes(),
        BackdropKind::Opaque,
    );
    let mica = sonicterm_app::app::with_backdrop_transparency(
        Window::default_attributes(),
        BackdropKind::Mica,
    );

    let opaque_dbg = format!("{opaque:?}");
    let mica_dbg = format!("{mica:?}");
    assert!(
        opaque_dbg.contains("transparent: false"),
        "opaque backdrop must keep the winit window opaque.\n{opaque_dbg}"
    );
    assert!(
        mica_dbg.contains("transparent: true"),
        "non-opaque backdrop must opt into winit window transparency.\n{mica_dbg}"
    );
}

#[test]
fn top_inset_helper_adds_titlebar_band() {
    use sonicterm_ui::tabbar_view::tab_bar_top_inset_with_titlebar;
    // Tab bar visible: titlebar inset stacks above the bar.
    let with = tab_bar_top_inset_with_titlebar(true, 4.0, 28.0);
    let without = tab_bar_top_inset_with_titlebar(true, 4.0, 0.0);
    assert!((with - without - 28.0).abs() < f32::EPSILON);
    // Tab bar hidden: titlebar inset + top padding still reserved.
    let hidden = tab_bar_top_inset_with_titlebar(false, 4.0, 28.0);
    assert!((hidden - 32.0).abs() < f32::EPSILON);
}
