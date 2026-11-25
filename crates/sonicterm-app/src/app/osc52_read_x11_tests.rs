use super::*;
use crate::lib_tests::CountAllocations;
use std::collections::VecDeque;

/// One `GetProperty` reply of `kind`.
fn reply(kind: PropertyKind, format: u8, bytes_after: u32, value: &[u8]) -> PropertyReply {
    PropertyReply { kind, format, bytes_after, value: value.to_vec() }
}

/// Contract: a text reply must be format 8 of `UTF8_STRING`, `STRING` or
/// `text/plain;charset=utf-8` with nothing left after it: at the cap it is read, one byte over
/// it (or with bytes after) is `TooLarge`, and the fetch asks one unit past the cap so the extra
/// byte is visible.
#[test]
fn x11_text_reply_one_unit_over_cap_too_large() {
    let max = 8;
    assert!(fetch_length_units(max) as usize * 4 > max);
    let done = |outcome| FirstReply::Done(outcome);
    let text = |value: &str| done(ReadOutcome::Text(value.into()));
    assert_eq!(classify_first(reply(PropertyKind::Utf8, 8, 0, b"12345678"), max), text("12345678"));
    let over = classify_first(reply(PropertyKind::Utf8, 8, 0, b"123456789"), max);
    assert_eq!(over, done(ReadOutcome::TooLarge));
    let after = classify_first(reply(PropertyKind::Utf8, 8, 4, b"12345678"), max);
    assert_eq!(after, done(ReadOutcome::TooLarge));
    let wide = classify_first(reply(PropertyKind::Utf8, 32, 0, b"1234"), max);
    assert_eq!(wide, done(ReadOutcome::Failed));
    let other = classify_first(reply(PropertyKind::Other, 8, 0, b"1234"), max);
    assert_eq!(other, done(ReadOutcome::Failed));
    assert_eq!(classify_first(reply(PropertyKind::Latin1, 8, 0, &[0xE9]), max), text("é"));
    let empty = classify_first(reply(PropertyKind::Utf8, 8, 0, b""), max);
    assert_eq!(empty, done(ReadOutcome::Empty));
}

/// Contract: an `INCR` announcement is format 32 holding the lower-bound size (ICCCM 2.7.2):
/// within the cap the transfer starts, above it is `TooLarge`, and any other shape fails.
#[test]
fn x11_incr_format32_announcement_accepted() {
    let announce =
        |format, value: &[u8]| classify_first(reply(PropertyKind::Incr, format, 0, value), 8);
    assert_eq!(announce(32, &8u32.to_ne_bytes()), FirstReply::Incr);
    assert_eq!(announce(32, &9u32.to_ne_bytes()), FirstReply::Done(ReadOutcome::TooLarge));
    assert_eq!(announce(8, &8u32.to_ne_bytes()), FirstReply::Done(ReadOutcome::Failed));
    assert_eq!(announce(32, &[1, 2]), FirstReply::Done(ReadOutcome::Failed));
}

/// Contract: every `INCR` chunk is format 8 of one text type; chunks accumulate until a
/// zero-length chunk ends the transfer, and passing the cap stops it `TooLarge`.
#[test]
fn x11_incr_chunk_format8_required() {
    let start = Instant::now();
    let mut transfer = IncrTransfer::new(8, start);
    assert_eq!(transfer.push(reply(PropertyKind::Utf8, 8, 0, b"1234")), None);
    assert_eq!(transfer.remaining(), 4);
    let done = transfer.push(reply(PropertyKind::Utf8, 8, 0, b""));
    assert_eq!(done, Some(ReadOutcome::Text("1234".into())));

    let mut wide = IncrTransfer::new(8, start);
    assert_eq!(wide.push(reply(PropertyKind::Utf8, 32, 0, b"1234")), Some(ReadOutcome::Failed));
    let mut mixed = IncrTransfer::new(8, start);
    assert_eq!(mixed.push(reply(PropertyKind::Utf8, 8, 0, b"12")), None);
    assert_eq!(mixed.push(reply(PropertyKind::Latin1, 8, 0, b"3")), Some(ReadOutcome::Failed));
    let mut over = IncrTransfer::new(8, start);
    assert_eq!(over.push(reply(PropertyKind::Utf8, 8, 0, b"12345")), None);
    assert_eq!(over.push(reply(PropertyKind::Utf8, 8, 0, b"6789")), Some(ReadOutcome::TooLarge));
    let mut after = IncrTransfer::new(8, start);
    let truncated = after.push(reply(PropertyKind::Utf8, 8, 1, b"12"));
    assert_eq!(truncated, Some(ReadOutcome::TooLarge), "a chunk longer than the room left");
}

/// Contract: each `INCR` chunk waits at most 2 s, and never past 5 s from the transfer's start.
#[test]
fn incr_per_chunk_deadline() {
    let start = Instant::now();
    let transfer = IncrTransfer::new(8, start);
    assert_eq!(transfer.chunk_deadline(start), start + CHUNK_WAIT);
    assert_eq!(transfer.chunk_deadline(start + Duration::from_secs(4)), start + INCR_TOTAL_WAIT);
}

/// Contract: a UTF-8 first reply is classified by moving its property value into the text: no
/// second copy of a value that may be 8 MiB is made while the original is alive.
#[test]
fn first_reply_moves_its_value() {
    let first = reply(PropertyKind::Utf8, 8, 0, &[b'a'; 4096]);
    let counter = CountAllocations::start();
    let outcome = classify_first(first, 4096);
    let allocations = counter.count();
    drop(counter);
    assert_eq!(outcome, FirstReply::Done(ReadOutcome::Text("a".repeat(4096))));
    assert_eq!(allocations, 0, "the value is moved, not copied");
}

/// Contract: an `INCR` transfer counts the accumulated bytes and the incoming chunk together
/// against the cap, and its final conversion is built into a buffer reserved first, so the whole
/// transfer holds at most twice the cap (raw bytes plus conversion scratch) at any moment.
#[test]
fn incr_transfer_peak_within_raw_and_scratch() {
    let cap = 64 * 1024;
    let chunk_len = 16 * 1024;
    let counter = CountAllocations::start();
    let mut transfer = IncrTransfer::new(cap, Instant::now());
    let mut outcome = None;
    for index in 0..4 {
        // The last chunk ends in an invalid byte, so the converted text is exactly the cap.
        let mut value = vec![b'a'; if index == 3 { chunk_len - 2 } else { chunk_len }];
        if index == 3 {
            *value.last_mut().unwrap() = 0xFF;
        }
        assert!(value.len() <= transfer.remaining(), "the next chunk fits what is left");
        outcome = transfer.push(PropertyReply {
            kind: PropertyKind::Utf8,
            format: 8,
            bytes_after: 0,
            value,
        });
        assert_eq!(outcome, None);
    }
    let end =
        PropertyReply { kind: PropertyKind::Utf8, format: 8, bytes_after: 0, value: Vec::new() };
    outcome = outcome.or_else(|| transfer.push(end));
    let peak = counter.peak_bytes();
    drop(counter);
    let Some(ReadOutcome::Text(text)) = outcome else { panic!("a text outcome") };
    assert_eq!(text.len(), cap);
    assert!(peak <= 2 * cap, "peak {peak} over twice the cap");
}

/// A scripted X11 selection owner on a fake clock. Each poll advances the clock by `step` and
/// returns the next scripted event, then `after_events` forever; fetches return the scripted
/// replies, then `endless_chunk`. `log` records the conversions, fetches and deletes in order.
struct FakeOwner {
    clock: Instant,
    step: Duration,
    owners: VecDeque<u32>,
    events: VecDeque<Polled>,
    after_events: Polled,
    replies: VecDeque<PropertyReply>,
    endless_chunk: Option<PropertyReply>,
    log: Vec<String>,
    polls: usize,
}

impl FakeOwner {
    /// An owner that answers a conversion with `replies` after storing the property, then sends
    /// `after_events`.
    fn answering(replies: Vec<PropertyReply>, after_events: Polled) -> Self {
        Self {
            clock: Instant::now(),
            step: Duration::from_millis(10),
            owners: VecDeque::new(),
            events: VecDeque::from([Polled::Notify(true)]),
            after_events,
            replies: replies.into(),
            endless_chunk: None,
            log: Vec::new(),
            polls: 0,
        }
    }
}

impl SelectionPort for FakeOwner {
    fn owner(&mut self) -> Option<u32> {
        self.owners.pop_front()
    }

    fn convert(&mut self, target: ConvertTarget) -> bool {
        self.log.push(format!("convert {target:?}"));
        true
    }

    fn poll(&mut self) -> Polled {
        self.polls += 1;
        self.clock += self.step;
        // A runaway wait ends instead of hanging the test.
        if self.polls > 100_000 {
            return Polled::Closed;
        }
        self.events.pop_front().unwrap_or(self.after_events)
    }

    fn fetch(&mut self, delete: bool, _max_bytes: usize) -> Option<PropertyReply> {
        self.log.push(format!("fetch delete={delete}"));
        self.replies.pop_front().or_else(|| self.endless_chunk.clone())
    }

    fn delete(&mut self) {
        self.log.push("delete".into());
    }

    fn now(&self) -> Instant {
        self.clock
    }

    fn pause(&mut self) {
        self.clock += self.step;
    }
}

/// A format-32 `INCR` announcement of `size` bytes.
fn announcement(size: u32) -> PropertyReply {
    reply(PropertyKind::Incr, 32, 0, &size.to_ne_bytes())
}

/// Contract: an `INCR` announcement past the cap is refused from a fetch that does not delete it,
/// and the property is never deleted, because deleting it asks the owner for the first chunk.
#[test]
fn incr_too_large_refused_before_delete() {
    let mut owner = FakeOwner::answering(vec![announcement(9)], Polled::Idle);
    assert_eq!(read_selection(&mut owner, 8), Some(ReadOutcome::TooLarge));
    assert_eq!(owner.log, ["convert Text", "fetch delete=false"]);
}

/// Contract: an announcement within the cap is validated before it is deleted, so only then is
/// the owner asked for a chunk; an owner that then stays silent ends the read at the 2 s chunk
/// wait, with no answer.
#[test]
fn silent_incr_owner_ends_at_chunk_wait() {
    let mut owner = FakeOwner::answering(vec![announcement(8)], Polled::Idle);
    let start = owner.clock;
    assert_eq!(read_selection(&mut owner, 8), None);
    assert_eq!(owner.log, ["convert Text", "fetch delete=false", "delete"]);
    let elapsed = owner.clock - start;
    assert!(elapsed <= CHUNK_WAIT + Duration::from_millis(100), "{elapsed:?}");
}

/// Contract: an owner that answers every chunk request at once, a byte at a time, cannot keep the
/// transfer running: the deadline is checked before every poll, so the read ends with no answer
/// at the 5 s total bound, long before the cap is reached.
#[test]
fn continuously_active_owner_bounded_by_total_deadline() {
    let mut owner = FakeOwner::answering(vec![announcement(4096)], Polled::NewValue);
    owner.endless_chunk = Some(reply(PropertyKind::Utf8, 8, 0, b"a"));
    let start = owner.clock;
    assert_eq!(read_selection(&mut owner, 4096), None);
    let elapsed = owner.clock - start;
    assert!(elapsed <= INCR_TOTAL_WAIT + Duration::from_millis(100), "{elapsed:?}");
}

/// Contract: an owner that never answers while unrelated events keep arriving cannot hold the read
/// past the 2 s selection wait: the deadline is checked before every poll, not after draining.
#[test]
fn flooding_owner_bounded_by_selection_wait() {
    let mut owner = FakeOwner::answering(Vec::new(), Polled::Other);
    owner.events.clear();
    let start = owner.clock;
    assert_eq!(read_selection(&mut owner, 8), None);
    let elapsed = owner.clock - start;
    assert!(elapsed <= SELECTION_WAIT + Duration::from_millis(100), "{elapsed:?}");
}

/// The offer version `owners` (sampled in order) and a `TIMESTAMP` answer `stamp` give.
fn version_of(owners: &[u32], stamp: PropertyReply) -> Option<u64> {
    let mut owner = FakeOwner::answering(vec![stamp], Polled::Idle);
    owner.owners = owners.iter().copied().collect();
    offer_version_with(&mut owner)
}

/// Contract: the offer version is the owner window and its one nonzero format-32 INTEGER `TIMESTAMP`,
/// taken only when the owner is the same before and after the conversion; a raced owner, no
/// owner, or a malformed answer (another format, an `INCR`, a zero, more or less than one item)
/// gives no version, so no paste grant can match it.
#[test]
fn offer_version_needs_one_owner_and_a_well_formed_timestamp() {
    let stamp = |value: &[u8]| reply(PropertyKind::Integer, 32, 0, value);
    let good = 42u32.to_ne_bytes();
    assert_eq!(version_of(&[5, 5], stamp(&good)), Some((5 << 32) | 42));
    assert_eq!(version_of(&[5, 6], stamp(&good)), None, "the owner changed during conversion");
    assert_eq!(version_of(&[0], stamp(&good)), None, "no owner");
    assert_eq!(version_of(&[5, 5], reply(PropertyKind::Integer, 8, 0, &good)), None);
    assert_eq!(version_of(&[5, 5], reply(PropertyKind::Incr, 32, 0, &good)), None);
    assert_eq!(version_of(&[5, 5], stamp(&0u32.to_ne_bytes())), None, "a zero timestamp");
    assert_eq!(version_of(&[5, 5], stamp(&[1, 0, 0, 0, 2, 0, 0, 0])), None, "two items");
    assert_eq!(version_of(&[5, 5], stamp(&[])), None, "no item");
}

/// Only the exact requested atom yields bytes, direct or INCR. A lower bound over the cap
/// refuses before deleting the announcement, and a different type is never served as that MIME.
#[test]
fn x11_read_custom_mime_bounded() {
    use crate::app::osc52_read_source::TypeRead;
    let raw = |bytes: &[u8]| reply(PropertyKind::Requested, 8, 0, bytes);
    let read = |replies| {
        let mut port = FakeOwner::answering(replies, Polled::NewValue);
        let result = read_mime(&mut port, 29, 2);
        (result, port.log)
    };
    assert_eq!(read(vec![raw(&[0xff])]).0, Some(TypeRead::Bytes(vec![0xff])));
    assert_eq!(
        read(vec![announcement(2), raw(&[0xff]), raw(&[0]), raw(&[])]).0,
        Some(TypeRead::Bytes(vec![0xff, 0]))
    );
    assert_eq!(read(vec![raw(&[1, 2, 3])]).0, Some(TypeRead::TooLarge));
    let (over, calls) = read(vec![announcement(3)]);
    assert_eq!(over, Some(TypeRead::TooLarge));
    assert!(!calls.iter().any(|call| call == "delete"));
    assert_eq!(read(vec![reply(PropertyKind::Other, 8, 0, &[0])]).0, Some(TypeRead::Failed));
}

/// Case-sensitive X atoms round trip verbatim, including RFC restricted-name punctuation. Native
/// type names from other OSes are not aliases on X11; only its text spellings are normalized.
#[test]
fn x11_listing_keeps_exact_mime_atoms() {
    let names = ["TARGETS", "X/Case_1!", "TEXT", "public.html", "X/Case_1!", "text/plain"];
    assert_eq!(
        list_mime_atoms(names.map(str::to_owned)),
        super::super::osc52_read_source::TypeListing::Types(vec![
            "X/Case_1!".into(),
            "text/plain".into()
        ])
    );
}

/// A correctly sized property with the wrong type is not evidence of an ownership timestamp.
#[test]
fn x11_non_integer_timestamp_has_no_version() {
    let bytes = 42u32.to_ne_bytes();
    assert_eq!(version_of(&[5, 5], reply(PropertyKind::Other, 32, 0, &bytes)), None);
}

/// A legacy owner refuses UTF8_STRING but converts STRING. It must receive the fallback
/// conversion, and its Latin-1 bytes are decoded under the same output cap as other text.
#[test]
fn x11_string_only_owner_converts_latin1_within_cap() {
    for (cap, expected) in [(2, ReadOutcome::Text("é".into())), (1, ReadOutcome::TooLarge)] {
        let mut owner =
            FakeOwner::answering(vec![reply(PropertyKind::Latin1, 8, 0, &[0xe9])], Polled::Idle);
        owner.events = VecDeque::from([Polled::Notify(false), Polled::Notify(true)]);
        assert_eq!(read_selection(&mut owner, cap), Some(expected));
        assert_eq!(&owner.log[..2], ["convert Text", "convert Latin1"]);
    }
}
