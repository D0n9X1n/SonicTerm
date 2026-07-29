//! Unit tests for the pane-exit close policy.
//!
//! The policy is a decision about the user's scrollback, so each test pins one
//! classification. A suite that only proved "a tab can close" would pass
//! against a build that closed on every disconnect — including the crash whose
//! output the user needs to read.

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

#[test]
fn a_clean_exit_closes_the_tab_it_emptied() {
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    assert_eq!(tab_count(&app), 2, "precondition: two tabs seeded");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(
        tab_count(&app),
        1,
        "a shell that exited cleanly leaves its tab with nothing to show, so the tab goes"
    );
}

/// The other half of the policy, and the half a careless test would miss.
///
/// A shell that died badly has left the reason on screen. Closing the tab
/// discards it along with the rest of the scrollback, which is the one outcome
/// the user cannot undo.
#[test]
fn an_unclean_exit_holds_its_tab_open() {
    let (mut app, panes) = app_with_tabs(&["first", "second"]);

    app.handle_pane_process_exited(panes[0], Some(false));

    assert_eq!(
        tab_count(&app),
        2,
        "a non-zero exit must leave the tab open so its output stays readable"
    );
    assert_eq!(
        app.__test_pane_count_in_tab(0),
        Some(1),
        "and the pane itself must survive — an empty tab is not the hold-open state"
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
    let (mut app, panes) = app_with_tabs(&["first", "second"]);

    app.handle_pane_process_exited(panes[0], None);

    assert_eq!(tab_count(&app), 2, "an unreadable status must not be treated as a clean exit");
}

/// A split pane's exit is the reducer's case, not the tab's.
///
/// The tab still has something to show, so it stays; only the dead pane goes.
#[test]
fn a_clean_exit_in_a_split_closes_the_pane_and_keeps_the_tab() {
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
    assert_eq!(
        app.__test_active_pane_in_tab(0),
        Some(survivor),
        "focus must land on the pane that is still alive"
    );
}

/// A window with no tabs is not a state the app should be able to reach.
#[test]
fn the_last_tab_closing_on_a_clean_exit_takes_its_window_with_it() {
    let (mut app, panes) = app_with_tabs(&["only"]);
    assert!(!app.__test_main_hidden(), "precondition: the window starts visible");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(tab_count(&app), 0, "the window's only tab closed");
    assert!(
        app.__test_main_hidden(),
        "a window whose last tab closed must not stay on screen empty"
    );
}

/// An exit for a pane that is already gone must not disturb anything.
///
/// The classification takes a bounded wait, and the user can close the tab
/// inside it. The event then names a pane that no longer exists.
#[test]
fn an_exit_for_an_already_closed_pane_changes_nothing() {
    let (mut app, panes) = app_with_tabs(&["first", "second"]);
    app.close_tab_at(0);
    assert_eq!(tab_count(&app), 1, "precondition: the user closed the tab first");

    app.handle_pane_process_exited(panes[0], Some(true));

    assert_eq!(tab_count(&app), 1, "a late exit for a dead pane must not close a live tab");
}
