//! Foreground-process privilege warnings for tabs, sampled through a bounded cache.

use super::*;

fn pane_foreground_cache_is_fresh(pane: &PaneState, now: Instant) -> bool {
    pane.fg_proc_cache
        .as_ref()
        .is_some_and(|(sampled, _)| now.duration_since(*sampled) < FOREGROUND_PROCESS_TTL)
}

fn cached_foreground_privileged(pane: &PaneState) -> bool {
    pane.fg_proc_cache
        .as_ref()
        .and_then(|(_, process)| process.as_ref())
        .is_some_and(|process| process.privileged)
}

pub(super) fn refresh_tab_foreground_privilege(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    pane: &mut PaneState,
    tab_idx: usize,
    allow_proc_probe: bool,
) {
    let now = Instant::now();
    if !pane_foreground_cache_is_fresh(pane, now) && allow_proc_probe {
        let probed = pane.pty.as_ref().and_then(|pty| pty.pid()).and_then(|pid| {
            crate::app::frame_counters::time_probe(1, || {
                sonicterm_io::proc_info::foreground_process_info(pid)
            })
        });
        pane.fg_proc_cache = Some((now, probed));
    }
    tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
}

fn refresh_window_tab_privileges_at(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    allow_proc_probe: bool,
    force_proc_probe: bool,
    now: Instant,
) -> bool {
    #[cfg(windows)]
    {
        let mut changed = false;
        let mut stale = Vec::new();
        for (tab_idx, tab_state) in tab_states.iter().enumerate() {
            let Some(pane) = panes.get_mut(&tab_state.active_pane) else {
                // When: the tab's active pane no longer exists, clear any warning retained by its tab.
                changed |= tabs.set_foreground_privileged(tab_idx, false);
                continue;
            };
            if allow_proc_probe && (force_proc_probe || !pane_foreground_cache_is_fresh(pane, now))
            {
                // When: this pane's cache is stale or a deadline forces a sample, include it in the shared snapshot.
                if let Some(pid) = pane.pty.as_ref().and_then(|pty| pty.pid()) {
                    stale.push((tab_idx, tab_state.active_pane, pid));
                } else {
                    changed |=
                        pane.fg_proc_cache.as_ref().is_none_or(|(_, process)| process.is_some());
                    pane.fg_proc_cache = Some((now, None));
                }
            }
            changed |= tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
        }

        let pids = stale.iter().map(|(_, _, pid)| *pid).collect::<Vec<_>>();
        let observations = crate::app::frame_counters::time_probe(pids.len(), || {
            sonicterm_io::proc_info::foreground_processes_info(&pids)
        });
        for ((tab_idx, pane_id, _), observation) in stale.into_iter().zip(observations) {
            let Some(pane) = panes.get_mut(&pane_id) else {
                // When: the pane vanished after collection, its tab must not retain the old warning.
                changed |= tabs.set_foreground_privileged(tab_idx, false);
                continue;
            };
            changed |=
                pane.fg_proc_cache.as_ref().is_none_or(|(_, process)| process != &observation);
            pane.fg_proc_cache = Some((now, observation));
            changed |= tabs.set_foreground_privileged(tab_idx, cached_foreground_privileged(pane));
        }
        changed
    }

    #[cfg(not(windows))]
    {
        let _ = (tabs, tab_states, panes, allow_proc_probe, force_proc_probe, now);
        false
    }
}

pub(super) fn refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    allow_proc_probe: bool,
) -> bool {
    refresh_window_tab_privileges_at(
        tabs,
        tab_states,
        panes,
        allow_proc_probe,
        false,
        Instant::now(),
    )
}

#[cfg(windows)]
pub(super) fn force_refresh_window_tab_privileges(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    tab_states: &[TabState],
    panes: &mut HashMap<u64, PaneState>,
    now: Instant,
) -> bool {
    refresh_window_tab_privileges_at(tabs, tab_states, panes, true, true, now)
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
