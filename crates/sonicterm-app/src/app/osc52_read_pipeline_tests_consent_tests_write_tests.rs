//! `osc52 = "ask"` writes: an asked write takes a consent ticket on the same gate and palette page
//! as reads, reaches the clipboard only through Apply on its own page, and is dropped by Deny,
//! Escape, expiry or any revocation. It shares the consent suite's harness and helpers.

use super::*;
use crate::app::osc52_stream::ClipboardJob;
use sonicterm_cfg::config::Osc52Policy;
use sonicterm_ui::status_line::ClipOutcome;

/// Reload `[terminal] osc52` through the production config path, keeping the read policy.
fn set_write_policy(app: &mut App, policy: Osc52Policy) {
    let mut config = app.config.clone();
    config.terminal.osc52 = policy;
    app.apply_new_config(config);
}

/// An `osc52 = "ask"` harness whose main window is frontmost and whose test clipboard holds
/// `before`, so an approved write lands in the test buffer, never the desktop clipboard.
fn asking() -> Harness {
    let mut harness = harness(Osc52ReadPolicy::Ask, FakeSource::new("unused"));
    set_write_policy(&mut harness.app, Osc52Policy::Ask);
    harness.app.__test_set_memory_clipboard("before");
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    harness
}

fn clipboard(harness: &Harness) -> Option<String> {
    harness.app.__test_memory_clipboard()
}

/// Contract: under `osc52 = "ask"` a write never reaches the clipboard on arrival; its consent
/// page opens in the source window naming only the tab and pane, with Deny highlighted first, and
/// Apply on that page writes the exact text, then frees the ticket and program slots.
#[test]
fn ask_write_waits_for_apply_then_writes() {
    let mut harness = asking();
    assert!(request(&mut harness, b"\x1b]52;c;aGk=\x07").is_empty(), "a write is never answered");
    assert_eq!(clipboard(&harness).as_deref(), Some("before"), "nothing written before Apply");
    assert!(page_open(&harness));
    let expected =
        harness.app.i18n.t_args("clipboard-write-title", Some(&[("tab", "1"), ("pane", "1")]));
    assert_ne!(expected, "clipboard-write-title", "the title is localized");
    let palette = &harness.app.interaction.command_palette;
    assert_eq!(palette.confirm_title(), expected);
    assert!(!palette.confirm_applies(), "Deny is highlighted first");
    harness.app.interaction.command_palette.move_selection_up();
    press(&mut harness, NamedKey::Enter);
    assert_eq!(clipboard(&harness).as_deref(), Some("hi"));
    let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
    assert_eq!(slots, (0, 0));
    assert_eq!(harness.delivered_count(), 0, "a write sends the program nothing");
}

/// Contract: Deny, Escape and expiry drop an asked write: the clipboard keeps its text, nothing is
/// sent to the program, every slot and lease is freed, and the clip segment says it was denied.
#[test]
fn deny_escape_and_expiry_drop_the_write() {
    for dismiss in ["deny", "escape", "expiry"] {
        let mut harness = asking();
        request(&mut harness, b"\x1b]52;c;aGk=\x07");
        assert!(page_open(&harness), "{dismiss}");
        match dismiss {
            "deny" => press(&mut harness, NamedKey::Enter),
            "escape" => press(&mut harness, NamedKey::Escape),
            _ => harness.app.pump_clipboard_reads_at(Instant::now() + TICKET_LIFETIME),
        }
        assert!(!harness.app.interaction.command_palette.is_open(), "{dismiss}: the page closes");
        assert_eq!(clipboard(&harness).as_deref(), Some("before"), "{dismiss}");
        assert_eq!(harness.delivered_count(), 0, "{dismiss}: no reply for a write");
        let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
        assert_eq!((slots, harness.staging.reserved_bytes()), ((0, 0), 0), "{dismiss}");
        let record = harness.app.status_line.clip_record().expect("a clip record");
        assert_eq!((record.pane_id, record.outcome), (harness.pane, ClipOutcome::Denied));
    }
}

/// Contract: an asked write is revoked silently, writing nothing, when `osc52` changes while its
/// page shows, and when its pane leaves its window while it waits; its slots are freed either way.
#[test]
fn policy_or_owner_change_revokes_the_write() {
    let mut harness = asking();
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    assert!(page_open(&harness));
    set_write_policy(&mut harness.app, Osc52Policy::Allow);
    assert!(!page_open(&harness), "a policy change closes the page");
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::PolicyChanged));

    let mut harness = asking();
    focus(&mut harness, None);
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
    pane.read_identity.bump_owner();
    harness.app.pump_clipboard_reads();
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::OwnerChanged));
    assert_eq!(clipboard(&harness).as_deref(), Some("before"));
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
}

/// Contract: a write admitted under `allow` but handled after a reload to `ask` was never
/// approved, so the event loop denies it instead of writing.
#[test]
fn unapproved_write_under_ask_is_denied() {
    let mut harness = asking();
    let job = ClipboardJob::for_test(&harness.staging, "sneaky");
    let outcome = harness.app.submit_program_clipboard(harness.pane, job, false);
    assert_eq!(outcome, Some(crate::app::event_loop::ClipWriteOutcome::Denied));
    assert_eq!(clipboard(&harness).as_deref(), Some("before"));
}

/// Contract: while an asked write or read waits, the clip segment shows a waiting hint for its
/// pane, past the record's hold, and the hint ends with the ticket.
#[test]
fn waiting_ticket_shows_the_clip_hint() {
    let mut harness = asking();
    focus(&mut harness, None);
    let now = Instant::now();
    assert_eq!(harness.app.waiting_clip_record(now), None, "nothing waits yet");
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    let later = now + Duration::from_secs(60);
    let hint = harness.app.waiting_clip_record(later).expect("a hint while the write waits");
    assert_eq!((hint.pane_id, hint.outcome), (harness.pane, ClipOutcome::Waiting));
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    harness.app.pump_clipboard_reads();
    let shown = harness.app.waiting_clip_record(later).expect("a hint while its page shows");
    assert_eq!(shown.outcome, ClipOutcome::Waiting);
    press(&mut harness, NamedKey::Escape);
    assert_eq!(harness.app.waiting_clip_record(later), None, "the hint ends with the ticket");
}
