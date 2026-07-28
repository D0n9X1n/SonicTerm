//! Moving a tab between windows re-parents its panes, and empties the window
//! it left.
//!
//! A `PaneState` carries its governor owner when a tab moves, and that owner
//! was created below the *source* window's owner. The governor has no move
//! operation, so unless the move re-parents it, two things go wrong at once:
//! the destination reports no memory for a pane it holds, and the source
//! window cannot finish closing, because a pane owner it no longer holds is
//! still counted among its open children.
//!
//! The second failure is the permanent one. A window drained by the move is
//! reaped in the same synchronous call, and reaping removes it from the window
//! map — so the periodic re-attribution pass, which walks live windows, can
//! never reach it afterwards. Its owner record stays `Closing` for the life of
//! the process and keeps the root's open-child count too high.
//!
//! These drive `transfer_tab`, the production cross-window move, rather than a
//! test seam that re-parents on the caller's behalf. A seam that calls the
//! re-attribution pass directly tests that the pass works — which it does —
//! rather than whether the production path reaches it.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

/// A tab moved to another window has its pane owners re-parented there.
#[test]
fn a_torn_out_pane_is_parented_to_the_window_it_lands_in() {
    let mut app = app();
    let source = app.__test_seed_child_window(&["only"]);
    let main = app.__test_main_window_id().expect("synthetic main window");

    let main_owner = app.__test_window_owner(main).expect("main window owner");
    let moved_pane = *app
        .__test_child_pane_ids(source)
        .expect("source window panes")
        .first()
        .expect("one seeded pane");

    // The production move: detach from the child, attach to main, reap the
    // drained source. No re-attribution call on the test's behalf.
    app.transfer_tab(Some(source), 0, None, 0).expect("transfer succeeds");

    let owner = app.__test_pane_owner(main, moved_pane).expect("moved pane kept an owner");
    let parent = app.__test_owner_snapshot(owner).expect("owner record").parent;

    assert_eq!(
        parent,
        Some(main_owner),
        "a moved pane must be parented to the window it now lives in; parented \
         to the window it left, that window reports memory for a pane it does \
         not hold and the destination reports none for one it does"
    );
}

/// A window drained by a move finishes closing its owner.
#[test]
fn a_window_emptied_by_a_tear_out_closes_its_owner() {
    let mut app = app();
    let source = app.__test_seed_child_window(&["only"]);
    let source_owner = app.__test_window_owner(source).expect("source window owner");

    app.transfer_tab(Some(source), 0, None, 0).expect("transfer succeeds");

    assert!(
        !app.__test_owner_is_open(source_owner),
        "a window whose last pane has left must finish closing its owner; \
         stranded mid-close its record is retained for the life of the \
         process, and the root keeps counting a window that no longer exists"
    );
}
