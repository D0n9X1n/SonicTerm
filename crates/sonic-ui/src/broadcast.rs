//! Broadcast-input state and pure pane-set selection helpers.

use std::collections::BTreeSet;

use sonic_types::BroadcastScope;

use crate::pane::{PaneId, PaneTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BroadcastState {
    #[default]
    Off,
    On {
        scope: BroadcastScope,
        source_pane: PaneId,
    },
}

impl BroadcastState {
    #[must_use]
    pub fn toggled(self, scope: BroadcastScope, source_pane: PaneId) -> Self {
        match self {
            Self::On { scope: active_scope, source_pane: active_source }
                if active_scope == scope && active_source == source_pane =>
            {
                Self::Off
            }
            _ => Self::On { scope, source_pane },
        }
    }

    #[must_use]
    pub fn receiving_panes<T>(self, tabs: &[T], active_tab_idx: usize) -> BTreeSet<PaneId>
    where
        T: BroadcastTab,
    {
        let Self::On { scope, source_pane } = self else {
            return BTreeSet::new();
        };
        receiving_panes(tabs, scope, source_pane, active_tab_idx)
    }
}

pub trait BroadcastTab {
    fn pane_tree(&self) -> &PaneTree;
}

impl BroadcastTab for PaneTree {
    fn pane_tree(&self) -> &PaneTree {
        self
    }
}

#[must_use]
pub fn receiving_panes<T>(
    tabs: &[T],
    scope: BroadcastScope,
    source_pane: PaneId,
    active_tab_idx: usize,
) -> BTreeSet<PaneId>
where
    T: BroadcastTab,
{
    let mut out = BTreeSet::new();
    let iter: Box<dyn Iterator<Item = &T> + '_> = match scope {
        BroadcastScope::Tab => Box::new(tabs.get(active_tab_idx).into_iter()),
        BroadcastScope::AllTabs => Box::new(tabs.iter()),
    };
    for tab in iter {
        for pane in tab.pane_tree().leaves() {
            if pane != source_pane {
                out.insert(pane);
            }
        }
    }
    out
}
