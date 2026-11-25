use super::*;
use crate::app::osc52_stream::ClipboardStaging;
use std::time::Instant;

/// Each interned name has a stable atom, including the predefined property types.
#[derive(Default)]
struct Port {
    names: Vec<String>,
    bytes: Vec<(u32, Vec<u8>)>,
    words: Vec<(u32, Vec<u32>)>,
    watched: Vec<u32>,
}

impl OwnerPort for Port {
    fn intern(&mut self, name: &str) -> Option<u32> {
        if !self.names.iter().any(|known| known == name) {
            self.names.push(name.to_owned());
        }
        Some(self.names.iter().position(|known| known == name).unwrap() as u32 + 1)
    }
    fn bytes(&mut self, _: Destination, kind: u32, bytes: &[u8]) -> bool {
        self.bytes.push((kind, bytes.to_vec()));
        true
    }
    fn words(&mut self, _: Destination, kind: u32, words: &[u32]) -> bool {
        self.words.push((kind, words.to_vec()));
        true
    }
    fn watch(&mut self, window: u32, enable: bool) -> bool {
        if enable {
            self.watched.push(window);
        } else {
            self.watched.retain(|prior| *prior != window);
        }
        true
    }
}

fn dest(window: u32) -> Destination {
    Destination { window, property: 97 }
}

/// TARGETS advertises each raw-text spelling, but not STRING for invalid UTF-8. TIMESTAMP identifies the
/// acquisition. Deleting each INCR property pulls one chunk, then a zero-length terminator.
#[test]
fn x11_targets_timestamp_and_incr_serving() {
    let now = Instant::now();
    let staging = ClipboardStaging::with_budget(4 << 20, 2 << 20);
    let payload = vec![0xff; 300_001];
    let entries = vec![
        ("text/plain".into(), vec![0xff]),
        ("x/custom".into(), payload.clone()),
        ("text/html".into(), vec![]),
        ("image/png".into(), vec![]),
        ("text/uri-list".into(), vec![]),
    ];
    let super::super::OfferPlan::Offer(offer) = super::super::plan(entries, &[], &|_, _| true)
    else {
        panic!("offer")
    };
    let mut port = Port::default();
    let mut owner =
        Owner::stage(&mut port, PendingInstall { lease: staging.lease(), offer }, 27, 290_000, now)
            .unwrap();
    let targets = port.intern("TARGETS").unwrap();
    let timestamp = port.intern("TIMESTAMP").unwrap();
    let names = [
        "UTF8_STRING",
        "text/plain;charset=utf-8",
        "text/plain",
        "TEXT",
        "x/custom",
        "text/html",
        "image/png",
        "text/uri-list",
    ];
    let atoms: Vec<_> = names.iter().map(|name| port.intern(name).unwrap()).collect();
    assert!(owner.request(&mut port, dest(7), targets, now));
    let mut expected = vec![targets, timestamp];
    expected.extend(&atoms);
    assert_eq!(port.words.last().unwrap().1, expected);
    assert!(owner.request(&mut port, dest(7), timestamp, now));
    assert_eq!(port.words.last().unwrap().1, [27]);
    for target in &atoms[..4] {
        assert!(owner.request(&mut port, dest(7), *target, now));
        assert_eq!(port.bytes.last().unwrap(), &(*target, vec![0xff]));
    }
    assert!(owner.request(&mut port, dest(7), atoms[4], now));
    let incr = port.intern("INCR").unwrap();
    assert_eq!(port.words.last().unwrap(), &(incr, vec![300_001]));
    for expected in [&payload[..256 * 1024], &payload[256 * 1024..], &[]] {
        owner.deleted(&mut port, dest(7), now);
        assert_eq!(port.bytes.last().unwrap().1.as_slice(), expected);
    }
    assert!(port.watched.is_empty());
}

fn raw_owner(port: &mut Port, staging: &std::sync::Arc<ClipboardStaging>, now: Instant) -> Owner {
    Owner::stage(
        port,
        PendingInstall { lease: staging.lease(), offer: NativeOffer::raw_text(vec![0xfe; 3]) },
        18,
        2,
        now,
    )
    .unwrap()
}

/// Losing the selection refuses new clients, not the old client's remaining chunks. The real
/// staging lease survives until the terminated transfer no longer needs the offer.
#[test]
fn x11_selection_clear_during_incr_finishes_admitted_transfer_then_releases_lease() {
    let now = Instant::now();
    let mut port = Port::default();
    let staging = ClipboardStaging::with_budget(4096, 64);
    let mut owner = raw_owner(&mut port, &staging, now);
    let text = port.intern("TEXT").unwrap();
    assert!(owner.request(&mut port, dest(1), text, now));
    owner.clear();
    assert!(!owner.request(&mut port, dest(2), text, now));
    for expected in [2, 1, 0] {
        assert!(owner.live(now));
        assert!(staging.reserved_bytes() > 0);
        owner.deleted(&mut port, dest(1), now);
        assert_eq!(port.bytes.last().unwrap().1.len(), expected);
    }
    assert!(!owner.live(now));
    drop(owner);
    assert_eq!(staging.reserved_bytes(), 0);
}

/// Eight stalled readers cannot admit a ninth. Progress extends only that cursor's deadline,
/// and the offer itself expires even if no other client ever takes the selection.
#[test]
fn x11_requestor_cap_and_deadline() {
    use std::time::Duration;
    let now = Instant::now();
    let mut port = Port::default();
    let staging = ClipboardStaging::with_budget(4096, 64);
    let mut owner = raw_owner(&mut port, &staging, now);
    let text = port.intern("TEXT").unwrap();
    for window in 1..=8 {
        assert!(owner.request(&mut port, dest(window), text, now));
    }
    assert!(!owner.request(&mut port, dest(9), text, now));
    let targets = port.intern("TARGETS").unwrap();
    assert!(
        !owner.request(&mut port, dest(9), targets, now),
        "a direct conversion is also a ninth requestor"
    );
    owner.deleted(&mut port, dest(1), now + Duration::from_secs(4));
    let deadline = now + Duration::from_secs(5);
    owner.expire(&mut port, deadline);
    assert_eq!(port.watched, [1]);
    assert!(owner.request(&mut port, dest(9), text, deadline));
    owner.expire(&mut port, now + Duration::from_secs(9));
    assert_eq!(port.watched, [9]);
    assert!(!owner.live(now + Duration::from_secs(3600)));
}

/// Beyond the process cap a new case-sensitive atom is omitted at planning, but a name already
/// admitted remains publishable without interning another atom.
#[test]
fn registration_and_atom_caps_omit_extra_names() {
    let names = AtomNames::new();
    for number in 0..256 {
        assert!(names.admit(&format!("x/custom-{number}")));
    }
    assert!(names.admit("x/custom-0"));
    assert!(!names.admit("x/CUSTOM-0"));
}

/// No DISPLAY means no XWayland bridge: the planner omits even text and never consumes an atom.
#[test]
fn native_wayland_unsupported() {
    let names = AtomNames::new();
    let held = |format: &NativeFormat, _: &[u8]| holds_with(None, &names, format);
    let plan = super::super::plan(vec![("text/plain".into(), vec![1])], &[], &held);
    assert!(matches!(plan, super::super::OfferPlan::Unavailable));
    assert!(holds_with(Some(std::ffi::OsStr::new(":0")), &names, &NativeFormat::Text));
}

/// A stale binding performs no ownership request; a server that still names a different owner
/// yields Failed (EIO), never Written (DONE), with no rollback that could overwrite the winner.
#[test]
fn x11_unconfirmed_ownership_answers_eio() {
    struct Contested {
        owner: u32,
        calls: Vec<(&'static str, u32)>,
    }
    impl OwnershipPort for Contested {
        fn set_owner(&mut self, _: u32, selection: u32, _: u32) -> bool {
            self.calls.push(("set", selection));
            true
        }
        fn owner(&mut self, selection: u32) -> Option<u32> {
            self.calls.push(("get", selection));
            Some(self.owner)
        }
    }
    let mut port = Contested { owner: 42, calls: Vec::new() };
    assert!(!claim(&mut port, 9, [17, 0], 100, &|| false).committed);
    assert!(port.calls.is_empty());
    let result = claim(&mut port, 9, [17, 0], 100, &|| true);
    assert!(result.committed && !result.written());
    assert_eq!(port.calls, [("set", 17), ("get", 17)]);
    assert_eq!(port.owner, 42);
}

/// An offer that cannot reserve the native table must fail before any atom is interned.
#[test]
fn x11_table_is_reserved_before_interning() {
    let raw = NativeOffer::raw_text(vec![1]);
    let tables = owner_workspace(&raw);
    assert!(tables >= size_of::<Cursor>() * 8 + size_of::<u32>() * 7);
    let staging = ClipboardStaging::with_budget(tables - 1, 64);
    let mut port = Port::default();
    assert!(Owner::stage(
        &mut port,
        PendingInstall { lease: staging.lease(), offer: raw },
        2,
        256,
        Instant::now()
    )
    .is_none());
    assert_eq!(staging.reserved_bytes(), 0);
    assert!(port.names.is_empty());
}

/// SelectionClear transfers retain their chunk deadline even when it falls after the idle owner's
/// hour. The hour limit applies to offers that were not cleared.
#[test]
fn x11_clear_near_expiry_keeps_last_chunk_deadline() {
    let start = Instant::now();
    let now = start + Duration::from_secs(3599);
    let staging = ClipboardStaging::with_budget(4096, 64);
    let mut port = Port::default();
    let mut owner = raw_owner(&mut port, &staging, start);
    let text = port.intern("TEXT").unwrap();
    assert!(owner.request(&mut port, dest(1), text, now));
    owner.clear();
    assert!(owner.live(now + Duration::from_secs(1)));
    owner.expire(&mut port, now + Duration::from_secs(5));
    assert!(!owner.live(now + Duration::from_secs(5)));
}

/// The bound counts data atoms, including the fixed MIME and text targets. Those targets remain
/// available when custom names fill the remaining slots.
#[test]
fn x11_fixed_targets_count_toward_atom_limit() {
    let names = AtomNames::with_fixed(FIXED_DATA_ATOMS);
    let display = Some(std::ffi::OsStr::new(":0"));
    for number in 0..248 {
        assert!(holds_with(display, &names, &NativeFormat::Custom(format!("x/{number}"))));
    }
    assert!(!holds_with(display, &names, &NativeFormat::Custom("x/overflow".into())));
    for format in [NativeFormat::Text, NativeFormat::Png, NativeFormat::Html, NativeFormat::UriList]
    {
        assert!(holds_with(display, &names, &format));
    }
}

/// Moving a selection to another window on our own X connection need not send SelectionClear.
/// Retire it at confirmation, without losing PRIMARY or the previous offer's admitted transfer.
#[test]
fn x11_local_replacement_retires_only_confirmed_selections() {
    let now = Instant::now();
    let mut port = Port::default();
    let staging = ClipboardStaging::with_budget(4096, 64);
    let mut old = Active { owner: raw_owner(&mut port, &staging, now), selections: [10, 11] };
    let text = port.intern("TEXT").unwrap();
    assert!(old.owner.request(&mut port, dest(1), text, now));
    old.retire(10);
    assert!(!old.accepts(10));
    assert!(old.accepts(11));
    old.retire(11);
    assert!(!old.accepts(11));
    assert!(old.owner.live(now));
    old.owner.expire(&mut port, now + Duration::from_secs(5));
    assert!(!old.owner.live(now));
}

/// CLIPBOARD and PRIMARY have one authorization point. Revocation before that point does not
/// touch either owner; revocation after the first set cannot turn a committed write into Unchanged.
#[test]
fn x11_two_selection_claim_checks_binding_once_before_first_set() {
    use std::cell::Cell;
    struct Accepting {
        sets: Vec<u32>,
    }
    impl OwnershipPort for Accepting {
        fn set_owner(&mut self, _: u32, selection: u32, _: u32) -> bool {
            self.sets.push(selection);
            true
        }
        fn owner(&mut self, _: u32) -> Option<u32> {
            Some(9)
        }
    }
    for initially_valid in [false, true] {
        let mut port = Accepting { sets: Vec::new() };
        let checks = Cell::new(0);
        let gate = || {
            checks.set(checks.get() + 1);
            initially_valid && checks.get() == 1
        };
        let result = claim(&mut port, 9, [17, 1], 100, &gate);
        assert_eq!(checks.get(), 1, "one commit point for both selections");
        if initially_valid {
            assert!(result.written());
            assert_eq!(port.sets, [17, 1]);
        } else {
            assert!(!result.committed);
            assert!(port.sets.is_empty());
        }
    }
}

/// PRIMARY's newer server timestamp can ignore a successful SetSelectionOwner request. The
/// confirmed CLIPBOARD window must still serve the bytes and hold the lease, despite EIO.
#[test]
fn x11_unconfirmed_primary_keeps_clipboard_serving() {
    struct Competing {
        clipboard_owner: u32,
        primary_owner: u32,
    }
    impl OwnershipPort for Competing {
        fn set_owner(&mut self, window: u32, selection: u32, _: u32) -> bool {
            if selection == 17 {
                self.clipboard_owner = window;
            }
            true
        }
        fn owner(&mut self, selection: u32) -> Option<u32> {
            Some(if selection == 17 { self.clipboard_owner } else { self.primary_owner })
        }
    }
    let now = Instant::now();
    let staging = ClipboardStaging::with_budget(4096, 64);
    let mut data = Port::default();
    let owner = raw_owner(&mut data, &staging, now);
    let mut offers = std::collections::HashMap::new();
    let mut port = Competing { clipboard_owner: 42, primary_owner: 43 };
    let result =
        install_claim(&mut offers, &mut port, 9, Active { owner, selections: [17, 1] }, &|| true);
    assert!(result.committed && !result.written(), "failed, not stale or written");
    assert_eq!((result.clipboard, result.primary), (TargetOutcome::Taken, TargetOutcome::Refused));
    assert_eq!((port.clipboard_owner, port.primary_owner), (9, 43));
    let active = offers.get_mut(&9).expect("the confirmed selection keeps its owner window");
    assert!(active.accepts(17) && !active.accepts(1));
    assert!(staging.reserved_bytes() > 0);
    let target = data.intern("TEXT").unwrap();
    assert!(active.owner.request(&mut data, dest(8), target, now));
    active.owner.deleted(&mut data, dest(8), now);
    assert_eq!(data.bytes.last().unwrap().1, [0xfe; 2]);
}

/// ICCCM STRING can encode printable Latin-1 plus TAB and LF, not arbitrary Unicode, opaque
/// binary data or control sequences. The other text targets retain the original bytes.
#[test]
fn x11_string_target_only_offers_representable_latin1() {
    let now = Instant::now();
    let staging = ClipboardStaging::with_budget(1 << 20, 1 << 20);
    for (raw, encoded) in [
        ("café\t\n".as_bytes(), Some(&b"caf\xe9\t\n"[..])),
        ("猫".as_bytes(), None),
        (&[0xff][..], None),
        (b"\x1bx", None),
        ("\u{80}".as_bytes(), None),
    ] {
        let mut port = Port::default();
        let mut owner = Owner::stage(
            &mut port,
            PendingInstall { lease: staging.lease(), offer: NativeOffer::raw_text(raw.to_vec()) },
            9,
            1024,
            now,
        )
        .unwrap();
        let string = port.intern("STRING").unwrap();
        let targets = port.intern("TARGETS").unwrap();
        let utf8 = port.intern("UTF8_STRING").unwrap();
        assert!(owner.request(&mut port, dest(7), targets, now));
        assert_eq!(port.words.last().unwrap().1.contains(&string), encoded.is_some());
        assert_eq!(owner.request(&mut port, dest(7), string, now), encoded.is_some());
        if let Some(expected) = encoded {
            assert_eq!(port.bytes.last().unwrap().1, expected);
        }
        assert!(owner.request(&mut port, dest(7), utf8, now));
        assert_eq!(port.bytes.last().unwrap().1, raw);
    }
}

/// A refused table reservation frees the existing payload while its original lease is held.
#[test]
fn refused_owner_stage_frees_payload_before_lease() {
    use std::{cell::RefCell, sync::Arc};
    thread_local! {static SAMPLE:RefCell<Option<(Arc<ClipboardStaging>,usize)>>=const { RefCell::new(None) };}
    let staging = ClipboardStaging::with_budget(4096, 4096);
    let mut lease = staging.lease();
    assert!(lease.grow_to(4096));
    let payload = vec![0xff; 4096];
    let address = payload.as_ptr() as usize;
    SAMPLE.set(Some((staging.clone(), 0)));
    crate::lib_tests::watch_free(address, || {
        SAMPLE.with_borrow_mut(|sample| {
            let (staging, count) = sample.as_mut().unwrap();
            *count = staging.reserved_bytes();
        })
    });
    assert!(Owner::stage(
        &mut Port::default(),
        PendingInstall { lease, offer: NativeOffer::raw_text(payload) },
        2,
        256,
        Instant::now()
    )
    .is_none());
    assert_eq!(SAMPLE.take().unwrap().1, 4096);
    assert_eq!(staging.reserved_bytes(), 0);
}

/// Failing native setup (window creation or timestamp acquisition) drops the still-pending
/// installation, before Owner::stage. Probe the same move-only custody the server receives.
#[test]
fn refusal_order_x11_pending_install() {
    let staging = ClipboardStaging::with_budget(256, 256);
    let bytes = vec![0xfe; 256];
    let address = bytes.as_ptr() as usize;
    let mut lease = staging.lease();
    assert!(lease.grow_to(256));
    let pending = PendingInstall { offer: NativeOffer::raw_text(bytes), lease };
    let reading = crate::app::osc52_stream::reserved_at_free(&staging, address, || drop(pending));
    assert_eq!(reading, 256);
    assert_eq!(staging.reserved_bytes(), 0);
}
