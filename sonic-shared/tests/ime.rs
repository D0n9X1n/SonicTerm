//! Integration tests for the IME composition state machine.

use sonic_shared::ime::ImeState;

#[test]
fn fresh_state_is_idle() {
    let s = ImeState::new();
    assert!(!s.is_composing());
    assert!(s.preedit().is_empty());
    assert_eq!(s.cursor(), None);
}

#[test]
fn preedit_marks_composing_and_stores_text_and_cursor() {
    let mut s = ImeState::new();
    s.handle_enabled();
    s.handle_preedit("ni", Some((2, 2)));
    assert!(s.is_composing());
    assert_eq!(s.preedit(), "ni");
    assert_eq!(s.cursor(), Some((2, 2)));

    // Subsequent preedit overwrites (does not append).
    s.handle_preedit("nih", Some((3, 3)));
    assert_eq!(s.preedit(), "nih");
    assert_eq!(s.cursor(), Some((3, 3)));
}

#[test]
fn empty_preedit_ends_composition() {
    let mut s = ImeState::new();
    s.handle_preedit("a", Some((1, 1)));
    assert!(s.is_composing());
    s.handle_preedit("", None);
    assert!(!s.is_composing());
    assert!(s.preedit().is_empty());
}

#[test]
fn commit_drains_via_take_commits_and_clears_preedit() {
    let mut s = ImeState::new();
    s.handle_enabled();
    s.handle_preedit("ni", Some((2, 2)));
    s.handle_commit("你好");
    // After commit, composition is over and preedit is cleared.
    assert!(!s.is_composing());
    assert!(s.preedit().is_empty());
    // Take_commits returns the committed text exactly once.
    assert_eq!(s.take_commits(), "你好");
    assert_eq!(s.take_commits(), "");
}

#[test]
fn multiple_commits_accumulate_until_drained() {
    let mut s = ImeState::new();
    s.handle_commit("こん");
    s.handle_commit("にちは");
    assert_eq!(s.take_commits(), "こんにちは");
    assert_eq!(s.take_commits(), "");
}

#[test]
fn composing_blocks_raw_input_flag() {
    let mut s = ImeState::new();
    assert!(!s.is_composing(), "idle ImeState must not block raw input");
    s.handle_preedit("p", Some((1, 1)));
    assert!(s.is_composing(), "active preedit must block raw input");
    s.handle_commit("片");
    assert!(!s.is_composing(), "after commit raw input flows again");
}

#[test]
fn disabled_clears_preedit_but_preserves_pending_commit() {
    let mut s = ImeState::new();
    s.handle_commit("A");
    s.handle_preedit("x", Some((1, 1)));
    s.handle_disabled();
    assert!(!s.is_composing());
    assert!(s.preedit().is_empty());
    // Pending commit bytes survive an IME disable so the host can still
    // forward them on its next drain.
    assert_eq!(s.take_commits(), "A");
}
