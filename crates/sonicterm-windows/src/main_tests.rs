//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.
//!
//! This file used to carry a counting-allocator measurement of `SoftwareSurface`,
//! the retained BGRA surface that used to live in `software_presenter.rs`. That
//! type is gone: it duplicated `sonicterm_gpu::software_windows::WindowsSoftwareFrame`,
//! which is the presenter a degraded renderer actually constructs, and nothing
//! ever constructed the copy.
//!
//! The measurement is not relocated. `WindowsSoftwareFrame` is `pub(crate)` and
//! its module is `#![cfg(target_os = "windows")]`, so the same test in
//! `sonicterm-gpu` would run on Windows only — and a `#[global_allocator]`
//! there would wrap every allocation in that crate's 77 other unit tests to
//! serve one measurement.
//!
//! What it established survives where it is load-bearing: the `SoftwareFrame`
//! bound in the resource table is `MAX_SURFACE_BYTES`, a constant the live path
//! enforces through `validated_surface_size`, tied by test rather than copied.
//! What is lost is the empirical per-window table — 1080p through 8K measured
//! against real heap. Those figures were taken against the deleted type, and
//! the live type computes its size the same way, but that is now an argument
//! rather than a measurement.

#[test]
fn integration_test_target_is_present() {
    assert_eq!(env!("CARGO_PKG_NAME"), "sonicterm-windows");
}

#[test]
fn software_presenter_tests_are_declared_by_their_source_module() {
    const SOURCE: &str = include_str!("software_presenter.rs");

    assert!(SOURCE.contains("#[path = \"software_presenter_tests.rs\"]"));
    assert!(SOURCE.contains("mod software_presenter_tests;"));
}

#[test]
fn ole_shutdown_is_owned_by_same_thread_guard() {
    const MAIN: &str = include_str!("main.rs");
    const OLE: &str = include_str!("os_drag_win.rs");

    assert!(MAIN.lines().any(|line| line.trim() == "let ole_guard = os_drag_win::init_ole();"));
    assert!(MAIN.contains("let ole_available = ole_guard.is_some();"));
    assert!(MAIN.contains("if ole_available"));
    assert!(MAIN.contains("OLE unavailable; native tab drag/drop is disabled"));
    assert!(
        OLE.contains("pub struct OleGuard(std::marker::PhantomData<*const ()>)"),
        "OleGuard must remain !Send so OleUninitialize runs on the OleInitialize thread"
    );
    assert!(OLE.contains("pub fn init_ole() -> Option<OleGuard>"));
    assert!(OLE.contains("impl Drop for OleGuard"));
    assert!(!MAIN.contains("shutdown_ole"));
}

#[test]
fn raw_hwnd_and_ole_entry_points_are_explicitly_unsafe() {
    const MAIN: &str = include_str!("main.rs");
    const OLE: &str = include_str!("os_drag_win.rs");
    const BACKDROP: &str = include_str!("backdrop.rs");
    const MENU: &str = include_str!("menubar.rs");
    const TAB_DRAG: &str = include_str!("tab_drag_os.rs");

    assert!(OLE
        .lines()
        .any(|line| line.trim_start().starts_with("pub(crate) unsafe fn begin_tab_drag")));
    assert!(OLE
        .lines()
        .any(|line| line.trim_start().starts_with("pub unsafe fn register_for_window")));
    assert!(BACKDROP
        .lines()
        .any(|line| line.trim_start().starts_with("pub unsafe fn apply_backdrop")));
    assert!(MENU.lines().any(|line| line.trim_start().starts_with("pub unsafe fn new")));
    assert!(TAB_DRAG.lines().any(|line| line.trim_start().starts_with("pub unsafe fn boxed")));
    assert!(!TAB_DRAG.contains("pub fn new() -> Self"));
    assert!(!TAB_DRAG.contains("impl Default for WinOsTabDragBackend"));
    assert!(MAIN.contains("if ole_available"));
}

#[test]
fn ole_drag_paths_reject_missing_initialization_and_empty_payloads() {
    const OLE: &str = include_str!("os_drag_win.rs");
    const TAB_DRAG: &str = include_str!("tab_drag_os.rs");

    assert!(OLE.contains("if !ole_initialized_on_current_thread()"));
    assert!(OLE.contains("if payload_json.is_empty()"));
    assert!(OLE.contains("if len == 0"));
    assert!(TAB_DRAG.contains("let outcome ="));
    assert!(TAB_DRAG.contains("if outcome.hr != windows::Win32::Foundation::DRAGDROP_S_DROP"));
    assert!(TAB_DRAG.contains("DragOutcome::Cancelled"));
    assert!(!TAB_DRAG.contains("let effect ="));
    assert!(TAB_DRAG.contains("if registered"));
}

/// No caller may answer the detection question with a literal.
///
/// `should_use(false)` in `main.rs` made the branch unreachable under `Auto`,
/// the default mode — the answer was fixed before the adapter was consulted.
/// The argument must come from runtime detection or the call is decorative.
///
/// Lives here rather than beside the presenter tests: this reads `main.rs` as
/// text and needs nothing platform-specific, and `software_presenter` is now
/// `#[cfg(target_os = "windows")]` — so a guard placed there would stop running
/// on macOS, which is where most of this repository's development happens.
///
/// A source scan, because this is a property of the *call site* rather than of
/// any value: a behavioural test cannot tell `should_use(false)` from
/// `should_use(detected)` on a host where detection is false.
///
/// Scoped to literals only. It will not catch a caller that passes a variable
/// which is always false, and it is not meant to — that is a different defect
/// with a different remedy.
#[test]
fn no_caller_hardcodes_the_detection_argument() {
    const MAIN: &str = include_str!("main.rs");

    for (index, line) in MAIN.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        assert!(
            !trimmed.contains("should_use(false)") && !trimmed.contains("should_use(true)"),
            "main.rs:{} answers the software-detection question with a literal: {}\n\
             The argument must come from runtime adapter detection. A literal makes the \
             call decorative — `Auto` follows detection, so a hardcoded value fixes the \
             answer before the adapter is consulted.",
            index + 1,
            trimmed
        );
    }
}
