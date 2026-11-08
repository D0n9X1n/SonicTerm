//! The App's test-only seams in one owner: clipboard overrides, the PTY write ledger and its gate,
//! the viewport override, and the observations unit tests read. Each field keeps its own cfg gate,
//! so a release build carries only those production code or the perf harness reads.

use std::sync::Arc;

use parking_lot::Mutex;

#[allow(unused_imports)]
use super::*;

/// Test-only App state, grouped so the `test-support` audit can see it in one place.
#[derive(Default)]
#[doc(hidden)]
pub struct TestSupportState {
    /// Test-only in-memory clipboard override for integration tests that need
    /// to observe copy/paste routing without depending on a desktop clipboard
    /// service. `None` means production arboard behavior; `Some(_)` means reads
    /// and writes use this buffer instead.
    #[doc(hidden)]
    pub(in crate::app) test_clipboard_text: Option<String>,
    /// Test-only copied-file list standing in for the native clipboard's file
    /// list. Installing either test override keeps every read off the desktop
    /// clipboard; `None` here then means no files are on the clipboard.
    #[doc(hidden)]
    pub(in crate::app) test_clipboard_files: Option<Vec<std::path::PathBuf>>,
    /// Test-only clipboard write rejection injected at the production write
    /// boundary. Disabled in every constructor so ordinary runs retain the real
    /// clipboard behavior and the in-memory success seam remains opt-in.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) test_clipboard_write_failure: bool,
    /// Test-only PTY write ledger. `write_to_pane` records every boundary write
    /// here before resolving the pane to a real PTY, so headless tests can assert
    /// which pane an action targeted without constructing a process-backed PTY.
    #[doc(hidden)]
    pub(in crate::app) test_pty_writes: Arc<Mutex<Vec<(u64, Vec<u8>)>>>,
    /// Unit-test observation of actual PTY spawn inputs; absent from production builds.
    #[cfg(test)]
    pub(in crate::app) test_pane_launches: std::cell::RefCell<Vec<(u64, pane_launch::PaneLaunch)>>,
    /// Unit-test record of each logged PTY write's pane and producer source, so tests can check attribution.
    #[cfg(test)]
    pub(in crate::app) test_pty_write_sources: std::cell::RefCell<Vec<(u64, PtyInputSource)>>,
    /// Whether the PTY write ledger above is actually recorded. `false` in
    /// production so `dispatch_pty_write_effect` does no lock/clone/push per
    /// write (the ledger would otherwise grow unbounded for the whole
    /// session —). Set `true` when the app is built without an
    /// event-loop proxy (headless/test construction) so existing tests keep
    /// capturing writes with no per-test opt-in.
    #[doc(hidden)]
    pub(in crate::app) pty_write_log_enabled: bool,
    /// Test-only callback fired
    /// inside [`Self::cancel_drag_session`] AFTER the `self.registry.windows.keys()`
    /// snapshot is collected but BEFORE the per-id iteration body runs.
    /// Lets a regression test
    /// mutate `self.registry.windows` in the exact race window that the
    /// `get_mut(&id).else { continue }` arm is designed to tolerate.
    /// Consumed (`take()`-d) at the call site so the closure is invoked
    /// at most once per `cancel_drag_session` run and the mutable
    /// borrow on `self.registry.windows` is not held while it runs. Compiled only
    /// under `cfg(test)` or `test-support`, so release builds carry neither the
    /// field nor the call; an integration test installs it through
    /// `App::__test_set_post_snapshot_hook`.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Unit-test observation of every scheduler trim decision, in pass order.
    #[cfg(test)]
    pub(in crate::app) test_scheduler_trims: Vec<(WindowId, retention::TrimDecision)>,
    /// Test-only viewport override. When
    /// `Some((outer, cell_w, cell_h))`, [`App::compute_active_pane_rects`]
    /// uses `outer` instead of fetching the renderer's logical size and
    /// [`App::resize_visible_panes`] uses `(cell_w, cell_h)` instead of
    /// the renderer's `cell_size()`. Lets tests exercise the production
    /// `close_active_pane` path (Grid + PtyHandle resize wiring) without
    /// a live wgpu surface. Stays `None` in release builds whose tests
    /// don't touch it.
    #[doc(hidden)]
    pub test_viewport_override: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
}
