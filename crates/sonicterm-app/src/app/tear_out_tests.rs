//! Unit tests for tear-out's tab identity.
//!
//! A queued tear-out records where a tab sat. Positions move, so the request
//! also records which tab it means, and re-resolves the position from that id
//! when it is applied. These tests pin the re-resolution — the part that
//! decides whether the tab the user grabbed is the tab that moves.

use super::*;

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app_with_tabs(titles: &[&str]) -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    // Every seeded pane creates an inline-media charge; see the note in
    // `pane_exit_tests.rs` for why the counters are process-global.
    for title in titles {
        app.__test_seed_tab(title);
    }
    app
}

/// A tab closing at a lower index must not redirect a queued tear-out.
///
/// This is the whole defect: the recorded index stays *in range* when a tab
/// below it closes, so a bounds check passes while the slot now holds a
/// different tab. Asserting both halves — that the stale index misnames the
/// tab, and that the id still finds it — is what makes this a test of the fix
/// rather than of `position`.
#[test]
fn a_lower_tab_closing_moves_the_index_but_not_the_identity() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");

    // The user grabs the middle tab.
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");
    let last = app.tab_id_at(window, 2).expect("tab at index 2");
    assert_ne!(grabbed, last, "test setup: three distinct tabs");

    // Its neighbour below closes mid-gesture — a shell exiting, now that a
    // background thread can close a tab with no user action to serialise
    // against the drag.
    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, grabbed),
        Some(0),
        "the grabbed tab moved down one slot and must still be findable there"
    );
    // The half that shows why the id is needed: the recorded index survives
    // the close and now names the wrong tab.
    assert_eq!(
        app.tab_id_at(window, 1),
        Some(last),
        "the recorded index 1 now holds the tab that was at 2 — a build trusting the index \
         would tear out this one instead of the one the user grabbed"
    );
}

/// A tab that closed entirely must fail the tear-out, not promote a neighbour.
#[test]
fn a_tab_that_closed_resolves_to_nothing() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let doomed = app.tab_id_at(window, 0).expect("tab at index 0");

    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, doomed),
        None,
        "a closed tab must resolve to nothing so the tear-out fails, rather than moving \
         whichever tab inherited its slot"
    );
    // And the surviving tab is still reachable by its own id, so the failure
    // above is specific rather than the lookup being broken outright.
    let survivor = app.tab_id_at(window, 0).expect("survivor at index 0");
    assert_eq!(app.tab_index_of_id(window, survivor), Some(0));
}

/// The drain must resolve through the id, not the recorded index.
///
/// This drives `resolve_tear_out_source_index` — the method the drain calls —
/// rather than the lookup helpers underneath it. That distinction is the point
/// of the test: an earlier version exercised only the helpers, and a build that
/// ignored the recorded id entirely and trusted the stale index passed it.
#[test]
fn a_queued_tear_out_follows_its_tab_when_a_lower_tab_closes() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");

    // The request as the gesture recorded it.
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    // A shell exits in the tab below and closes it mid-gesture.
    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(0),
        "the drop must move the tab the user grabbed, which is now at index 0 — resolving to \
         the recorded index 1 would tear out the tab that inherited the slot"
    );
}

/// A tear-out whose tab closed must fail rather than move a neighbour.
#[test]
fn a_queued_tear_out_fails_when_its_own_tab_closed() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 0).expect("tab at index 0");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 0,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        None,
        "the grabbed tab is gone, so the tear-out must fail; index 0 still resolves to a live \
         tab and a build trusting it would tear out the wrong one"
    );
}

/// A request with no recorded id keeps the old index behaviour.
#[test]
fn a_tear_out_without_an_id_falls_back_to_its_index() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: None,
        drop_screen_pos: None,
    };

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(1),
        "a request built without an id must still resolve, or the fallback path is dead"
    );
}
