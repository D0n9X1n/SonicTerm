//! Unit tests for the pane-exit close policy.
//!
//! The policy is a decision about the user's scrollback, so each test pins one
//! classification. A suite that only proved "a tab can close" would pass
//! against a build that closed on every disconnect — including the crash whose
//! output the user needs to read.
//!
//! Two assertion habits here are deliberate, because their absence let real
//! mutants survive an earlier version of this file:
//!
//! * **Identity, not count.** A surviving tab is named by id. Counting alone
//!   passes against a build that closes the *wrong* tab.
//! * **The panes map, not the tree.** Hold-open means the `PaneState` — and
//!   with it the scrollback — is still there. `__test_pane_count_in_tab`
//!   counts tree leaves, which a build that drops the `PaneState` while
//!   leaving its leaf would satisfy while destroying exactly what the policy
//!   exists to protect.
//!
//! Every test here takes `MEDIA_COUNTER_LOCK` as its first statement. Each
//! seeded pane creates an inline-media charge, and the per-pane budget is a
//! process-wide ceiling divided by the live charge count — so a pane alive in
//! this file shrinks the budget a sibling test is measuring, and that sibling
//! fails reporting a defect that is not there. Declaring the guard first makes
//! it drop last, after the `App` and the charges it owns.
//!
//! What these tests do *not* cover: they call the handler directly, so the VT
//! worker's disconnect arm, its bounded classification, and the `UserEvent`
//! dispatch are outside them. Those are covered by the io-layer probe and by
//! the event-loop dispatch being a single compiler-checked match arm.

use super::*;

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app_with_tabs(titles: &[&str]) -> (App, Vec<u64>) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let panes = titles.iter().map(|title| app.__test_seed_tab(title)).collect();
    (app, panes)
}

fn tab_count(app: &App) -> usize {
    app.main_tabs().map(|tabs| tabs.len()).unwrap_or(0)
}

/// Tab ids in order, so a survivor can be named rather than counted.
fn tab_ids(app: &App) -> Vec<sonicterm_ui::tabs::TabId> {
    app.main_tabs().map(|tabs| tabs.tabs().iter().map(|t| t.id).collect()).unwrap_or_default()
}

/// Whether the pane's `PaneState` — parser, grid, scrollback — is still held.
fn pane_state_lives(app: &App, pane_id: u64) -> bool {
    app.main_panes().map(|panes| panes.contains_key(&pane_id)).unwrap_or(false)
}

#[test]
fn a_clean_exit_closes_the_tab_it_emptied() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    let before = tab_ids(&app);
    assert_eq!(before.len(), 2, "precondition: two tabs seeded");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(
        tab_ids(&app),
        vec![before[1]],
        "the exited pane's own tab must close and the other must survive — asserted by id, \
         because a count alone passes against a build that closes the wrong one"
    );
    assert!(
        !pane_state_lives(&app, panes[0]),
        "the dead pane's state must be released, not orphaned in the map"
    );
    assert!(pane_state_lives(&app, panes[1]), "the untouched pane must be left alone");
}

/// The other half of the policy, and the half a careless test would miss.
///
/// A shell that died badly has left the reason on screen. Closing the tab
/// discards it along with the rest of the scrollback, which is the one outcome
/// the user cannot undo.
#[test]
fn an_unclean_exit_holds_its_tab_open() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    let before = tab_ids(&app);

    app.handle_pane_process_exited(panes[0], Some(false));

    assert_eq!(tab_ids(&app), before, "a non-zero exit must leave every tab exactly as it was");
    // The assertion that matters: holding the tab open is pointless if the
    // `PaneState` behind it is gone. A build that dispatched `PtyExit`
    // regardless of cleanliness would keep the leaf and drop the scrollback,
    // satisfying a tree-only check while destroying what the policy protects.
    assert!(
        pane_state_lives(&app, panes[0]),
        "the pane's scrollback must survive an unclean exit — that is the entire point"
    );
}

/// `None` is uncertainty, not a verdict.
///
/// It arrives when the child outlived the VT worker's bounded wait, or when
/// the probe failed. Reading it as a clean exit would close tabs on our own
/// ignorance; reading it as a crash happens to reach the same place as the
/// test above, but for the right reason.
#[test]
fn an_unknown_exit_status_holds_its_tab_open() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    let before = tab_ids(&app);

    app.handle_pane_process_exited(panes[0], None);

    assert_eq!(tab_ids(&app), before, "an unreadable status must not be treated as a clean exit");
    assert!(
        pane_state_lives(&app, panes[0]),
        "and the scrollback must survive it, for the same reason as an unclean exit"
    );
}

/// A split pane's exit is the reducer's case, not the tab's.
///
/// The tab still has something to show, so it stays; only the dead pane goes.
#[test]
fn a_clean_exit_in_a_split_closes_the_pane_and_keeps_the_tab() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["only"]);
    let survivor = panes[0];

    app.__test_split_active_right();
    let exiting = app.__test_active_pane_in_tab(0).expect("split focuses the new pane");
    assert_ne!(exiting, survivor, "precondition: the split produced a second pane");
    assert_eq!(app.__test_pane_count_in_tab(0), Some(2), "precondition: two panes");

    app.handle_pane_process_exited(exiting, Some(true));

    assert_eq!(tab_count(&app), 1, "the tab still has a live pane, so it must survive");
    assert_eq!(
        app.__test_pane_count_in_tab(0),
        Some(1),
        "the exited pane must be gone from the tree"
    );
    // Separately from the tree: the `PaneState` must actually be released.
    // Closing the tree node while leaving the state in the map leaks the
    // pane's grid and its PTY handle for the life of the window.
    assert!(!pane_state_lives(&app, exiting), "the exited pane's state must not leak in the map");
    assert!(pane_state_lives(&app, survivor), "the survivor's state must be untouched");
    assert_eq!(
        app.__test_active_pane_in_tab(0),
        Some(survivor),
        "focus must land on the pane that is still alive"
    );
}

/// An unclean exit in a split holds that pane open too.
///
/// Without this, a build that closed only unclean split panes — the exact
/// inverse of the policy — would pass every other test in this file.
#[test]
fn an_unclean_exit_in_a_split_holds_the_pane_open() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["only"]);
    let survivor = panes[0];

    app.__test_split_active_right();
    let exiting = app.__test_active_pane_in_tab(0).expect("split focuses the new pane");

    app.handle_pane_process_exited(exiting, Some(false));

    assert_eq!(
        app.__test_pane_count_in_tab(0),
        Some(2),
        "a split pane that died badly must stay in the tree"
    );
    assert!(
        pane_state_lives(&app, exiting),
        "and keep its scrollback, exactly as a sole pane would"
    );
    assert!(pane_state_lives(&app, survivor), "the sibling is unaffected either way");
}

/// A window with no tabs is not a state the app should be able to reach.
#[test]
fn the_last_tab_closing_on_a_clean_exit_takes_its_window_with_it() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["only"]);
    assert!(!app.__test_main_hidden(), "precondition: the window starts visible");
    assert!(!app.__test_pending_exit(), "precondition: no exit is pending");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(tab_count(&app), 0, "the window's only tab closed");
    assert!(
        app.__test_main_hidden(),
        "a window whose last tab closed must not stay on screen empty"
    );
    // Hiding alone leaves the process alive with nothing on screen. The
    // reaper's second half — deciding to exit when no window is left — has to
    // run too, or quitting the last tab strands a headless process.
    assert!(
        app.__test_pending_exit(),
        "with no child windows left, the last tab closing must also request exit"
    );
}

/// The policy must not depend on which window a pane was born in.
///
/// Panes created in a torn-out window run a different VT worker than
/// main-window panes. A handler that only walked the main window would leave
/// every child-window pane permanently open — and no main-window test could
/// tell.
#[test]
fn a_clean_exit_closes_a_child_windows_tab_too() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["first", "second"]);
    let child_panes = app.__test_child_pane_ids(child).expect("seeded child window");
    assert_eq!(child_panes.len(), 2, "precondition: two child tabs");

    app.handle_pane_process_exited(child_panes[0], Some(true));

    // The TAB count, not the pane count. `close_pty_pane` already scans child
    // windows and removes the `PaneState` on its own, so a pane-map assertion
    // drops 2→1 from the reducer alone and passes against a build whose child
    // branch was deleted outright — leaving a tab whose tree points at a pane
    // that no longer exists, which is the very bug this change fixes.
    assert_eq!(
        app.__test_child_tab_count(child),
        Some(1),
        "a child window's tab must close on a clean exit exactly as the main window's does"
    );
    assert!(
        !app.__test_child_pane_ids(child).unwrap_or_default().contains(&child_panes[0]),
        "and the dead pane's state must be released with it"
    );
    assert_eq!(
        tab_count(&app),
        0,
        "and the main window must be untouched — the pane did not live there"
    );
}

/// The hold-open half of the policy in a child window.
#[test]
fn an_unclean_exit_holds_a_child_windows_tab_open() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["first", "second"]);
    let child_panes = app.__test_child_pane_ids(child).expect("seeded child window");

    app.handle_pane_process_exited(child_panes[0], Some(false));

    assert_eq!(
        app.__test_child_tab_count(child),
        Some(2),
        "a child window's tab must hold open on an unclean exit"
    );
    assert!(
        app.__test_child_pane_ids(child).unwrap_or_default().contains(&child_panes[0]),
        "and its scrollback must survive, exactly as a main-window pane's does"
    );
}

/// An exit for a pane that is already gone must not disturb anything.
///
/// The classification takes a bounded wait, and the user can close the tab
/// inside it. The event then names a pane that no longer exists.
#[test]
fn an_exit_for_an_already_closed_pane_changes_nothing() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    let before = tab_ids(&app);
    app.close_tab_at(0);
    assert_eq!(tab_count(&app), 1, "precondition: the user closed the tab first");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(
        tab_ids(&app),
        vec![before[1]],
        "a late exit for a dead pane must not close the tab that is still live"
    );
    assert!(
        pane_state_lives(&app, panes[1]),
        "and must not disturb the surviving pane's state on its way through"
    );
}
