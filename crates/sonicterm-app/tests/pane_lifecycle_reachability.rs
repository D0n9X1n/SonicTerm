//! Every `PaneState` is reachable, and closing a tab drops the ones it owned.
//!
//! `WindowState.panes` is a flat `HashMap<u64, PaneState>`; the tab's
//! `PaneTree` is what says which ids that tab owns. Every teardown and move
//! path in the app is written against the tree:
//!
//! ```text
//! for id in state.tree.leaves() { ws.panes.remove(&id); }
//! ```
//!
//! `close_tab_at`, `detach_tab_state`, and the child-window detach path all
//! use that shape. So they share one unstated precondition: **the ids in
//! `panes` are exactly the union of the tab trees' leaves.** A pane inserted
//! into the map without a matching leaf is invisible to every one of those
//! loops — it is never removed, never dropped, and its `Drop` side effects
//! never run.
//!
//! That matters beyond the map entry. `PaneState` owns the pane's
//! inline-media charge, returned on `Drop` precisely so no teardown call site
//! has to remember to return it. An unreachable `PaneState` is a charge that
//! never comes back, and the process-wide media ceiling ratchets closed until
//! inline images stop rendering everywhere — the failure mode that motivated
//! tying the charge to `Drop` in the first place.
//!
//! These tests pin the invariant through the real operations rather than
//! asserting it once against a hand-built map.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// Ids in the main window's `panes` are exactly its tab trees' leaves.
fn assert_main_panes_match_leaves(app: &App, context: &str) {
    let mut from_map = app.__test_pane_ids();
    from_map.sort_unstable();

    let mut from_trees: Vec<u64> = app
        .main_tab_states()
        .expect("the synthetic main window exists")
        .iter()
        .flat_map(|state| state.tree.leaves())
        .collect();
    from_trees.sort_unstable();

    assert_eq!(
        from_map, from_trees,
        "{context}: `panes` must hold exactly the tab trees' leaves. Ids only in \
         the map are unreachable — no teardown path can drop them, so their \
         inline-media charge is never returned. Ids only in a tree have no state."
    );
}

/// Seeding and closing panes leaves no unreachable pane behind.
#[test]
fn closing_tabs_drops_every_pane_they_owned() {
    let mut app = app();

    for index in 0..8 {
        app.__test_seed_tab(&format!("tab {index}"));
        assert_main_panes_match_leaves(&app, &format!("after seeding tab {index}"));
    }
    assert_eq!(app.__test_pane_ids().len(), 8, "precondition: eight panes are live");
    assert_eq!(app.__test_main_tab_count(), 8);

    while !app.__test_pane_ids().is_empty() {
        let before = app.__test_pane_ids().len();
        app.__test_invoke_close_active_pane();
        assert_main_panes_match_leaves(&app, &format!("after closing (had {before} panes)"));
        assert!(
            app.__test_pane_ids().len() < before,
            "close must make progress: still {before} panes"
        );
    }

    assert!(
        app.__test_pane_ids().is_empty(),
        "closing every tab must leave no pane state behind, found {:?}",
        app.__test_pane_ids()
    );
}

/// Pane state tracks tab count exactly, across closes.
///
/// Each seeded tab owns exactly one pane, so `panes.len()` must equal the tab
/// count at every point. This is the check that catches an orphan which leaves
/// the map and the trees *agreeing* — a leaked `PaneState` whose tab is gone
/// shows up here as a pane count that outruns the tab count, where a
/// map-versus-tree comparison alone would see nothing wrong.
#[test]
fn pane_state_count_tracks_tab_count_across_closes() {
    let mut app = app();
    for index in 0..5 {
        app.__test_seed_tab(&format!("tab {index}"));
    }

    let summed: usize =
        (0..app.__test_main_tab_count()).filter_map(|i| app.__test_pane_count_in_tab(i)).sum();
    assert_eq!(
        summed,
        app.__test_pane_ids().len(),
        "per-tab leaf counts must sum to the pane map's size"
    );

    // Close down to one tab, asserting the counts stay locked together. A
    // teardown that removed the tab but kept its `PaneState` breaks this even
    // though map and tree still agree with each other.
    while app.__test_main_tab_count() > 1 {
        let tabs_before = app.__test_main_tab_count();
        app.__test_invoke_close_active_pane();
        let tabs_after = app.__test_main_tab_count();
        assert!(tabs_after < tabs_before, "close must make progress");
        assert_eq!(
            app.__test_pane_ids().len(),
            tabs_after,
            "one pane per tab: {} panes for {tabs_after} tabs — a closed tab's \
             PaneState was left behind",
            app.__test_pane_ids().len()
        );
    }
}

/// Churning tabs does not accumulate pane state.
///
/// One open/close cycle can look clean while a slow leak still doubles the map
/// over a working day. This repeats the cycle far more often than a user
/// would and asserts the map returns to its starting size every time.
#[test]
fn repeated_tab_churn_does_not_accumulate_pane_state() {
    let mut app = app();
    app.__test_seed_tab("persistent");
    let baseline = app.__test_pane_ids().len();

    for cycle in 0..200 {
        app.__test_seed_tab(&format!("scratch {cycle}"));
        app.__test_invoke_close_active_pane();

        assert_eq!(
            app.__test_pane_ids().len(),
            baseline,
            "cycle {cycle}: pane state accumulated — {} entries, expected {baseline}",
            app.__test_pane_ids().len()
        );
        assert_eq!(
            app.__test_main_tab_count(),
            baseline,
            "cycle {cycle}: tab count drifted from pane count"
        );
    }

    assert_main_panes_match_leaves(&app, "after 200 open/close cycles");
}

/// A child window's panes are dropped when its tabs close.
///
/// Child windows carry the same map/tree pair, and tear-out moves panes
/// between windows — the path most likely to orphan an entry, because a pane
/// leaves one map and must arrive in another.
#[test]
fn closing_child_window_tabs_drops_their_panes() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["one", "two", "three"]);

    assert_eq!(app.__test_child_pane_count(child), Some(3), "precondition: three child panes");
    assert_eq!(app.__test_child_tab_count(child), Some(3));

    while app.__test_child_tab_count(child).unwrap_or(0) > 0 {
        let before = app.__test_child_tab_count(child).unwrap_or(0);
        app.__test_invoke_close_tab_at_in_child(child, 0);
        let after = app.__test_child_tab_count(child).unwrap_or(0);
        assert!(after < before, "child close must make progress: still {before} tabs");

        // Panes must track tabs exactly: one tab closed, one pane gone. A
        // teardown that dropped the tab but kept its `PaneState` leaves the
        // pane count above the tab count here.
        assert_eq!(
            app.__test_child_pane_count(child).unwrap_or(0),
            after,
            "child pane count {} must track its tab count {after} — a closed \
             tab's PaneState was left behind",
            app.__test_child_pane_count(child).unwrap_or(0)
        );
    }

    assert_eq!(
        app.__test_child_pane_count(child).unwrap_or(0),
        0,
        "closing every child tab must leave no pane state behind: {:?}",
        app.__test_child_pane_ids(child)
    );
}
