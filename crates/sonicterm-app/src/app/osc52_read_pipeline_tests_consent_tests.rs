//! The `ask` consent page: where it opens, what it names, and how each answer, revocation and
//! expiry ends its ticket. It shares the read pipeline suite's harness, fake source and triggers.

use super::*;
use sonicterm_cfg::keymap::BroadcastScope;
use sonicterm_ui::broadcast::BroadcastState;
use sonicterm_ui::command_palette::{CommandPaletteMode, ConfirmRow};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

/// Make `window` the OS-frontmost terminal window, as a native focus event records it.
fn focus(harness: &mut Harness, window: Option<WindowId>) {
    harness.app.registry.frontmost_window = window;
}

fn main_window(harness: &Harness) -> WindowId {
    harness.app.registry.main_window_id.expect("a main window")
}

/// Send `key` to the open palette, as the source window's keyboard does.
fn press(harness: &mut Harness, key: NamedKey) {
    assert!(harness.app.command_palette_handle_logical_key(&Key::Named(key)));
}

/// Whether the palette shows a read's consent page.
fn page_open(harness: &Harness) -> bool {
    harness.app.clip_state.reads.presented.is_some()
        && harness.app.interaction.command_palette.is_open()
        && harness.app.interaction.command_palette.mode() == CommandPaletteMode::Confirm
}

/// An `ask` harness whose main window is frontmost, with one `c` read on its consent page.
fn presented(source: Arc<FakeSource>) -> Harness {
    let mut harness = harness(Osc52ReadPolicy::Ask, source);
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    assert!(request(&mut harness, b"\x1b]52;c;?\x07").is_empty(), "a ticket, not a reply");
    assert!(page_open(&harness), "the page opens when the ticket arrives");
    harness
}

/// The title the page shows for the pane at `tab` and `pane`, both 1-based positions.
fn title(harness: &Harness, tab: &str, pane: &str) -> String {
    harness.app.i18n.t_args("clipboard-read-title", Some(&[("tab", tab), ("pane", pane)]))
}

/// Contract: under `ask` a read opens the palette's confirm page in its source window without
/// moving focus; the title and rows name only the tab and pane by position, never a title a
/// program set; Deny is highlighted first, and nothing is read before an answer.
#[test]
fn ask_opens_page_in_source_window_naming_tab_and_pane() {
    let source = FakeSource::new("secret");
    let mut harness = harness(Osc52ReadPolicy::Ask, source.clone());
    let main = main_window(&harness);
    // The read comes from a second tab, whose title stands for one a program set.
    let pane = harness.app.__test_seed_tab("PROGRAM-TITLE");
    focus(&mut harness, Some(main));
    request_on(&mut harness, pane, b"\x1b]52;c;?\x07");
    assert!(page_open(&harness));
    let expected = title(&harness, "2", "1");
    assert_ne!(expected, "clipboard-read-title", "the title is localized");
    let palette = &harness.app.interaction.command_palette;
    assert_eq!(palette.confirm_title(), expected);
    let allow = harness.app.i18n.translate("clipboard-read-allow");
    let deny = harness.app.i18n.translate("clipboard-read-deny");
    assert_eq!(palette.confirm_rows()[..2], [ConfirmRow::Apply(allow), ConfirmRow::Cancel(deny)]);
    let shown = std::iter::once(palette.confirm_title())
        .chain(palette.confirm_rows().iter().map(ConfirmRow::label));
    assert!(shown.into_iter().all(|text| !text.contains("PROGRAM-TITLE")));
    assert!(!palette.confirm_applies(), "Deny is highlighted first");
    assert_eq!(harness.app.interaction.palette_attached_window, None, "the main window hosts it");
    assert_eq!(harness.app.registry.frontmost_window, Some(main), "focus does not move");
    assert_eq!((source.calls(), harness.delivered_count()), (0, 0), "nothing is read yet");
}

/// Contract: the page opens only in the source window while it is frontmost and its palette is
/// closed; it never raises a window or moves focus, and never reuses or replaces an open palette.
#[test]
fn page_only_frontmost_source_and_busy_palette_waits() {
    let mut harness = harness(Osc52ReadPolicy::Ask, FakeSource::new("secret"));
    let main = main_window(&harness);
    let child = harness.app.__test_seed_child_window(&["child"]);
    let child_pane = harness.app.registry.windows[&child].tab_states[0].active_pane;
    focus(&mut harness, Some(main));
    request_on(&mut harness, child_pane, b"\x1b]52;c;?\x07");
    assert!(!harness.app.interaction.command_palette.is_open(), "never over another window");
    assert_eq!(harness.app.registry.frontmost_window, Some(main), "no raise, no focus change");
    // The user opens the palette; the child becomes frontmost while it is still open.
    harness.app.interaction.command_palette.open();
    let session = harness.app.interaction.command_palette.session();
    focus(&mut harness, Some(child));
    harness.app.pump_clipboard_reads();
    let palette = &harness.app.interaction.command_palette;
    assert_eq!((palette.mode(), palette.session()), (CommandPaletteMode::Commands, session));
    assert!(harness.app.clip_state.reads.presented.is_none(), "an open palette is never reused");
    harness.app.interaction.command_palette.close();
    harness.app.pump_clipboard_reads();
    assert!(page_open(&harness), "the page opens once the palette is free");
    assert_eq!(harness.app.interaction.palette_attached_window, Some(child));
    assert_ne!(harness.app.interaction.command_palette.session(), session, "a fresh session");
}

/// Contract: Allow consumes the page's session and approves only its ticket; the read then runs
/// the ordinary revalidated path and delivers the exact bytes to the source pane alone, even
/// while input broadcast is on, and opening and closing the palette afterwards keeps the approval.
#[test]
fn allow_delivers_exact_bytes_to_source_only() {
    let source = FakeSource::new("hello");
    let mut harness = presented(source.clone());
    let receiver = harness.app.__test_seed_tab("receiver");
    harness.app.interaction.broadcast =
        BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: harness.pane };
    harness.app.interaction.command_palette.move_selection_up();
    press(&mut harness, NamedKey::Enter);
    assert!(!harness.app.interaction.command_palette.is_open(), "Apply closes the page");
    harness.app.interaction.command_palette.open();
    harness.app.interaction.command_palette.close();
    harness.pump_until(|harness| harness.delivered_count() == 1);
    for _ in 0..3 {
        harness.app.pump_clipboard_reads();
    }
    let delivered = harness.take_delivered();
    assert_eq!(delivered.len(), 1, "one reply, never a broadcast copy");
    assert_ne!(delivered[0].0, receiver);
    assert_eq!(
        (delivered[0].0, delivered[0].1.bytes()),
        (harness.pane, &b"\x1b]52;c;aGVsbG8=\x1b\\"[..])
    );
    assert_eq!(source.calls(), 1);
}

/// How [`deny_escape_and_close_reply_empty`] dismisses the page without allowing.
#[derive(Debug, Clone, Copy)]
enum Dismiss {
    EnterOnDeny,
    Escape,
    ClickDeny,
    ClickOutside,
}

/// Contract: Enter on Deny, Escape, a click on Deny and an outside click each close the page and
/// answer the read with one empty ST reply, as a rejected prompt is; the clipboard is never read
/// and the slot is freed once the reply is written.
#[test]
fn deny_escape_and_close_reply_empty() {
    use Dismiss::*;
    for dismiss in [EnterOnDeny, Escape, ClickDeny, ClickOutside] {
        let source = FakeSource::new("secret");
        let mut harness = presented(source.clone());
        let main = main_window(&harness);
        match dismiss {
            EnterOnDeny => press(&mut harness, NamedKey::Enter),
            Escape => press(&mut harness, NamedKey::Escape),
            ClickDeny => harness.app.click_confirm_row(main, 1),
            // An outside click closes a confirm page through this call (the palette pointer
            // suite pins that route); headless tests cannot hit-test outside the palette.
            ClickOutside => harness.app.close_confirm_page(false),
        }
        assert!(!harness.app.interaction.command_palette.is_open(), "{dismiss:?}");
        let delivered = harness.take_delivered();
        assert_eq!(delivered.len(), 1, "{dismiss:?}");
        let reply = (delivered[0].0, delivered[0].1.bytes());
        assert_eq!(reply, (harness.pane, &b"\x1b]52;c;\x1b\\"[..]), "{dismiss:?}");
        assert_eq!(source.calls(), 0, "{dismiss:?}");
        drop(delivered);
        assert_eq!(harness.staging.ticket_slots_in_use(), 0, "{dismiss:?}");
    }
}

/// Contract: the open page schedules a wake at its ticket's expiry; unanswered by then, the page
/// closes and the read gets the empty reply a rejected prompt gets.
#[test]
fn expiry_while_open_closes_page_and_replies_empty() {
    let source = FakeSource::new("secret");
    let mut harness = presented(source.clone());
    let wake = harness.app.next_clipboard_read_wake().expect("the open page needs a wake");
    harness.app.pump_clipboard_reads_at(wake + Duration::from_millis(1));
    assert!(!harness.app.interaction.command_palette.is_open());
    assert!(harness.app.clip_state.reads.presented.is_none());
    let delivered = harness.take_delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].1.bytes(), b"\x1b]52;c;\x1b\\");
    assert_eq!(source.calls(), 0);
}

/// Contract: while a page is open, a second read from the same pane is refused at once with an
/// empty reply (one ticket per pane) and a read from another pane waits; neither reuses nor
/// replaces the open page, and the waiting read gets its own page, in a new session, once the
/// first is answered.
#[test]
fn second_request_while_open_never_reuses_page() {
    let mut harness = presented(FakeSource::new("secret"));
    let session = harness.app.interaction.command_palette.session();
    let first_title = harness.app.interaction.command_palette.confirm_title().to_string();
    assert_eq!(request(&mut harness, b"\x1b]52;c;?\x07"), b"\x1b]52;c;\x1b\\", "same pane");
    let other = harness.app.__test_seed_tab("other");
    assert!(request_on(&mut harness, other, b"\x1b]52;c;?\x07").is_empty(), "another pane waits");
    let palette = &harness.app.interaction.command_palette;
    assert_eq!((palette.session(), palette.confirm_title()), (session, first_title.as_str()));
    press(&mut harness, NamedKey::Escape);
    let delivered = harness.take_delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, harness.pane, "only the first read was answered");
    assert!(page_open(&harness), "the waiting read gets its own page");
    assert_ne!(harness.app.interaction.command_palette.session(), session);
    let second_title = title(&harness, "2", "1");
    assert_eq!(harness.app.interaction.command_palette.confirm_title(), second_title);
}

/// Contract: a policy change while the page is open, here to `deny`, closes the page and cancels
/// its ticket silently: no read and no reply.
#[test]
fn policy_flip_to_deny_closes_page_without_read() {
    let source = FakeSource::new("secret");
    let mut harness = presented(source.clone());
    set_policy(&mut harness.app, Osc52ReadPolicy::Deny);
    assert!(!harness.app.interaction.command_palette.is_open());
    assert_eq!((source.calls(), harness.port_calls.load(Ordering::SeqCst)), (0, 0));
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::PolicyChanged));
}

/// Contract: the pane closing, leaving its window and coming back, a command boundary, or its
/// window entering READONLY while the page is open each close the page and cancel silently.
#[test]
fn identity_change_while_open_closes_page_without_read() {
    use Trigger::*;
    for trigger in [PaneClosed, TransferAwayAndBack, CommandChanged, ReadOnly] {
        let source = FakeSource::new("secret");
        let mut harness = presented(source.clone());
        let reason = fire(&mut harness, trigger);
        harness.app.pump_clipboard_reads();
        assert!(!harness.app.interaction.command_palette.is_open(), "{trigger:?}");
        let calls = (source.calls(), harness.port_calls.load(Ordering::SeqCst));
        assert_eq!(calls, (0, 0), "{trigger:?}");
        assert_eq!(harness.staging.ticket_slots_in_use(), 0, "{trigger:?}");
        assert_eq!(harness.app.clip_state.reads.last_cancel, Some(reason), "{trigger:?}");
    }
}

/// Contract: Apply acts only on the page's own session in the window that showed it: after the
/// palette moved to another window under the same session, or another page replaced it, Apply
/// reads nothing and the ticket ends silently.
#[test]
fn apply_after_window_lost_page_reads_nothing() {
    for replaced in [false, true] {
        let source = FakeSource::new("secret");
        let mut harness = presented(source.clone());
        if replaced {
            let palette = &mut harness.app.interaction.command_palette;
            palette.start_confirm("other", "Apply", "Cancel", Vec::new());
        } else {
            // The palette's host changes without a new session, which no page owner did.
            let child = harness.app.__test_seed_child_window(&["child"]);
            harness.app.interaction.palette_attached_window = Some(child);
        }
        harness.app.interaction.command_palette.move_selection_up();
        press(&mut harness, NamedKey::Enter);
        for _ in 0..3 {
            harness.app.pump_clipboard_reads();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!harness.app.interaction.command_palette.is_open(), "replaced: {replaced}");
        let calls = (source.calls(), harness.port_calls.load(Ordering::SeqCst));
        assert_eq!(calls, (0, 0), "replaced: {replaced}");
        assert_eq!(harness.staging.ticket_slots_in_use(), 0, "replaced: {replaced}");
        let reason = harness.app.clip_state.reads.last_cancel;
        assert_eq!(reason, Some(CancelReason::PageLost), "replaced: {replaced}");
    }
}

#[cfg(test)]
#[path = "osc52_read_pipeline_tests_consent_tests_write_tests.rs"]
mod osc52_read_pipeline_tests_consent_tests_write_tests;

/// Contract: moving the source tab within its window through the production reorder advances its
/// panes' owner epoch, because the page names the tab by position: the waiting page is cancelled,
/// nothing is read or answered, and its slot is freed.
#[test]
fn tab_reorder_cancels_a_waiting_page() {
    let source = FakeSource::new("secret");
    let mut harness = harness(Osc52ReadPolicy::Ask, source.clone());
    harness.app.__test_seed_tab("second");
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    request(&mut harness, b"\x1b]52;c;?\x07");
    assert!(page_open(&harness), "the page names tab 1");
    assert!(harness.app.registry.windows.get_mut(&main).unwrap().reorder_tab(0, 1));
    harness.app.pump_clipboard_reads();
    assert!(!page_open(&harness), "the page naming the old position closed");
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::OwnerChanged));
    assert_eq!((source.calls(), harness.delivered_count()), (0, 0));
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
}
