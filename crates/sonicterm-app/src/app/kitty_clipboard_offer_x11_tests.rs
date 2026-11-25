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

/// TARGETS advertises every text spelling; each yields the same bytes. TIMESTAMP identifies the
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
    let mut owner = Owner::stage(&mut port, offer, staging.lease(), 27, 290_000, now).unwrap();
    let targets = port.intern("TARGETS").unwrap();
    let timestamp = port.intern("TIMESTAMP").unwrap();
    let names = [
        "UTF8_STRING",
        "text/plain;charset=utf-8",
        "text/plain",
        "STRING",
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
    for target in &atoms[..5] {
        assert!(owner.request(&mut port, dest(7), *target, now));
        assert_eq!(port.bytes.last().unwrap(), &(*target, vec![0xff]));
    }
    assert!(owner.request(&mut port, dest(7), atoms[5], now));
    let incr = port.intern("INCR").unwrap();
    assert_eq!(port.words.last().unwrap(), &(incr, vec![300_001]));
    for expected in [&payload[..256 * 1024], &payload[256 * 1024..], &[]] {
        owner.deleted(&mut port, dest(7), now);
        assert_eq!(port.bytes.last().unwrap().1.as_slice(), expected);
    }
    assert!(port.watched.is_empty());
}

fn raw_owner(port: &mut Port, staging: &std::sync::Arc<ClipboardStaging>, now: Instant) -> Owner {
    Owner::stage(port, NativeOffer::raw_text(vec![0xfe; 3]), staging.lease(), 18, 2, now).unwrap()
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
    assert_eq!(claim(&mut port, 9, &[17], 100, &|| false), Published::Unchanged);
    assert!(port.calls.is_empty());
    assert_eq!(claim(&mut port, 9, &[17], 100, &|| true), Published::Failed);
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
    assert!(Owner::stage(&mut port, raw, staging.lease(), 2, 256, Instant::now()).is_none());
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
