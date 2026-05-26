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
