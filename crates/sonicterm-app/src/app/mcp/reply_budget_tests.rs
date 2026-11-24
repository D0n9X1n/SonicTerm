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
