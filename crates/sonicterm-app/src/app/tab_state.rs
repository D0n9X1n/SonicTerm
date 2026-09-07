use std::collections::HashMap;

use sonicterm_types::{OwnerKind, ResourceOwnerId};
use sonicterm_ui::tabs::Tab;
use winit::window::WindowId;

use super::{App, OwnerGuard, PaneState, TabState, TransferError};

/// Refused attachment retaining the live tab and every pane for source restoration.
#[doc(hidden)]
pub struct TabAttachmentError {
    /// The destination or accounting boundary that refused the transfer.
    pub error: TransferError,
    /// Tab that remains owned by the caller.
    pub tab: Tab,
    /// Unchanged pane tree, focus, zoom, and tab-local state.
    pub state: TabState,
    /// Live panes, PTYs, and original charges retained on refusal.
    pub panes: HashMap<u64, PaneState>,
}

impl std::fmt::Debug for TabAttachmentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TabAttachmentError")
            .field("error", &self.error)
            .field("tab", &self.tab.id)
            .finish()
    }
}

impl App {
    /// Detach a main-window tab and return custody of its live pane graph.
    pub fn detach_tab_state(
        &mut self,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        self.detach_from_child(self.main_window_id?, index)
    }

    /// Attach and activate a main-window tab, returning all live state if it cannot be admitted.
    pub fn attach_tab_state(
        &mut self,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) -> Result<(), Box<TabAttachmentError>> {
        let Some(target) = self.main_window_id else {
            // When: main_window_id is absent, return custody instead of dropping the caller's PTYs.
            return Err(Box::new(TabAttachmentError {
                error: TransferError::TargetMissing,
                tab,
                state,
                panes,
            }));
        };
        self.attach_to_window(target, index, tab, state, panes)
    }

    /// Detach one window's tab without closing its panes or changing their attribution.
    pub fn detach_from_child(
        &mut self,
        source: WindowId,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        let window = self.windows.get_mut(&source)?;
        let tab = window.tabs.tabs().get(index)?.clone();
        let state = window.tab_states.get(index)?;
        let leaves = state.tree.leaves();
        if !leaves.iter().all(|id| window.panes.contains_key(id)) {
            // When: leaves names an id absent from window.panes, refuse before detaching a partial tab.
            return None;
        }
        let mut panes = HashMap::with_capacity(leaves.len());
        let state = window.tab_states.remove(index);
        window.tabs.close(tab.id);
        for id in leaves {
            let pane = window.remove_pane(id).expect("validated live leaf");
            panes.insert(id, pane);
        }
        Some((tab, state, panes))
    }

    /// Attach to an existing child while retaining custody on any destination or accounting refusal.
    pub fn attach_to_child(
        &mut self,
        target: WindowId,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) -> Result<(), Box<TabAttachmentError>> {
        self.attach_to_window(target, index, tab, state, panes)
    }

    pub(super) fn validate_transfer_destination(
        &self,
        target: WindowId,
    ) -> Result<(), TransferError> {
        let window = self.windows.get(&target).ok_or(TransferError::TargetMissing)?;
        let test_geometry = window.test_pane_viewport.is_some()
            || (self.main_window_id == Some(target) && self.test_viewport_override.is_some());
        if window.renderer.is_none() && !test_geometry {
            // When: `window.renderer` and explicit test geometry are absent, retain the source instead of inventing a destination size.
            return Err(TransferError::TargetNotReady);
        }
        Ok(())
    }

    pub(super) fn transfer_pane_owners(
        &self,
        panes: &mut HashMap<u64, PaneState>,
        target: Option<ResourceOwnerId>,
    ) -> Result<(), TransferError> {
        let Some(target) = target else {
            // When: the target is explicitly unregistered, only uncharged panes can leave their source hierarchy.
            if panes.values().any(|pane| {
                pane.charges.values().any(|charge| !charge.committed_amount().is_zero())
            }) {
                // When: panes retain a nonzero committed_amount, an unregistered destination cannot preserve their attribution.
                return Err(TransferError::AccountingRefused);
            }
            for pane in panes.values_mut() {
                pane.charges.clear();
                drop(pane.owner.take());
            }
            return Ok(());
        };
        let mut ids: Vec<_> = panes.keys().copied().collect();
        ids.sort_unstable();
        let mut provisional = HashMap::with_capacity(ids.len());
        for id in ids {
            let pane = &panes[&id];
            if pane.owner.as_ref().is_some_and(|owner| {
                self.governor
                    .snapshot(owner.id())
                    .is_ok_and(|snapshot| snapshot.parent == Some(target))
            }) {
                // When: pane.owner already has parent target, retain its guard and charges without replacing their identities.
                continue;
            }
            let owner = self
                .governor
                .create_child(target, OwnerKind::AppPane, super::pane_owner_limits())
                .map_err(|error| {
                    tracing::warn!(
                        ?error,
                        pane_id = id,
                        "tab transfer could not create destination pane owner"
                    );
                    TransferError::AccountingRefused
                })?;
            provisional.insert(id, OwnerGuard::new(self.governor.clone(), owner));
        }
        let transfers = panes
            .iter_mut()
            .filter_map(|(id, pane)| provisional.get(id).map(|owner| (pane, owner.id())))
            .flat_map(|(pane, owner)| pane.charges.values_mut().map(move |charge| (charge, owner)));
        sonicterm_resource::CommittedReservation::transfer_many(transfers).map_err(|error| {
            tracing::warn!(
                ?error,
                "tab transfer charge batch refused; source attribution retained"
            );
            TransferError::AccountingRefused
        })?;
        for (id, owner) in provisional {
            let pane = panes.get_mut(&id).expect("provisional owner has a live pane");
            drop(pane.owner.replace(owner));
        }
        Ok(())
    }

    fn attach_to_window(
        &mut self,
        target: WindowId,
        index: usize,
        tab: Tab,
        state: TabState,
        mut panes: HashMap<u64, PaneState>,
    ) -> Result<(), Box<TabAttachmentError>> {
        let prepare = (|| {
            self.validate_transfer_destination(target)?;
            let leaves = state.tree.leaves();
            if !leaves.contains(&state.active_pane)
                || state.tree.zoomed_pane_id().is_some_and(|id| id != state.active_pane)
                || leaves.len() != panes.len()
                || !leaves.iter().all(|id| panes.contains_key(id))
            {
                // When: active/visible identity or leaf custody disagrees, do not invent a pane or discard live state.
                return Err(TransferError::InvalidTopology);
            }
            let window = self.windows.get_mut(&target).expect("validated destination");
            if window.tabs.tabs().iter().any(|existing| existing.id == tab.id)
                || panes.keys().any(|id| window.panes.contains_key(id))
            {
                // When: window.tabs or window.panes already contains a moved identity, insertion would replace unrelated live state.
                return Err(TransferError::InvalidTopology);
            }
            let index = index.min(window.tabs.len());
            let mut tabs = window.tabs.clone();
            tabs.insert(index, tab.clone());
            window.tab_states.reserve(1);
            window.panes.reserve(panes.len());
            let owner = window.owner.as_ref().map(OwnerGuard::id);
            self.transfer_pane_owners(&mut panes, owner)?;
            Ok((index, tabs))
        })();
        let (index, tabs) = match prepare {
            Ok(prepared) => prepared,
            Err(error) => {
                // When: prepare returns Err(error), every live pane and original charge stays in the returned custody.
                return Err(Box::new(TabAttachmentError { error, tab, state, panes }));
            }
        };
        let window = self.windows.get_mut(&target).expect("synchronous prepared destination");
        for pane in panes.values() {
            *pane.redraw_target.lock() = Some(target);
        }
        window.panes.extend(panes);
        window.tabs = tabs;
        window.tab_states.insert(index, state);
        if self.main_window_id == Some(target) {
            self.resize_visible_panes();
        } else {
            // When: target is not main_window_id, complete geometry against the destination child's own renderer.
            super::child_window::resize_visible_panes_in_child(window);
        }
        Ok(())
    }
}
