//! DECSET 5522 paste events: which panes get an event instead of text, the event's bytes, and the
//! one-time grant each event issues, bound to its recipient and the clipboard offer, consumed once
//! by a named follow-up read and revoked by any change to either. Every clipboard is a fake: the
//! offer's types and version come from [`FakeOffer`], text and files from the test clipboard.

use super::*;
use crate::app::kitty_clipboard_read::KittyReadRequest;
use crate::app::osc52_read_grants::{GrantCredential, PASTE_GRANT_LIFETIME};
use crate::app::osc52_read_source::{ClipboardTextSource, ReadLocation, ReadOutcome, TypeListing};
use crate::app::osc52_read_ticket::{admit_kitty_read, ClipboardTicketHandle};
use crate::app::osc52_stream::{ClipState, ClipboardStaging};
use crate::app::App;
use base64::Engine as _;
use sonicterm_cfg::config::{Config, Osc52ReadPolicy};
use sonicterm_cfg::keymap::{BroadcastScope, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_io::pty::{SensitiveReply, SensitiveReplyError};
use sonicterm_ui::broadcast::BroadcastState;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_vt::vt::KittyClipboardLocation;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A clipboard offer: the types a listing names, the version the platform reports (`None` when it
/// cannot), and the text a follow-up read is served.
struct FakeOffer {
    types: Mutex<Vec<String>>,
    version: Mutex<Option<u64>>,
}

impl ClipboardTextSource for FakeOffer {
    fn read_text_bounded(&self, _location: ReadLocation, _max_bytes: usize) -> ReadOutcome {
        ReadOutcome::Text("secret".into())
    }

    fn list_types(&self, _location: ReadLocation) -> TypeListing {
        TypeListing::Types(self.types.lock().unwrap().clone())
    }

    fn offer_version(&self, _location: ReadLocation) -> Option<u64> {
        *self.version.lock().unwrap()
    }
}

/// An App under `osc52_read = "ask"` with no frontmost window, so a read that needs consent waits
/// without a page. Sensitive replies (events and read replies) are recorded and dropped at once,
/// or refused while `refuse` is set; PTY writes go to the test log.
struct Harness {
    app: App,
    pane: u64,
    staging: Arc<ClipboardStaging>,
    offer: Arc<FakeOffer>,
    sent: Arc<Mutex<Vec<(u64, Vec<u8>)>>>,
    refuse: Arc<AtomicBool>,
}

fn harness() -> Harness {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    // A private staging keeps ticket slots and the policy off the process default.
    let staging = ClipboardStaging::with_budget(1 << 20, 1 << 20);
    app.clip_state = ClipState::new(staging.clone());
    set_policy(&mut app, Osc52ReadPolicy::Ask);
    let offer = Arc::new(FakeOffer {
        types: Mutex::new(vec!["text/plain".into()]),
        version: Mutex::new(Some(7)),
    });
    app.clip_state.reads.source = offer.clone();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let refuse = Arc::new(AtomicBool::new(false));
    let (sink, refusing) = (sent.clone(), refuse.clone());
    app.clip_state.reads.port = Some(Box::new(move |pane_id, reply: SensitiveReply| {
        if refusing.load(Ordering::SeqCst) {
            return Err(SensitiveReplyError::Full(reply));
        }
        sink.lock().unwrap().push((pane_id, reply.bytes().to_vec()));
        Ok(())
    }));
    let pane = app.__test_seed_tab("main");
    app.testing.test_clipboard_text = Some("plain".into());
    app.testing.test_clipboard_files = Some(Vec::new());
    app.__test_enable_pty_write_log();
    Harness { app, pane, staging, offer, sent, refuse }
}

/// Reload `[terminal] osc52_read` through the production config path.
fn set_policy(app: &mut App, policy: Osc52ReadPolicy) {
    let mut config = Config::default();
    config.terminal.osc52_read = policy;
    app.apply_new_config(config);
}

/// Feed `bytes` to `pane`'s parser and publish its modes, as the VT worker does after a batch.
fn feed(app: &App, pane: u64, bytes: &[u8]) {
    let pane = app.pane_by_id(pane).expect("a live pane");
    let mut parser = pane.parser.lock();
    parser.advance(bytes);
    pane.__test_publish_input_modes(&parser);
}

/// Add a child window whose two tabs are `receiver` and `peer`, and broadcast from the harness pane.
fn broadcast(harness: &mut Harness) -> (winit::window::WindowId, u64, u64) {
    let child = harness.app.__test_seed_child_window(&["receiver", "peer"]);
    let window = &harness.app.registry.windows[&child];
    let (receiver, peer) = (window.tab_states[0].active_pane, window.tab_states[1].active_pane);
    harness.app.interaction.broadcast =
        BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: harness.pane };
    (child, receiver, peer)
}

impl Harness {
    fn paste(&mut self) {
        self.app.paste_clipboard_for_kind(FrontmostKind::Main);
    }

    /// The paste events sent so far, in order; an event carries no `id`, a read reply does.
    fn events(&self) -> Vec<(u64, Vec<u8>)> {
        let sent = self.sent.lock().unwrap();
        sent.iter().filter(|(_, bytes)| !contains(bytes, b":id=")).cloned().collect()
    }

    /// Whether `pane` was served the follow-up read's data.
    fn served(&self, pane: u64) -> bool {
        let sent = self.sent.lock().unwrap();
        sent.iter().any(|(target, bytes)| {
            *target == pane && contains(bytes, b":id=k:mime=dGV4dC9wbGFpbg==;c2VjcmV0")
        })
    }

    fn grant_count(&self) -> usize {
        self.app.clip_state.reads.paste_grants.len()
    }

    fn waiting(&self) -> usize {
        self.app.clip_state.reads.waiting.len()
    }

    /// Run the read pipeline until `pane` is served; false after 5 s.
    fn pump_until_served(&mut self, pane: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.served(pane) {
            if Instant::now() >= deadline {
                return false;
            }
            self.app.pump_clipboard_reads();
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Admit an OSC 5522 `text/plain` read from `pane` naming itself `name` with `password`, as
    /// its VT worker posts one.
    fn follow_up(&mut self, pane: u64, name: &str, password: &str) {
        let identity = self.app.pane_by_id(pane).expect("a live pane").read_identity.clone();
        let request = KittyReadRequest {
            id: "k".into(),
            location: KittyClipboardLocation::Standard,
            listing: false,
            types: vec!["text/plain".into()],
            bel: false,
            credential: GrantCredential::new(name, password),
        };
        let ticket = admit_kitty_read(&self.staging, &identity, pane, 0, request, Instant::now())
            .expect("an admitted read");
        self.app.admit_clipboard_ticket(ClipboardTicketHandle::new(ticket));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The `pw` field of `event` as sent (Base64) and decoded (the password a program echoes).
fn password_of(event: &[u8]) -> (String, String) {
    let text = String::from_utf8(event.to_vec()).expect("an ASCII event");
    let start = text.find(":pw=").expect("the event carries a password") + 4;
    let wire: String = text[start..]
        .chars()
        .take_while(|character| !matches!(character, ':' | ';' | '\x1b'))
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD.decode(&wire).expect("Base64 `pw`");
    (wire, String::from_utf8(decoded).expect("a UTF-8 password"))
}

/// Contract: with DECSET 5522 set, a paste sends the pane exactly OK, the listing `DATA` packet
/// and DONE, each carrying the same Base64 one-time password and no `id`, through the sensitive
/// reply slot, and writes no text to the PTY.
#[test]
fn paste_with_mode_sends_event_not_text() {
    let mut harness = harness();
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.paste();
    let events = harness.events();
    assert_eq!(events.len(), 1, "one event");
    assert_eq!(events[0].0, harness.pane);
    let (wire, password) = password_of(&events[0].1);
    let random = base64::engine::general_purpose::STANDARD.decode(&password).expect("Base64");
    assert_eq!(random.len(), 16, "16 bytes from the OS CSPRNG");
    let expected = format!(
        "\x1b]5522;type=read:status=OK:pw={wire}\x1b\\\
         \x1b]5522;type=read:status=DATA:mime=Lg==:pw={wire};dGV4dC9wbGFpbgo=\x1b\\\
         \x1b]5522;type=read:status=DONE:pw={wire}\x1b\\"
    );
    assert_eq!(String::from_utf8(events[0].1.clone()).unwrap(), expected);
    assert!(harness.app.__test_drain_pty_writes().is_empty(), "no text is pasted");
}

/// Contract: an offer holding only an image still sends the event, listing the image type, though
/// the clipboard has no text to read.
#[test]
fn image_only_offer_still_sends_event() {
    let mut harness = harness();
    *harness.offer.types.lock().unwrap() = vec!["image/png".into()];
    harness.app.testing.test_clipboard_text = None;
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.paste();
    let events = harness.events();
    assert_eq!(events.len(), 1);
    assert!(contains(&events[0].1, b":mime=Lg==:pw="), "the listing packet");
    assert!(contains(&events[0].1, b";aW1hZ2UvcG5nCg==\x1b\\"), "listing `image/png`");
}

/// Contract: recipients are chosen before the clipboard text is looked up: with no text at all,
/// the mode pane still gets its event and the text recipients get nothing.
#[test]
fn recipients_decided_before_text_lookup() {
    let mut harness = harness();
    let (_, _receiver, _peer) = broadcast(&mut harness);
    harness.app.testing.test_clipboard_text = None;
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.paste();
    let targets: Vec<u64> = harness.events().into_iter().map(|(pane, _)| pane).collect();
    assert_eq!(targets, vec![harness.pane]);
    assert!(harness.app.__test_drain_pty_writes().is_empty(), "no text for text recipients");
}

/// Contract: an open search still takes the clipboard text and nothing else, mode or not; copied
/// files still reach text recipients as quoted paths while the mode pane gets its event instead.
#[test]
fn search_and_file_routing_unchanged_for_text_recipients() {
    let mut harness = harness();
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.app.open_search();
    harness.app.testing.test_clipboard_text = Some("needle".into());
    harness.paste();
    let search = harness.app.main().unwrap().tab_states[0].search.as_ref().unwrap();
    assert_eq!(search.query, "needle");
    assert!(harness.events().is_empty() && harness.app.__test_drain_pty_writes().is_empty());

    let mut harness = self::harness();
    let (_, receiver, peer) = broadcast(&mut harness);
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.app.testing.test_clipboard_files = Some(vec!["/tmp/file".into()]);
    harness.paste();
    let targets: Vec<u64> = harness.events().into_iter().map(|(pane, _)| pane).collect();
    assert_eq!(targets, vec![harness.pane], "the mode pane gets the event");
    let quoted = b"'/tmp/file'".to_vec();
    assert_eq!(harness.app.__test_drain_pty_writes(), [(receiver, quoted.clone()), (peer, quoted)]);
}

/// Contract: a follow-up read carrying the pasted password and a nonempty name is served without
/// a prompt, and that consumes the grant.
#[test]
fn follow_up_with_otp_and_name_served_without_prompt() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    assert_eq!(harness.grant_count(), 1);
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 0, "no prompt");
    assert!(harness.pump_until_served(pane), "the read is served");
    assert_eq!(harness.grant_count(), 0, "consumed");
}

/// Contract: when the clipboard offer changed after the event, the grant is discarded at use and
/// the follow-up read goes through normal consent.
#[test]
fn clipboard_changed_after_event_requires_consent() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    *harness.offer.version.lock().unwrap() = Some(8);
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 1, "the read asks");
    assert_eq!(harness.grant_count(), 0, "the stale grant is gone");
}

/// Contract: a platform that cannot report the offer version gets the event, but no grant, so the
/// follow-up read needs consent.
#[test]
fn no_offer_version_no_grant() {
    let mut harness = harness();
    let pane = harness.pane;
    *harness.offer.version.lock().unwrap() = None;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let events = harness.events();
    assert_eq!(events.len(), 1, "the event is still sent");
    assert_eq!(harness.grant_count(), 0);
    let (_, password) = password_of(&events[0].1);
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 1);
}

/// Contract: the grant is one-time: a second read with the same password asks.
#[test]
fn otp_replay_prompts() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    harness.follow_up(pane, "Paste event", &password);
    assert!(harness.pump_until_served(pane));
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 1, "the replay asks");
}

/// Contract: a password without a nonempty name is no credential: the read asks and the grant stays
/// for a read that names itself.
#[test]
fn otp_without_name_prompts() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    harness.follow_up(pane, "", &password);
    assert_eq!(harness.waiting(), 1);
    assert_eq!(harness.grant_count(), 1, "nothing consumed it");
}

/// Contract: a grant lives 60 s; the pipeline drops it after that and the read asks.
#[test]
fn otp_expires() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    let later = Instant::now() + PASTE_GRANT_LIFETIME + Duration::from_secs(1);
    harness.app.pump_clipboard_reads_at(later);
    assert_eq!(harness.grant_count(), 0);
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 1);
}

/// Contract: a grant exists only for an event the pane's writer admitted; a refused event issues
/// none and removes the pane's earlier grant, so its password asks.
#[test]
fn failed_event_admission_removes_grant() {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    harness.refuse.store(true, Ordering::SeqCst);
    harness.paste();
    assert_eq!(harness.grant_count(), 0);
    harness.refuse.store(false, Ordering::SeqCst);
    harness.follow_up(pane, "Paste event", &password);
    assert_eq!(harness.waiting(), 1);
}

/// Contract: DECRST 5522, RIS, a move out of the window, a new PTY and any policy change each
/// revoke the grant; after DECRST or RIS a paste is plain text again.
#[test]
fn ris_and_decrst_reset_mode_and_revoke() {
    for trigger in ["decrst", "ris", "owner", "incarnation", "policy"] {
        let mut harness = harness();
        let pane = harness.pane;
        feed(&harness.app, pane, b"\x1b[?5522h");
        harness.paste();
        let (_, password) = password_of(&harness.events()[0].1);
        assert_eq!(harness.grant_count(), 1, "{trigger}");
        let identity = harness.app.pane_by_id(pane).expect("the pane").read_identity.clone();
        match trigger {
            "decrst" => feed(&harness.app, pane, b"\x1b[?5522l"),
            "ris" => feed(&harness.app, pane, b"\x1bc"),
            "owner" => identity.bump_owner(),
            "incarnation" => identity.set_incarnation(7),
            _ => set_policy(&mut harness.app, Osc52ReadPolicy::Allow),
        }
        harness.app.pump_clipboard_reads();
        assert_eq!(harness.grant_count(), 0, "{trigger}: revoked");
        if matches!(trigger, "decrst" | "ris" | "owner") {
            // The pane still asks under `ask`, so the old password opens consent.
            harness.follow_up(pane, "Paste event", &password);
            assert_eq!(harness.waiting(), 1, "{trigger}: the read asks");
        }
        if matches!(trigger, "decrst" | "ris") {
            harness.sent.lock().unwrap().clear();
            harness.paste();
            assert!(harness.events().is_empty(), "{trigger}: no event");
            assert_eq!(harness.app.__test_drain_pty_writes(), [(pane, b"plain".to_vec())]);
        }
    }
}

/// Contract: under `osc52_read = "deny"` a paste stays plain text with the mode set: no event, no
/// grant.
#[test]
fn deny_policy_plain_paste() {
    let mut harness = harness();
    set_policy(&mut harness.app, Osc52ReadPolicy::Deny);
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    harness.paste();
    assert!(harness.events().is_empty());
    assert_eq!(harness.grant_count(), 0);
    assert_eq!(harness.app.__test_drain_pty_writes(), [(harness.pane, b"plain".to_vec())]);
}

/// Contract: in a mixed broadcast each admitted mode pane gets its own event with its own password
/// and grant, a pane without the mode gets the text, and each password serves only its pane.
#[test]
fn broadcast_each_admitted_mode_pane_gets_own_event_and_grant() {
    let mut harness = harness();
    let (_, receiver, peer) = broadcast(&mut harness);
    let source = harness.pane;
    feed(&harness.app, source, b"\x1b[?5522h");
    feed(&harness.app, receiver, b"\x1b[?5522h");
    harness.paste();
    let events = harness.events();
    let targets: Vec<u64> = events.iter().map(|(pane, _)| *pane).collect();
    assert_eq!(targets, vec![source, receiver]);
    let (_, source_password) = password_of(&events[0].1);
    let (_, receiver_password) = password_of(&events[1].1);
    assert_ne!(source_password, receiver_password, "each recipient has its own password");
    assert_eq!(harness.grant_count(), 2);
    assert_eq!(harness.app.__test_drain_pty_writes(), [(peer, b"plain".to_vec())]);
    harness.follow_up(source, "Paste event", &source_password);
    assert!(harness.pump_until_served(source));
    harness.follow_up(receiver, "Paste event", &receiver_password);
    assert!(harness.pump_until_served(receiver));
}

/// Contract: one pane cannot use another recipient's password: the read asks and both grants stay.
#[test]
fn grant_not_usable_from_other_pane() {
    let mut harness = harness();
    let (_, receiver, _peer) = broadcast(&mut harness);
    let source = harness.pane;
    feed(&harness.app, source, b"\x1b[?5522h");
    feed(&harness.app, receiver, b"\x1b[?5522h");
    harness.paste();
    let (_, source_password) = password_of(&harness.events()[0].1);
    harness.follow_up(receiver, "Paste event", &source_password);
    assert_eq!(harness.waiting(), 1, "the receiver asks");
    assert_eq!(harness.grant_count(), 2);
    harness.follow_up(source, "Paste event", &source_password);
    assert!(harness.pump_until_served(source), "the source's own grant still serves it");
}

/// Contract: a READONLY broadcast receiver gets neither the event nor a grant nor text, even with
/// the mode set.
#[test]
fn readonly_receiver_gets_nothing() {
    let mut harness = harness();
    let (child, receiver, _peer) = broadcast(&mut harness);
    feed(&harness.app, harness.pane, b"\x1b[?5522h");
    feed(&harness.app, receiver, b"\x1b[?5522h");
    harness.app.registry.windows.get_mut(&child).unwrap().copy_mode =
        Some(CopyModeState::read_only_at((0, 0)));
    harness.paste();
    let targets: Vec<u64> = harness.events().into_iter().map(|(pane, _)| pane).collect();
    assert_eq!(targets, vec![harness.pane]);
    assert_eq!(harness.grant_count(), 1);
    assert!(harness.app.__test_drain_pty_writes().is_empty());
}
