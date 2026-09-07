use super::*;
use sonicterm_cfg::keymap::Direction;

fn tabs() -> [PaneTree; 2] {
    let mut first = PaneTree::leaf(1);
    assert!(first.split(1, Direction::Right, 2));
    let mut second = PaneTree::leaf(3);
    assert!(second.split(3, Direction::Down, 4));
    [first, second]
}

/// Repeating the same source and scope disables broadcast; any other toggle replaces it.
#[test]
fn toggle_is_identity_sensitive() {
    let state = BroadcastState::Off.toggled(BroadcastScope::Tab, 1);
    assert_eq!(state.toggled(BroadcastScope::Tab, 1), BroadcastState::Off);
    assert_eq!(
        state.toggled(BroadcastScope::AllTabs, 1),
        BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: 1 }
    );
    assert_eq!(
        state.toggled(BroadcastScope::Tab, 2),
        BroadcastState::On { scope: BroadcastScope::Tab, source_pane: 2 }
    );
}

/// Tab scope selects only the active tab and never returns the source pane.
#[test]
fn tab_scope_excludes_source_and_other_tabs() {
    let state = BroadcastState::On { scope: BroadcastScope::Tab, source_pane: 1 };
    assert_eq!(state.receiving_panes(&tabs(), 0), BTreeSet::from([2]));
}

/// All-tabs scope spans every tree while still excluding the source pane.
#[test]
fn all_tabs_scope_collects_every_other_leaf() {
    let destinations = receiving_panes(&tabs(), BroadcastScope::AllTabs, 3, 0);
    assert_eq!(destinations, BTreeSet::from([1, 2, 4]));
}

/// Off state and an out-of-range active tab produce no destinations.
#[test]
fn inactive_or_missing_tab_has_no_receivers() {
    assert!(BroadcastState::Off.receiving_panes(&tabs(), 0).is_empty());
    assert!(receiving_panes(&tabs(), BroadcastScope::Tab, 1, 9).is_empty());
}
