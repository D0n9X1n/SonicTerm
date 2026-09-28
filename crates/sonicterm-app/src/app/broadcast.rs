//! Broadcast input: fan-out from the source pane and its receiver and participant sets.

use super::*;

impl App {
    pub(super) fn broadcast_from(
        &mut self,
        active_id: u64,
        bytes: Vec<u8>,
        source: PtyInputSource,
    ) {
        let BroadcastState::On { source_pane, .. } = self.broadcast else {
            // When: `broadcast` is not `On`, so there is no fan-out group and the
            // bytes belong to the focused pane alone.
            return;
        };
        if active_id != source_pane {
            // When: `active_id` is not the `source_pane` that armed the
            // broadcast, so typing here must not fan out to the group.
            return;
        }
        let receivers = self.broadcast_receivers();
        for pane_id in receivers {
            self.write_to_pane(pane_id, bytes.clone(), source);
        }
    }

    pub(crate) fn broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        let BroadcastState::On { scope, source_pane } = self.broadcast else {
            // When: `broadcast` is not `On`, so no `scope` or `source_pane`
            // defines a group and the receiver set is empty.
            return Default::default();
        };
        self.broadcast_receivers_for(scope, source_pane)
    }

    /// Return render-only participants, including the live source; never use this set for PTY fan-out.
    pub(crate) fn broadcast_participants(&self) -> std::collections::BTreeSet<u64> {
        let BroadcastState::On { source_pane, .. } = self.broadcast else {
            // When: broadcast is Off, no pane needs safety chrome.
            return Default::default();
        };
        if self.pane_by_id(source_pane).is_none() {
            // When: source_pane is gone, mirrored input is inert even if other tabs survive.
            return Default::default();
        }
        let mut participants = self.broadcast_receivers();
        participants.insert(source_pane);
        participants
    }

    pub(super) fn clear_closed_broadcast_source(&mut self) {
        if let BroadcastState::On { source_pane, .. } = self.broadcast {
            if self.pane_by_id(source_pane).is_none() {
                // A closed source needs its safety chrome erased in every surviving window before sleeping.
                self.broadcast = BroadcastState::Off;
                self.request_redraw_all_terminal_windows();
            }
        }
    }

    fn broadcast_receivers_for(
        &self,
        scope: BroadcastScope,
        source_pane: u64,
    ) -> std::collections::BTreeSet<u64> {
        let mut receivers = std::collections::BTreeSet::new();
        for ws in self.windows.values() {
            match scope {
                BroadcastScope::Tab => {
                    // When: `scope` is `Tab`, so only panes sharing the source's
                    // own tab receive the fan-out.
                    if let Some((tab_idx, _)) = ws
                        .tab_states
                        .iter()
                        .enumerate()
                        .find(|(_, tab)| tab.tree.leaves().contains(&source_pane))
                    {
                        // When: a tab's `leaves` hold `source_pane`, so that tab's
                        // panes are the receiver set for this window.
                        receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                            &ws.tab_states,
                            scope,
                            source_pane,
                            tab_idx,
                        ));
                        break;
                    }
                }
                BroadcastScope::AllTabs => {
                    receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                        &ws.tab_states,
                        scope,
                        source_pane,
                        ws.tabs.active_index(),
                    ));
                }
            }
        }
        receivers.retain(|pane_id| self.admits_new_user_input(*pane_id));
        receivers
    }
}
