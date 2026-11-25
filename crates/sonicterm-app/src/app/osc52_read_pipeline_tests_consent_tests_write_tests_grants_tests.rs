//! OSC 5522 session grants on the consent page: Always and Ban, offered only when a program names
//! itself with `name` and `pw`, remembered per pane, PTY, owner, operation and location, revoked by
//! RIS, a move, a PTY change, a policy change or the pane closing, and rechecked at delivery. It
//! shares the write suite's harness and helpers.

use super::*;
use crate::app::kitty_clipboard_tx::KittyClipboardTx;
use crate::app::osc52_read_grants::GrantCredential;

/// The `name` (`tool`) and `pw` (`s3cret`) metadata a named program adds to its packets.
const NAMED: &str = ":name=dG9vbA==:pw=czNjcmV0";

/// The credential [`NAMED`] decodes to.
fn named() -> Option<GrantCredential> {
    GrantCredential::new("tool", "s3cret")
}

/// Answer the open page with the choice at `choice` (0 Always, 1 Ban): the page opens on Deny, and
/// the choices follow it.
fn choose(harness: &mut Harness, choice: usize) {
    for _ in 0..=choice {
        press(harness, NamedKey::ArrowDown);
    }
    press(harness, NamedKey::Enter);
}

/// The open page's row labels, in display order.
fn rows(harness: &Harness) -> Vec<String> {
    let palette = &harness.app.interaction.command_palette;
    palette.confirm_rows().iter().map(|row| row.label().to_owned()).collect()
}

fn grant_count(harness: &Harness) -> usize {
    harness.app.clip_state.reads.grants.len()
}

/// Contract: a named read or write offers Always and Ban with the program-supplied name on its own
/// labelled line; Always serves the next read, and writes the next write, without a page, and a
/// read grant never grants a write.
#[test]
fn read_and_write_always_grants_skip_prompt() {
    let mut harness = asking();
    kitty_read_as(&mut harness, &["text/plain"], named());
    assert!(page_open(&harness));
    let i18n = &harness.app.i18n;
    let name_line = i18n.t_args("clipboard-grant-name", Some(&[("name", "tool")]));
    for label in [i18n.translate("clipboard-grant-always"), i18n.translate("clipboard-grant-ban")] {
        assert!(rows(&harness).contains(&label), "{label} offered");
    }
    assert!(rows(&harness).contains(&name_line), "the name is shown on its own line");
    choose(&mut harness, 0);
    harness.pump_until(|harness| harness.delivered_count() == 1);
    drop(harness.take_delivered());
    kitty_read_as(&mut harness, &["text/plain"], named());
    assert!(!page_open(&harness), "a granted read skips the page");
    harness.pump_until(|harness| harness.delivered_count() == 1);
    let served = delivered_bytes(&harness);
    assert!(String::from_utf8_lossy(&served[0]).contains("status=DONE"), "served in full");

    kitty_write_as(&mut harness, "hi", NAMED);
    assert!(page_open(&harness), "a read grant does not grant writes");
    choose(&mut harness, 0);
    assert_eq!(clipboard(&harness).as_deref(), Some("hi"));
    kitty_write_as(&mut harness, "again", NAMED);
    assert!(!page_open(&harness), "a granted write skips the page");
    assert_eq!(clipboard(&harness).as_deref(), Some("again"));
}

/// Contract: Ban refuses the request `EPERM` and every later one from the same program, operation
/// and location without a page; a read ban never bans writes, and nothing is written.
#[test]
fn ban_eperm() {
    let mut harness = asking();
    let read_eperm = b"\x1b]5522;type=read:status=EPERM:id=k\x1b\\".to_vec();
    kitty_read_as(&mut harness, &["text/plain"], named());
    choose(&mut harness, 1);
    assert_eq!(delivered_bytes(&harness), vec![read_eperm.clone()]);
    kitty_read_as(&mut harness, &["text/plain"], named());
    assert!(!page_open(&harness), "a banned read skips the page");
    assert_eq!(delivered_bytes(&harness), vec![read_eperm]);

    let write_eperm = b"\x1b]5522;type=write:status=EPERM:id=k\x1b\\".to_vec();
    kitty_write_as(&mut harness, "hi", NAMED);
    assert!(page_open(&harness), "a read ban does not ban writes");
    choose(&mut harness, 1);
    kitty_write_as(&mut harness, "again", NAMED);
    assert!(!page_open(&harness), "a banned write skips the page");
    assert_eq!(delivered_bytes(&harness), vec![write_eperm.clone(), write_eperm]);
    assert_eq!(clipboard(&harness).as_deref(), Some("before"), "nothing written");
}

/// Contract: a Ban covers every read under its password, so a listing-only read, which `ask`
/// otherwise answers without a page, is refused `EPERM` too and lists nothing.
#[test]
fn ban_refuses_a_listing_only_read() {
    let mut harness = asking();
    kitty_read_as(&mut harness, &["text/plain"], named());
    choose(&mut harness, 1);
    drop(harness.take_delivered());
    kitty_read_listing_as(&mut harness, true, &[], named());
    assert!(!page_open(&harness), "a banned listing skips the page");
    let eperm = b"\x1b]5522;type=read:status=EPERM:id=k\x1b\\".to_vec();
    assert_eq!(delivered_bytes(&harness), vec![eperm], "refused, nothing listed");
}

/// Contract: a `pw` without a nonempty `name` counts as no password: the page offers no Always or
/// Ban, no grant is looked up or created, and the next request asks again.
#[test]
fn grant_requires_nonempty_name_before_lookup_and_creation() {
    let mut harness = asking();
    kitty_write_as(&mut harness, "hi", ":pw=czNjcmV0");
    assert!(page_open(&harness));
    assert_eq!(rows(&harness).len(), 4, "Allow, Deny and the two items only");
    press(&mut harness, NamedKey::ArrowDown);
    assert_eq!(harness.app.interaction.command_palette.confirm_choice(), None);
    press(&mut harness, NamedKey::Enter);
    assert_eq!(clipboard(&harness).as_deref(), Some("hi"), "Allow applies once");
    assert_eq!(grant_count(&harness), 0);
    kitty_write_as(&mut harness, "again", ":pw=czNjcmV0");
    assert!(page_open(&harness), "the next write asks again");
}

/// Contract: RIS, the pane closing, a move out of its window, a new PTY and any policy change each
/// drop the pane's grants, so the next named read asks again.
#[test]
fn grants_cleared_on_ris_close_epoch_incarnation_policy() {
    for trigger in ["ris", "close", "epoch", "incarnation", "policy"] {
        let mut harness = asking();
        kitty_read_as(&mut harness, &["text/plain"], named());
        choose(&mut harness, 0);
        harness.pump_until(|harness| harness.delivered_count() == 1);
        drop(harness.take_delivered());
        assert_eq!(grant_count(&harness), 1, "{trigger}: Always recorded a grant");
        let pane = harness.pane;
        let identity = harness.app.pane_by_id(pane).expect("the pane").read_identity.clone();
        match trigger {
            "ris" => {
                // RIS reaches the pane's OSC 5522 transaction as a section marker.
                let mut tx = KittyClipboardTx::new(harness.staging.clone(), identity.clone());
                tx.bind_pane(pane);
                let _ = tx.run(None, vec![None], Instant::now());
            }
            "close" => {
                let main = main_window(&harness);
                assert!(harness.app.__test_detach_pane(main, pane));
            }
            "epoch" => identity.bump_owner(),
            "incarnation" => identity.set_incarnation(7),
            _ => set_policy(&mut harness.app, Osc52ReadPolicy::Allow),
        }
        harness.app.pump_clipboard_reads();
        assert_eq!(grant_count(&harness), 0, "{trigger}: the grant is gone");
        if matches!(trigger, "ris" | "epoch") {
            // The pane still asks under `ask`, so the next named read opens the page again.
            kitty_read_as(&mut harness, &["text/plain"], named());
            assert!(page_open(&harness), "{trigger}: the next read asks again");
        }
    }
}

/// Contract: a read a grant approved is revalidated at delivery, so a later `deny` or a transfer
/// away and back while the native read runs sends nothing and frees its slot.
#[test]
fn deny_beats_grant_at_delivery_and_transfer_cancels() {
    for case in ["deny", "transfer"] {
        let (release, source) = FakeSource::gated("secret");
        let mut harness = harness(Osc52ReadPolicy::Ask, source.clone());
        let main = main_window(&harness);
        focus(&mut harness, Some(main));
        kitty_read_as(&mut harness, &["text/plain"], named());
        choose(&mut harness, 0);
        release.send(()).unwrap();
        harness.pump_until(|harness| harness.delivered_count() == 1);
        drop(harness.take_delivered());
        kitty_read_as(&mut harness, &["text/plain"], named());
        assert!(!page_open(&harness), "{case}: the grant approved the read");
        harness.pump_until(|_| source.calls() == 2);
        if case == "deny" {
            set_policy(&mut harness.app, Osc52ReadPolicy::Deny);
        } else {
            // When: the case is a transfer, the pane leaves its window and returns.
            let window = harness.app.registry.windows.get_mut(&main).unwrap();
            let detached = window.remove_pane(harness.pane).expect("the pane is held");
            assert!(window.panes.attach(detached).is_ok(), "the pane returns");
        }
        release.send(()).unwrap();
        harness.pump_until(|harness| harness.app.clip_state.reads.in_flight.is_none());
        assert_eq!(harness.delivered_count(), 0, "{case}: nothing is delivered");
        assert_eq!(harness.staging.ticket_slots_in_use(), 0, "{case}");
    }
}
