//! Pins the reply budget: values are charged by their serialized length, a value that does not fit
//! is omitted whole with a marker while a later smaller one may still fit, and `null` is free.

use serde_json::{json, Value};

use super::*;

/// A value past what is left is `null` and marked `budget`, never cut; a smaller later value that
/// fits is still written, and a `null` value costs nothing and gets no marker.
#[test]
fn a_value_past_the_budget_is_omitted_whole_with_a_marker() {
    let mut budget = ReplyBudget::new();
    let mut pane = json!({});
    let large = "x".repeat(REPLY_TEXT_BUDGET_BYTES - 10);
    budget.place(&mut pane, "first", json!(large));
    budget.place(&mut pane, "second", json!("y".repeat(20)));
    budget.place(&mut pane, "third", json!("z"));
    budget.place(&mut pane, "fourth", Value::Null);
    assert_eq!(pane["first"], large.as_str());
    assert_eq!(pane["second"], Value::Null);
    assert_eq!(pane["omitted"]["second"], "budget");
    assert_eq!(pane["third"], "z", "a value that still fits is written");
    assert_eq!(pane["omitted"].get("fourth"), None, "null is no omission");
}

/// The charge is the serialized length, so a control character's escape counts on the wire.
#[test]
fn escapes_count_against_the_budget() {
    let mut budget = ReplyBudget::new();
    let mut pane = json!({});
    // Each `\u{1}` serializes as six bytes (`\u0001`), so this value costs six times its length.
    let escaped = "\u{1}".repeat(REPLY_TEXT_BUDGET_BYTES / 6 + 1);
    budget.place(&mut pane, "title", json!(escaped));
    assert_eq!(pane["omitted"]["title"], "budget");
}

/// Status lists are charged to their own [`STATUS_TEXT_BUDGET_BYTES`] sublimit as well as the
/// shared budget: a list past the sublimit with no typed form is omitted whole while the shared
/// budget still writes other fields, and only an actual omission marks the budget as omitted.
#[test]
fn status_lists_have_their_own_sublimit_inside_the_shared_budget() {
    let mut budget = ReplyBudget::new();
    let mut pane = json!({});
    budget.place(&mut pane, "title", json!("t"));
    assert!(!budget.omitted(), "a value that fits omits nothing");
    // `["…"]` adds four bytes to the text, so two such lists just fit the sublimit and a third
    // does not.
    let list = json!(["s".repeat(STATUS_TEXT_BUDGET_BYTES / 2 - 4)]);
    budget.place_status(&mut pane, "first", list.clone(), Value::Null);
    budget.place_status(&mut pane, "second", list.clone(), Value::Null);
    assert_eq!(pane["first"], list);
    assert!(!budget.omitted());
    budget.place_status(&mut pane, "third", list, Value::Null);
    assert_eq!(pane["third"], Value::Null);
    assert_eq!(pane["omitted"]["third"], "budget");
    assert!(budget.omitted(), "the sublimit's omission is recorded");
    budget.place(&mut pane, "cwd", json!("c".repeat(STATUS_TEXT_BUDGET_BYTES)));
    assert_eq!(pane["cwd"], "c".repeat(STATUS_TEXT_BUDGET_BYTES), "the shared budget has room");
}

/// A shared-budget omission of any field is recorded, so the reply can say it is partial.
#[test]
fn a_shared_budget_omission_is_recorded() {
    let mut budget = ReplyBudget::new();
    let mut pane = json!({});
    budget.place(&mut pane, "cwd", json!("x".repeat(REPLY_TEXT_BUDGET_BYTES)));
    assert!(budget.omitted());
}

/// A status list past the sublimit keeps each record's typed state, kind and progress and omits
/// only the free text, marked `budget`; a list with no text to drop is placed whole, unmarked.
#[test]
fn a_status_list_past_the_sublimit_keeps_its_typed_records() {
    let mut budget = ReplyBudget::new();
    let mut pane = json!({});
    let typed = json!([{ "state": "working", "kind": null, "progress": 40 }]);
    let long_msg = "m".repeat(STATUS_TEXT_BUDGET_BYTES);
    let full = json!([{ "state": "working", "kind": null, "progress": 40, "msg": long_msg }]);
    budget.place_status(&mut pane, "program_status", full, typed.clone());
    assert_eq!(pane["program_status"], typed, "the typed records stay");
    assert_eq!(pane["omitted"]["program_status"], "budget", "the dropped text is marked");
    assert!(budget.omitted());
    let mut quiet = json!({});
    budget.place_status(&mut quiet, "program_status", typed.clone(), typed.clone());
    assert_eq!(quiet["program_status"], typed);
    assert_eq!(quiet.get("omitted"), None, "nothing was dropped");
}
