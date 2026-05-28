use sonic_cfg::keymap::Direction;
use sonic_types::BroadcastScope;
use sonic_ui::broadcast::BroadcastState;
use sonic_ui::pane::PaneTree;

#[test]
fn toggle_off_on_same_scope_and_source_turns_off() {
    let state = BroadcastState::Off.toggled(BroadcastScope::Tab, 7);
    assert_eq!(state, BroadcastState::On { scope: BroadcastScope::Tab, source_pane: 7 });
    assert_eq!(state.toggled(BroadcastScope::Tab, 7), BroadcastState::Off);
}

#[test]
fn receiving_panes_excludes_source_and_respects_scope() {
    let mut tab0 = PaneTree::leaf(1);
    assert!(tab0.split(1, Direction::Right, 2));
    let mut tab1 = PaneTree::leaf(3);
    assert!(tab1.split(3, Direction::Down, 4));
    let tabs = vec![tab0, tab1];

    let tab_receivers: Vec<_> = BroadcastState::On { scope: BroadcastScope::Tab, source_pane: 1 }
        .receiving_panes(&tabs, 0)
        .into_iter()
        .collect();
    assert_eq!(tab_receivers, vec![2]);

    let all_receivers: Vec<_> =
        BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: 1 }
            .receiving_panes(&tabs, 0)
            .into_iter()
            .collect();
    assert_eq!(all_receivers, vec![2, 3, 4]);
}

#[test]
fn off_has_no_receiving_panes() {
    let tabs = vec![PaneTree::leaf(1), PaneTree::leaf(2)];
    assert!(BroadcastState::Off.receiving_panes(&tabs, 0).is_empty());
}
