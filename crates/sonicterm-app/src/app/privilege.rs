//! Foreground-process privilege warnings for tabs, read from each pane's cached sample.
//!
//! Frames and the Windows timer only set demand on the App's foreground probes; the
//! worker samples off the event-loop thread and `App::drain_foreground_probe_results`
//! applies its results. Nothing here walks the process table.

use super::*;

/// Whether the pane's cached foreground process requires a privilege warning.
pub(super) fn cached_foreground_privileged(pane: &PaneState) -> bool {
    pane.fg_proc_cache
        .as_ref()
        .and_then(|(_, process)| process.as_ref())
        .is_some_and(|process| process.privileged)
}

/// Set frame demand for one tab's active pane and wake the worker if the pane now wants a sample.
pub(super) fn demand_frame_foreground(
    probes: &fg_probe::ForegroundProbes,
    pane_id: u64,
    pane: &mut PaneState,
    now: Instant,
) {
    if probes.request(pane_id, pane, fg_probe::WantOrigin::Frame, now, false)
        == fg_probe::Demand::Queued
    {
        probes.wake();
    }
}

/// Set frame demand for every tab's active pane and show each tab's cached warning.
///
/// Windows shows per-tab warnings for background tabs too; `probes` is `None` on a burst
/// frame, which reads the cache only. Returns whether any warning changed.
#[cfg(windows)]
pub(super) fn refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    probes: Option<&fg_probe::ForegroundProbes>,
    now: Instant,
) -> bool {
    let mut changed = false;
    let mut queued = false;
    for (tab_idx, tab_state) in tab_states.iter().enumerate() {
        let Some(pane) = panes.get_mut(&tab_state.active_pane) else {
            // When: the tab's active pane no longer exists, clear any warning retained by its tab.
            changed |= tabs.set_foreground_privileged(tab_idx, false);
            continue;
        };
        if let Some(probes) = probes {
            queued |= probes.request(
                tab_state.active_pane,
                pane,
                fg_probe::WantOrigin::Frame,
                now,
                false,
            ) == fg_probe::Demand::Queued;
        }
        changed |= tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
    }
    if queued {
        if let Some(probes) = probes {
            probes.wake();
        }
    }
    changed
}

/// Background tabs carry no per-tab warning off Windows; the active tab is handled by the title refresh.
#[cfg(not(windows))]
pub(super) fn refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    probes: Option<&fg_probe::ForegroundProbes>,
    now: Instant,
) -> bool {
    let _ = (tabs, tab_states, panes, probes, now);
    false
}

impl App {
    /// Return the privilege snapshot supplied by the native startup boundary.
    #[must_use]
    pub const fn process_privilege(&self) -> crate::ProcessPrivilege {
        self.process_privilege
    }

    /// Install the native process-privilege snapshot before window creation.
    pub(crate) fn set_process_privilege(&mut self, privilege: crate::ProcessPrivilege) {
        self.process_privilege = privilege;
    }
}

#[cfg(test)]
#[path = "privilege_tests.rs"]
mod privilege_tests;
