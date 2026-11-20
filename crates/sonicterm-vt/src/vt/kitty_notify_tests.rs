use super::*;

/// A shown notification's parts, for compact assertions.
fn shown(notify: KittyNotify) -> (Option<String>, Option<String>, String, NotifyOccasion) {
    match notify {
        KittyNotify::Show { id, title, body, occasion } => (id, title, body, occasion),
        other => panic!("expected a notification, got {other:?}"),
    }
}

/// herdr's one-shot form `OSC 99 ;; title` shows a title-only notification at once.
#[test]
fn a_one_shot_title_shows_at_once() {
    let mut notifications = KittyNotifications::default();
    assert_eq!(
        shown(notifications.receive(b";Build finished")),
        (None, Some("Build finished".into()), String::new(), NotifyOccasion::Always)
    );
}

/// herdr's two-chunk form, a title with `d=0` then the body under the same id, shows once,
/// with both parts and the id; a semicolon in a payload is text.
#[test]
fn a_title_then_body_under_one_id_shows_once() {
    let mut notifications = KittyNotifications::default();
    assert_eq!(notifications.receive(b"i=1:d=0;Agent done"), KittyNotify::Nothing);
    assert_eq!(
        shown(notifications.receive(b"i=1:p=body;tests pass; 3 files changed")),
        (
            Some("1".into()),
            Some("Agent done".into()),
            "tests pass; 3 files changed".into(),
            NotifyOccasion::Always
        )
    );
    assert_eq!(notifications.retained_bytes(), 0);
}

/// Base64 decodes whether each chunk was encoded and padded on its own (`YQ==` + `Yg==`) or one
/// encoding was split anywhere, including inside a group and inside a UTF-8 character.
#[test]
fn base64_decodes_across_both_chunking_styles() {
    let mut notifications = KittyNotifications::default();
    notifications.receive(b"i=a:d=0:e=1:p=body;YQ==");
    assert_eq!(shown(notifications.receive(b"i=a:e=1:p=body;Yg==")).2, "ab");
    // "héllo" is aMOpbGxv; split inside a group and inside the two-byte é.
    notifications.receive(b"i=b:d=0:e=1:p=body;aM");
    notifications.receive(b"i=b:d=0:e=1:p=body;Op");
    assert_eq!(shown(notifications.receive(b"i=b:e=1:p=body;bGxv")).2, "h\u{e9}llo");
}

/// The final chunk may omit its padding, as kitty's protocol allows: a two- or three-character
/// final group decodes, alone or after earlier chunks of the same id.
#[test]
fn an_unpadded_final_group_decodes() {
    let mut notifications = KittyNotifications::default();
    assert_eq!(shown(notifications.receive(b"e=1:p=body;YWI")).2, "ab");
    assert_eq!(shown(notifications.receive(b"e=1:p=body;YQ")).2, "a");
    assert_eq!(notifications.receive(b"i=u:d=0:e=1:p=body;aGVs"), KittyNotify::Nothing);
    assert_eq!(shown(notifications.receive(b"i=u:e=1:p=body;bG8")).2, "hello");
}

/// Invalid base64, a single leftover character and bytes that are not UTF-8 drop the notification.
#[test]
fn malformed_payloads_show_nothing() {
    let mut notifications = KittyNotifications::default();
    assert_eq!(notifications.receive(b"e=1:p=body;not*base64"), KittyNotify::Nothing);
    assert_eq!(notifications.receive(b"e=1:p=body;YWJjZ"), KittyNotify::Nothing, "one leftover");
    assert_eq!(
        notifications.receive(b"e=1:p=body;/w=="),
        KittyNotify::Nothing,
        "0xff is not UTF-8"
    );
    assert_eq!(notifications.receive(b"i=bad id;x"), KittyNotify::Nothing, "malformed id");
    assert_eq!(notifications.retained_bytes(), 0);
}

/// Interleaved ids accumulate apart, and the occasion travels with the notification.
#[test]
fn interleaved_ids_stay_apart_with_their_occasions() {
    let mut notifications = KittyNotifications::default();
    notifications.receive(b"i=x:d=0:o=unfocused;X title");
    notifications.receive(b"i=y:d=0:o=invisible;Y title");
    assert_eq!(
        shown(notifications.receive(b"i=x:o=unfocused:p=body;x body")),
        (Some("x".into()), Some("X title".into()), "x body".into(), NotifyOccasion::Unfocused)
    );
    assert_eq!(
        shown(notifications.receive(b"i=y:o=invisible:p=body;y body")).3,
        NotifyOccasion::Invisible
    );
}

/// At most eight notifications are received at once: a ninth id drops the oldest, and a
/// notification that would pass the byte bound is dropped whole.
#[test]
fn pending_notifications_stay_bounded() {
    let mut notifications = KittyNotifications::default();
    for id in 0..9 {
        notifications.receive(format!("i=n{id}:d=0;title {id}").as_bytes());
    }
    assert_eq!(
        notifications.receive(b"i=n0:p=body;late"),
        shown_body_only("late"),
        "n0 was evicted"
    );
    let oversized = format!("i=big:d=0;{}", "x".repeat(MAX_NOTIFY_BYTES + 1));
    assert_eq!(notifications.receive(oversized.as_bytes()), KittyNotify::Nothing);
    assert_eq!(notifications.receive(b"i=big:p=body;end"), shown_body_only("end"), "dropped whole");
    notifications.clear();
    assert_eq!(notifications.retained_bytes(), 0);
}

/// A notification that holds only `body` under `id`, as an evicted or dropped id restarts.
fn shown_body_only(body: &str) -> KittyNotify {
    let id = if body == "late" { "n0" } else { "big" };
    KittyNotify::Show {
        id: Some(id.into()),
        title: None,
        body: body.into(),
        occasion: NotifyOccasion::Always,
    }
}

/// A capability query echoes its id, or `0` without one; unsupported parts show nothing.
#[test]
fn queries_echo_their_id_and_other_parts_are_ignored() {
    let mut notifications = KittyNotifications::default();
    assert_eq!(notifications.receive(b"i=q1:p=?;"), KittyNotify::Query { id: "q1".into() });
    assert_eq!(notifications.receive(b"p=?;"), KittyNotify::Query { id: "0".into() });
    assert_eq!(notifications.receive(b"i=c:p=close;"), KittyNotify::Nothing);
}
