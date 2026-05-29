//! Regression for Haiku follow-up on PR #323: the tear-out window
//! spawn path and the `Action::NewWindow` (Cmd+N) handler both used
//! the Top-only macOS titlebar helpers (`with_integrated_titlebar`
//! and `integrated_titlebar_inset`). On a Bottom-config user that
//! produced a window with the integrated titlebar and a 28pt top
//! inset even though the tab bar belongs at the bottom and there is
//! no top chrome to merge with.
//!
//! These tests pin the position-aware helpers so the call sites can
//! never silently regress to the Top-only variants again. We can't
//! drive a full `App` headlessly (no display, no winit event loop),
//! so we exercise the exact helpers the call sites invoke and assert
//! they branch on `tab_bar_position` the way the fix requires.
//!
//! Coverage:
//!   tear-out spawn (main → new window): `tear_out_tab`;
//!   tear-out spawn (child → new window): `tear_out_from_child`;
//!   `Action::NewWindow` (Cmd+N): `create_new_terminal_window`.
//!
//! All three call sites now route through `with_integrated_titlebar_for(pos)`
//! and `integrated_titlebar_inset_for(pos)`. The asserts below pin
//! the position-conditional behavior of those helpers.

use sonic_app::app::{integrated_titlebar_inset_for, with_integrated_titlebar_for};
use sonic_core::config::TabBarPosition;
use winit::window::Window;

#[cfg(target_os = "macos")]
#[test]
fn bottom_config_tearout_window_has_zero_top_inset() {
    // Bottom-config: tear-out path must not reserve a 28pt top band.
    let inset = integrated_titlebar_inset_for(TabBarPosition::Bottom);
    assert_eq!(inset, 0.0, "Bottom tab-bar windows must have 0 top inset (got {inset})");
}

#[cfg(target_os = "macos")]
#[test]
fn top_config_tearout_window_keeps_28pt_top_inset() {
    let inset = integrated_titlebar_inset_for(TabBarPosition::Top);
    assert!(
        inset >= 22.0,
        "Top tab-bar windows must keep an integrated-titlebar reservation (got {inset})"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn bottom_config_tearout_window_uses_standard_nswindow_titlebar() {
    // Probe via Debug repr: bottom path must NOT flip
    // fullsize_content_view / titlebar_transparent to true, because
    // the OS draws its own title bar above our content area.
    let base = Window::default_attributes().with_title("probe");
    let bottom = with_integrated_titlebar_for(base, TabBarPosition::Bottom);
    let dbg = format!("{bottom:?}");
    assert!(
        dbg.contains("fullsize_content_view: false"),
        "Bottom tab-bar windows must keep the standard NSWindow titlebar.\n{dbg}"
    );
    assert!(
        dbg.contains("titlebar_transparent: false"),
        "Bottom tab-bar windows must keep an opaque OS titlebar.\n{dbg}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn top_config_tearout_window_uses_integrated_titlebar() {
    let base = Window::default_attributes().with_title("probe");
    let top = with_integrated_titlebar_for(base, TabBarPosition::Top);
    let dbg = format!("{top:?}");
    assert!(
        dbg.contains("fullsize_content_view: true"),
        "Top tab-bar windows must enable fullsize_content_view.\n{dbg}"
    );
    assert!(
        dbg.contains("titlebar_transparent: true"),
        "Top tab-bar windows must make the titlebar transparent.\n{dbg}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn position_aware_helpers_are_noop_off_macos() {
    // On non-macOS the inset is always 0 and the attrs helper is a
    // no-op regardless of position.
    assert_eq!(integrated_titlebar_inset_for(TabBarPosition::Top), 0.0);
    assert_eq!(integrated_titlebar_inset_for(TabBarPosition::Bottom), 0.0);
    let _ = with_integrated_titlebar_for(Window::default_attributes(), TabBarPosition::Top);
    let _ = with_integrated_titlebar_for(Window::default_attributes(), TabBarPosition::Bottom);
}

/// Source-level pin: the three call sites this PR fixed must continue
/// to invoke the position-aware helpers (never the Top-only ones).
/// If a future refactor accidentally reverts any site to
/// `with_integrated_titlebar(` or `integrated_titlebar_inset()`, this
/// test fails immediately — well before the bug ships to users.
#[test]
fn tearout_and_newwindow_callsites_use_position_aware_helpers() {
    // Locate the workspace root by walking up from CARGO_MANIFEST_DIR
    // (= crates/sonic-app) two levels.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().and_then(|p| p.parent()).expect("workspace root");
    let files = [
        workspace.join("crates/sonic-app/src/app/tear_out.rs"),
        workspace.join("crates/sonic-app/src/app/misc.rs"),
    ];
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        // Strip use-statement imports so we only check call sites.
        // The substrings we forbid include a trailing `(` — the
        // position-aware variants are `_for(...)`, so they don't
        // match `with_integrated_titlebar(` literally.
        for forbidden in ["with_integrated_titlebar(", "integrated_titlebar_inset()"] {
            for (lineno, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                // Allow `use` lines (imports). The literal Top-only
                // helpers are intentionally re-exported from
                // `sonic_app::app` for back-compat with the older
                // `integrated_titlebar.rs` test; we only forbid
                // *call sites*, not the export.
                if trimmed.starts_with("use ") {
                    continue;
                }
                assert!(
                    !line.contains(forbidden),
                    "{}:{} call site uses Top-only helper `{}` — must use the *_for(pos) variant.\n{}",
                    path.display(),
                    lineno + 1,
                    forbidden,
                    line
                );
            }
        }
    }
}
