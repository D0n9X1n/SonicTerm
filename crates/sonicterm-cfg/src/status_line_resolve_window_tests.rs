//! The `window` segment's own key and how explicit lists treat the new segment.

use super::*;

/// Parse a `[status_line]` body into its config table.
fn parse(body: &str) -> StatusLineConfig {
    toml::from_str::<crate::config::Config>(&format!("[status_line]\n{body}"))
        .expect("status line table parses")
        .status_line
}

/// `number` defaults to on and accepts only a boolean; it is a known key, so no unknown-key
/// warning names it.
#[test]
fn the_window_number_key_defaults_on_and_takes_a_boolean() {
    assert!(StatusLineConfig::default().resolve().unwrap().window.number);
    let off = parse("[status_line.segment.window]\nnumber = false\n");
    assert!(!off.resolve().unwrap().window.number);
    assert!(off.unknown_keys().is_empty(), "{:?}", off.unknown_keys());
    let wrong = parse("[status_line.segment.window]\nnumber = \"no\"\n");
    let error = format!("{:#}", wrong.resolve().unwrap_err());
    assert!(error.contains("status_line.segment.window"), "{error}");
}

/// An explicit centre list is the user's own choice: it does not gain the window segment, which
/// then waits in the unlisted set with its configured keys for a session enable.
#[test]
fn an_explicit_centre_list_does_not_gain_the_window_segment() {
    let resolved = parse("center = [\"link\"]\n[status_line.segment.window]\nclick = \"none\"\n")
        .resolve()
        .unwrap();
    assert!(resolved.segment(StatusSegmentId::Window).is_none());
    let unlisted = resolved
        .unlisted
        .iter()
        .find(|segment| segment.id == StatusSegmentId::Window)
        .expect("window waits unlisted");
    assert_eq!(unlisted.side, StatusSide::Center);
    assert_eq!(unlisted.click, ClickBinding::None);
}
