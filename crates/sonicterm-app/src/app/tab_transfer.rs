//! Epic #289 Phase C — cross-window tab drag & drop.
//!
//! Implements the *pure* tab-transfer primitive that moves a `Tab`
//! (with its `TabState` and the full `PaneState` graph it owns)
//! between two `TabContainer`s. The OS-level NSDraggingSession / OLE
//! integration that *invokes* this primitive ships in a follow-up
//! commit on this PR (see PR body: "Phase C ships the simulated
//! primitive + tests; OS-drag hookup deferred to a follow-up").
//!
//! Why a separate "container" struct rather than just operating on
//! `WindowState` / `App` directly: the canonical drop-target windows
//! (`WindowState`) require a live `Arc<Window>` + `GpuRenderer` — both
//! impossible to construct in a unit test without a real wgpu surface.
//! The transfer logic is also pure data-shuffling — no IO, no GPU,
//! no PTY resize is *required* for correctness — so the right shape
//! is a value type the integration tests can build at will.
//!
//! The App-level wrapper (`App::transfer_tab`) lives in `app/mod.rs`
//! and dispatches to four real-window flavors (main↔main reorder,
//! main→child, child→main, child→child) by delegating to the existing
//! `detach_tab_state` / `attach_tab_state` / `detach_from_child` /
//! `attach_to_child` helpers. All five flavors are exercised by the
//! regression tests at the pure-container level.

use std::collections::HashMap;

use sonicterm_ui::tabs::{Tab, TabBar};

use super::{PaneState, TabState};

/// A self-contained, GPU-free analogue of `WindowState` exposing only
/// the fields the tab-transfer primitive touches. Used by:
///
/// * the pure transfer function below,
/// * regression tests in `tests/cross_window_tab_transfer.rs`.
///
/// Production code uses `WindowState` directly; this type exists
/// purely so the pure logic can be tested in isolation.
#[doc(hidden)]
pub struct TabContainer {
    pub tabs: TabBar,
    pub tab_states: Vec<TabState>,
    pub panes: HashMap<u64, PaneState>,
}

impl Default for TabContainer {
    fn default() -> Self {
        Self::new()
    }
}

impl TabContainer {
    pub fn new() -> Self {
        Self { tabs: TabBar::new(), tab_states: Vec::new(), panes: HashMap::new() }
    }

    /// Push a tab + its single-leaf pane into this container. Returns
    /// the synthesized `pane_id` so the test can assert "this exact
    /// PaneState moved to the other container, not a clone".
    pub fn push_tab(&mut self, title: &str, pane: PaneState) -> u64 {
        use sonicterm_ui::pane::PaneTree;
        let pane_id = super::next_pane_id();
        self.panes.insert(pane_id, pane);
        self.tabs.push(Tab::new(title));
        self.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        pane_id
    }
}

/// Outcome of a transfer attempt.
#[derive(Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransferOutcome {
    /// Transfer succeeded; first field is the target's new active tab
    /// index, second is `true` when the *source* container is now empty
    /// (caller's signal to close the source window).
    Moved { target_active: usize, source_empty: bool },
    /// Source index out of range; no state mutated.
    SourceIndexOutOfRange,
    /// Same-container reorder where `src_idx == dst_idx` or the
    /// computed final position would equal the original; treated as a
    /// no-op so the test suite can rely on idempotence.
    NoOp,
}

/// Move `src.tabs[src_idx]` (with its `TabState` + the full `PaneState`
/// subtree it owns) into `dst.tabs` at `dst_idx`. Activates the moved
/// tab in `dst`. The two containers MUST be different instances —
/// for same-container reorder use [`reorder_within`]. Returns a
/// `TransferOutcome` documenting the result; *no panic on out-of-range
/// input* — defensive because the real call site receives slot indices
/// from a cursor position that can race with concurrent tab closes.
///
/// ## Caller responsibility
///
/// If `outcome.source_empty == true`, the caller MUST close the source
/// window (production: `App::close_window`). The pure primitive does
/// not touch windows — only tab vectors and pane maps.
#[doc(hidden)]
pub fn transfer_tab_between(
    src: &mut TabContainer,
    src_idx: usize,
    dst: &mut TabContainer,
    dst_idx: usize,
) -> TransferOutcome {
    if src_idx >= src.tabs.len() || src_idx >= src.tab_states.len() {
        return TransferOutcome::SourceIndexOutOfRange;
    }

    // Detach
    let tab = src.tabs.tabs()[src_idx].clone();
    let state = src.tab_states.remove(src_idx);
    let mut moved_panes: HashMap<u64, PaneState> = HashMap::new();
    for leaf_id in state.tree.leaves() {
        if let Some(p) = src.panes.remove(&leaf_id) {
            moved_panes.insert(leaf_id, p);
        }
    }
    let tab_id = tab.id;
    src.tabs.close(tab_id);

    // Attach
    let insert_at = dst_idx.min(dst.tabs.len());
    for (id, pane) in moved_panes {
        dst.panes.insert(id, pane);
    }
    dst.tabs.insert(insert_at, tab);
    dst.tab_states.insert(insert_at, state);
    dst.tabs.activate(insert_at);

    TransferOutcome::Moved { target_active: insert_at, source_empty: src.tabs.is_empty() }
}

/// Reorder a tab within a single container. The destination index is
/// interpreted as the slot the moved tab should occupy in the FINAL
/// arrangement, matching the spec's `transfer_tab(A, 0, A, 2)` →
/// `[orig[1], orig[2], orig[0]]` example. Same-window analogue of
/// [`transfer_tab_between`]; separate function because Rust's borrow
/// checker rightly forbids two `&mut` to the same value.
#[doc(hidden)]
pub fn reorder_within(c: &mut TabContainer, src_idx: usize, dst_idx: usize) -> TransferOutcome {
    if src_idx >= c.tabs.len() || src_idx >= c.tab_states.len() {
        return TransferOutcome::SourceIndexOutOfRange;
    }
    let last = c.tabs.len().saturating_sub(1);
    let to = dst_idx.min(last);
    if to == src_idx {
        return TransferOutcome::NoOp;
    }
    c.tabs.reorder(src_idx, to);
    let state = c.tab_states.remove(src_idx);
    c.tab_states.insert(to, state);
    c.tabs.activate(to);
    TransferOutcome::Moved { target_active: to, source_empty: false }
}
