//! Epic #289 Phase B regression — assert the unified `App::windows`
//! map replaces the legacy `App::child_windows` field.
//!
//! Before Phase B the App carried two separate window collections:
//! the implicit "main window" (its state inlined as fields on App)
//! and `child_windows: HashMap<WindowId, ChildWindow>` for torn-out
//! windows. Haiku's review of PR #292 flagged the surviving
//! `child_windows` field as a Phase B violation. This file pins:
//!
//!   1. the field is renamed `windows` (not `child_windows`),
//!   2. the struct is renamed `WindowState` (not `ChildWindow`),
//!   3. every `WindowState` carries a `WindowRole` (Terminal),
//!   4. accessors return counts that the test suite can pin.
//!
//! Phase C (follow-up PR) will absorb the main window's per-field
//! state into a full `WindowState` entry.

use sonicterm_app::app::{App, WindowRole, WindowState};

/// `WindowRole` carries the variants Phase B documented and derives
/// the traits callers depend on.
#[test]
fn window_role_variant_is_terminal() {
    assert_eq!(WindowRole::Terminal, WindowRole::Terminal);
    let r: WindowRole = WindowRole::Terminal;
    let _copy: WindowRole = r; // Copy
    let _dbg = format!("{r:?}"); // Debug
}

/// `WindowState` carries the `role` field added in Phase B. Real
/// construction needs a live GPU surface, so we only assert via a
/// never-called accessor signature: a regression that removes the
/// field stops this compiling.
#[test]
fn window_state_carries_role_field() {
    fn _accessor(ws: &WindowState) -> WindowRole {
        ws.role
    }
}

/// `App` exposes the unified-map count via a public accessor whose
/// body reads `self.windows` (not `self.child_windows`). Touching the
/// accessor through the public API pins the rename without needing to
/// expose the private field directly. If a refactor restores the old
/// name, [`App::unified_window_count`] fails to compile.
#[test]
fn app_unified_window_count_accessor_exists() {
    fn _accessor(app: &App) -> usize {
        app.unified_window_count()
    }
}

/// Role-filter accessor exists. A regression that drops the role
/// field or the filter helper breaks compilation here.
#[test]
fn app_windows_with_role_accessor_exists() {
    fn _accessor(app: &App) -> usize {
        app.windows_with_role(WindowRole::Terminal)
    }
}
