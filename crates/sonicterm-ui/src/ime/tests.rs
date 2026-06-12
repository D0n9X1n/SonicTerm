
//! Coverage for the IME composition state machine. The renderer reads
//! `preedit()` + `cursor()` to draw the inline composition and advance
//! the cursor BLOCK to the caret byte offset — the "cursor follows the
//! insertion point during preedit" fix (#pane-ime). These pin that the
//! cursor offset is tracked, cleared, and folded correctly.

use super::*;

#[test]
fn preedit_tracks_text_and_caret_offset() {
    let mut ime = ImeState::new();
    ime.handle_enabled();
    // IME reports caret at byte 6 of a 9-byte preedit (e.g. "ni hao|wo").
    ime.handle_preedit("nihaowo", Some((6, 6)));
    assert_eq!(ime.preedit(), "nihaowo");
    assert_eq!(ime.cursor(), Some((6, 6)), "caret offset must be retained for the cursor block");
    assert!(ime.is_composing());
}

#[test]
fn caret_moves_as_composition_grows() {
    // The reported bug: cursor block frozen at the START of the preedit.
    // Each Preedit update carries a new caret offset; the state must
    // reflect the latest one so the renderer advances the block.
    let mut ime = ImeState::new();
    ime.handle_preedit("ni", Some((2, 2)));
    assert_eq!(ime.cursor().map(|(_, e)| e), Some(2));
    ime.handle_preedit("niha", Some((4, 4)));
    assert_eq!(ime.cursor().map(|(_, e)| e), Some(4), "caret must follow as more is typed");
}

#[test]
fn empty_preedit_ends_composition() {
    let mut ime = ImeState::new();
    ime.handle_preedit("ni", Some((2, 2)));
    assert!(ime.is_composing());
    // IME panel closed without commit → empty preedit ends the session.
    ime.handle_preedit("", None);
    assert!(!ime.is_composing());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.cursor(), None, "caret must clear when composition ends");
}

#[test]
fn commit_clears_preedit_and_caret_and_buffers_text() {
    let mut ime = ImeState::new();
    ime.handle_preedit("nihao", Some((5, 5)));
    ime.handle_commit("你好");
    assert!(!ime.is_composing());
    assert_eq!(ime.preedit(), "", "commit clears the inline preedit");
    assert_eq!(ime.cursor(), None, "commit clears the caret");
    assert_eq!(ime.take_commits(), "你好", "committed text is buffered for the PTY");
    assert_eq!(ime.take_commits(), "", "drain is one-shot");
}

#[test]
fn cancel_drops_preedit_without_committing() {
    let mut ime = ImeState::new();
    ime.handle_commit("已提交"); // a prior commit the host hasn't drained
    ime.handle_preedit("wip", Some((3, 3)));
    ime.cancel();
    assert!(!ime.is_composing());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.cursor(), None);
    // cancel must NOT eat an already-received commit.
    assert_eq!(ime.take_commits(), "已提交", "cancel preserves undrained commits");
}

#[test]
fn disabled_clears_composition_state() {
    let mut ime = ImeState::new();
    ime.handle_preedit("ni", Some((2, 2)));
    ime.handle_disabled();
    assert!(!ime.is_composing());
    assert_eq!(ime.preedit(), "");
    assert_eq!(ime.cursor(), None);
}
