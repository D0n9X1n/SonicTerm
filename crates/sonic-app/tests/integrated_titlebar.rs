//! Verifies that the macOS integrated-titlebar helper is wired into the
//! `WindowAttributes` builder. We can't construct a real `Window` in a
//! headless test, and winit's per-platform attributes struct is not
//! publicly readable, so we use the `Debug` representation as a stable
//! probe — it includes the macOS platform-specific fields.

#[cfg(target_os = "macos")]
#[test]
fn integrated_titlebar_applied_on_macos() {
    use winit::window::Window;

    let base = Window::default_attributes().with_title("probe");
    let baseline = format!("{:?}", base);
    let out = sonic_app::app::with_integrated_titlebar(base);
    let dbg = format!("{:?}", out);

    // Sanity: baseline must NOT already have the flags set.
    assert!(
        baseline.contains("fullsize_content_view: false"),
        "winit changed default; update test. baseline = {baseline}"
    );

    // The helper must flip both fields on macOS.
    assert!(
        dbg.contains("fullsize_content_view: true"),
        "fullsize_content_view must be enabled for integrated titlebar.\n{dbg}"
    );
    assert!(
        dbg.contains("titlebar_transparent: true"),
        "titlebar_transparent must be enabled for integrated titlebar.\n{dbg}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn integrated_titlebar_is_noop_off_macos() {
    use winit::window::Window;
    // Just confirm the helper is callable and returns without panic.
    let _ = sonic_app::app::with_integrated_titlebar(Window::default_attributes());
}

#[test]
fn backdrop_transparency_tracks_opaque_vs_material_backdrop() {
    use sonic_core::config::BackdropKind;
    use winit::window::Window;

    let opaque = sonic_app::app::with_backdrop_transparency(
        Window::default_attributes(),
        BackdropKind::Opaque,
    );
    let mica = sonic_app::app::with_backdrop_transparency(
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
    use sonic_shared::render::tab_bar_top_inset_with_titlebar;
    // Tab bar visible: titlebar inset stacks above the bar.
    let with = tab_bar_top_inset_with_titlebar(true, 4.0, 28.0);
    let without = tab_bar_top_inset_with_titlebar(true, 4.0, 0.0);
    assert!((with - without - 28.0).abs() < f32::EPSILON);
    // Tab bar hidden: titlebar inset + top padding still reserved.
    let hidden = tab_bar_top_inset_with_titlebar(false, 4.0, 28.0);
    assert!((hidden - 32.0).abs() < f32::EPSILON);
}
