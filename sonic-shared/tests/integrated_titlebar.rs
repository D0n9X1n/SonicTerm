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
    let out = sonic_shared::app::with_integrated_titlebar(base);
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
    let _ = sonic_shared::app::with_integrated_titlebar(Window::default_attributes());
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

#[cfg(target_os = "macos")]
#[test]
fn integrated_titlebar_inset_macos_reserves_at_least_22_logical_px() {
    // Regression for the PR-#83 overlap bug: the macOS integrated
    // titlebar style extends our content under the traffic lights, so
    // we must reserve a band ≥ 22pt (the minimum a standard NSWindow
    // titlebar consumes; AppKit's default is 28pt).
    let inset = sonic_shared::app::integrated_titlebar_inset();
    assert!(
        inset >= 22.0,
        "macOS integrated titlebar inset must reserve >=22 logical px (got {inset})"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn integrated_titlebar_inset_is_zero_off_macos() {
    assert_eq!(sonic_shared::app::integrated_titlebar_inset(), 0.0);
}
