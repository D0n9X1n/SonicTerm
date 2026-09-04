//! Does the process-wide capture staging ceiling hold against real heap?
//!
//! Every accounting defect found in this milestone shared one shape: a figure
//! checked against the number it was derived from rather than against memory
//! actually held. A per-capture budget asserted at chosen live counts is that
//! shape exactly — it confirms the division is correct without ever asking
//! what the divisions sum to.
//!
//! These tests ask the allocator instead. They live in an integration test
//! because `#[global_allocator]` is crate-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{
    Parser, VtEvent, GUARANTEED_CONCURRENT_CAPTURES, MAX_MEDIA_PAYLOAD_BYTES,
    MAX_PROCESS_CAPTURE_STAGING_BYTES, MIN_CAPTURE_STAGING_BYTES,
};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every test in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading — and these tests each stage tens of megabytes, so the
/// misattribution is not marginal. They also contend for the same staging
/// pools, so a parallel run would have them refusing each other's captures.
///
/// A lock rather than `--test-threads=1`, because the gate cannot be told to
/// serialise one file and a suite that only works under a flag is a suite that
/// will eventually run without it.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Counting;

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` must be valid; the atomic byte update is allocation-free before forwarding it unchanged.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the exact valid layout received under `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }
    // SAFETY: `ptr` and its original `layout` must match; allocation-free bookkeeping cannot re-enter deallocation.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` and original `layout` are forwarded unchanged from the valid deallocation call.
        unsafe { System.dealloc(ptr, layout) }
    }
    // SAFETY: `ptr`, original `layout`, and `new_size` must be valid; atomic bookkeeping allocates nothing and cannot re-enter.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        // SAFETY: `ptr`, original `layout`, and `new_size` are forwarded unchanged under `GlobalAlloc::realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn held() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Tolerance over the ceiling for allocations that are not staging.
///
/// Parsers and their grids are built before the measurement window opens, so
/// what is measured is the staging the captures take plus the parsers' own
/// per-sequence bookkeeping. That bookkeeping is kilobytes against a ceiling
/// in mebibytes, so the tolerance is small on purpose: a generous one would
/// let a real overshoot hide inside it.
const SLACK_BYTES: usize = 2 * 1024 * 1024;

/// An APC introducer with no terminator, plus `payload_len` payload bytes.
///
/// The stalled-transfer shape: the capture opens, bytes accumulate, and the
/// terminator never arrives, so nothing releases the buffer.
fn stalled_capture_chunk(payload_len: usize) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload_len + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(payload_len + 3, b'A');
    chunk
}

/// An unterminated iTerm2 file sequence with exactly `payload_len` data bytes.
fn stalled_iterm2_capture_chunk(payload_len: usize) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload_len + 32);
    chunk.extend_from_slice(b"\x1b]1337;File=inline=1:");
    chunk.resize(chunk.len() + payload_len, b'A');
    chunk
}

/// The size of an ordinary encoded image, stated absolutely.
///
/// Deliberately *not* derived from `MIN_CAPTURE_STAGING_BYTES`. A payload
/// sized as the floor shrinks when the floor shrinks, so the rendering
/// assertions would keep passing while every real image was truncated —
/// measured: cutting the floor to 64 KiB left all six tests in this file green
/// until this constant was made absolute. That is the defect shape this file
/// exists to catch, reproduced in the file itself.
///
/// 3 MiB is a photograph from a phone camera, or a screenshot at retina
/// resolution: the thing a user actually pipes through `imgcat`.
const ORDINARY_IMAGE_BYTES: usize = 3 * 1024 * 1024;

/// The floor must be able to hold an ordinary image whole.
///
/// This is what fails if the ceiling is ever made to hold by quietly shrinking
/// the floor: the bound would be met and every image would be broken. A
/// compile-time assertion rather than a runtime one because both sides are
/// constants — a violation should stop the build, not wait for a test run.
const _: () = assert!(
    MIN_CAPTURE_STAGING_BYTES >= ORDINARY_IMAGE_BYTES,
    "the staging floor is below the size of an ordinary encoded image: an admitted pane \
     can no longer complete one, so the ceiling would be bought with broken pictures"
);

/// A complete kitty sequence carrying `payload_len` payload bytes.
fn whole_image_chunk(payload_len: usize) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload_len + 16);
    chunk.extend_from_slice(b"\x1b_Gf=100;");
    chunk.resize(payload_len, b'A');
    chunk.extend_from_slice(b"\x1b\\");
    chunk
}

/// The media payload length from a parser's events, if it dispatched one.
///
/// A dispatched event is whole by construction — a capture that could not be
/// staged whole is not dispatched at all — so the length is the whole claim.
fn media_payload(events: &[VtEvent]) -> Option<usize> {
    events.iter().find_map(|event| match event {
        VtEvent::Media(media) => Some(media.data.len()),
        _ => None,
    })
}

/// Real heap across every parser must stop below the process ceiling.
///
/// Measured before the pools: 64 panes each took the 4 MiB floor and the
/// process held **374 MiB against a stated 64 MiB**. Past `live == 16` the
/// floor won the clamp, so the per-capture budget stopped falling and the
/// total became `N × 4 MiB` — with no `N` at which it stopped, and nothing in
/// the workspace bounding the number of panes.
#[test]
fn real_heap_stops_below_the_process_ceiling_at_many_panes() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const PANES: usize = 64;

    // Build the input and the parsers before the measurement window, so what
    // is measured is staging rather than grids and harness buffers.
    let chunk = stalled_capture_chunk(MAX_MEDIA_PAYLOAD_BYTES);
    let mut parsers: Vec<Parser> = (0..PANES).map(|_| Parser::new(Grid::new(80, 24))).collect();

    let before = held();
    for parser in parsers.iter_mut() {
        parser.advance(&chunk);
    }
    let truth = held().saturating_sub(before);

    // Every parser must still be mid-capture, or a small total would be small
    // for the wrong reason.
    let live: usize = parsers.iter().map(Parser::live_capture_count).sum();
    assert_eq!(live, PANES, "precondition: every pane must still hold its capture");

    assert!(
        truth <= MAX_PROCESS_CAPTURE_STAGING_BYTES + SLACK_BYTES,
        "{PANES} panes hold {} MiB of real heap against a stated ceiling of {} MiB \
         — over by {} MiB",
        truth / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024),
        truth.saturating_sub(MAX_PROCESS_CAPTURE_STAGING_BYTES) / (1024 * 1024)
    );

    drop(parsers);
}

/// iTerm2 OSC 1337 must obey the same real-heap process ceiling as APC/DCS.
///
/// vte's standard OSC buffer is an unbounded `Vec`; parser figures alone would
/// miss a second private copy. This allocator measurement proves confirmed file
/// payloads leave vte before growing and stage only through the shared pool.
#[test]
fn iterm2_real_heap_stops_below_the_process_ceiling_at_many_panes() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const PANES: usize = 64;

    let chunk = stalled_iterm2_capture_chunk(MAX_MEDIA_PAYLOAD_BYTES);
    let mut parsers: Vec<Parser> = (0..PANES).map(|_| Parser::new(Grid::new(80, 24))).collect();

    let before = held();
    for parser in parsers.iter_mut() {
        parser.advance(&chunk);
    }
    let truth = held().saturating_sub(before);
    let live: usize = parsers.iter().map(Parser::live_capture_count).sum();

    assert_eq!(live, PANES, "precondition: every pane must still hold its capture");
    assert!(
        truth <= MAX_PROCESS_CAPTURE_STAGING_BYTES + SLACK_BYTES,
        "{PANES} iTerm2 panes hold {} MiB of real heap against a {} MiB ceiling",
        truth / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024)
    );

    drop(parsers);
}

/// Maximum iTerm2 metadata must not add heap outside the 64 MiB staging pool.
///
/// Thirteen captures consume every floor reservation; one also consumes the
/// entire growth pool. A heap-backed 1 KiB metadata buffer per capture pushes
/// this exact arrangement beyond the ceiling, so the tolerance is deliberately
/// smaller than the aggregate metadata that used to escape the pool.
#[test]
fn maximum_iterm2_metadata_adds_no_heap_beyond_the_staging_pool() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const METADATA_BYTES: usize = 1024 - b"File=".len();
    const TIGHT_SLACK_BYTES: usize = 4 * 1024;

    let mut header = b"\x1b]1337;File=".to_vec();
    header.extend(std::iter::repeat_n(b'a', METADATA_BYTES));
    header.push(b':');
    let floor_payload = vec![b'A'; MIN_CAPTURE_STAGING_BYTES];
    let full_payload = vec![b'A'; MAX_MEDIA_PAYLOAD_BYTES];
    let mut parsers: Vec<Parser> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| Parser::new(Grid::new(80, 24)))
        .collect();

    let before = held();
    for parser in parsers.iter_mut() {
        parser.advance(&header);
    }
    for parser in parsers.iter_mut().take(GUARANTEED_CONCURRENT_CAPTURES - 1) {
        parser.advance(&floor_payload);
    }
    parsers.last_mut().expect("one grown capture").advance(&full_payload);
    let truth = held().saturating_sub(before);

    assert!(
        truth <= MAX_PROCESS_CAPTURE_STAGING_BYTES + TIGHT_SLACK_BYTES,
        "maximum metadata added {} bytes beyond the process staging ceiling",
        truth.saturating_sub(MAX_PROCESS_CAPTURE_STAGING_BYTES)
    );

    drop(parsers);
}

/// Panes that grew alone must not let later panes push the total past the
/// ceiling.
///
/// The harder ordering, and the one a per-capture budget cannot see: four
/// captures arrive while uncontended, each grows to the full 16 MiB, and only
/// then do sixty more open. The four are already large and the budget was only
/// consulted for *future* growth, so the sixty added their floors on top.
/// Measured before the pools: **350 MiB against a stated 64 MiB.**
#[test]
fn early_grown_captures_do_not_let_later_panes_pass_the_ceiling() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const EARLY: usize = 4;
    const LATE: usize = 60;

    let big = stalled_capture_chunk(MAX_MEDIA_PAYLOAD_BYTES);
    let small = stalled_capture_chunk(MIN_CAPTURE_STAGING_BYTES);
    let mut early: Vec<Parser> = (0..EARLY).map(|_| Parser::new(Grid::new(80, 24))).collect();
    let mut late: Vec<Parser> = (0..LATE).map(|_| Parser::new(Grid::new(80, 24))).collect();

    let before = held();

    // Four panes receive a large image with nothing else in flight.
    for parser in early.iter_mut() {
        parser.advance(&big);
    }
    // Only now do sixty more open captures of their own.
    for parser in late.iter_mut() {
        parser.advance(&small);
    }

    let truth = held().saturating_sub(before);

    let live: usize = early.iter().chain(late.iter()).map(Parser::live_capture_count).sum();
    assert_eq!(live, EARLY + LATE, "precondition: every pane must still hold its capture");

    assert!(
        truth <= MAX_PROCESS_CAPTURE_STAGING_BYTES + SLACK_BYTES,
        "{EARLY} grown panes plus {LATE} later panes hold {} MiB of real heap against a \
         stated ceiling of {} MiB — over by {} MiB",
        truth / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024),
        truth.saturating_sub(MAX_PROCESS_CAPTURE_STAGING_BYTES) / (1024 * 1024)
    );

    drop(early);
    drop(late);
}

/// Interleaved captures must hold the ceiling too.
///
/// Round-robin is the shape of real concurrent transfers, and it is the
/// ordering most likely to let every capture grow a little before any of them
/// is capped. The pools must bound the total under that ordering as well as
/// under the two extremes above.
#[test]
fn interleaved_captures_hold_the_ceiling() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const PANES: usize = 40;
    const BLOCK: usize = 256 * 1024;

    let block = vec![b'A'; BLOCK];
    let mut parsers: Vec<Parser> = (0..PANES).map(|_| Parser::new(Grid::new(80, 24))).collect();

    let before = held();
    for parser in parsers.iter_mut() {
        parser.advance(b"\x1b_G");
    }
    for _ in 0..(MAX_MEDIA_PAYLOAD_BYTES / BLOCK) {
        for parser in parsers.iter_mut() {
            parser.advance(&block);
        }
    }
    let truth = held().saturating_sub(before);

    let live: usize = parsers.iter().map(Parser::live_capture_count).sum();
    assert_eq!(live, PANES, "precondition: every pane must still hold its capture");

    assert!(
        truth <= MAX_PROCESS_CAPTURE_STAGING_BYTES + SLACK_BYTES,
        "{PANES} interleaved panes hold {} MiB of real heap against a stated ceiling of \
         {} MiB — over by {} MiB",
        truth / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024),
        truth.saturating_sub(MAX_PROCESS_CAPTURE_STAGING_BYTES) / (1024 * 1024)
    );

    drop(parsers);
}

/// The bound must not be bought by breaking the picture.
///
/// The first operating principle is to give the user what they asked for. A
/// ceiling that held because every pane now truncated at a few kilobytes would
/// pass the tests above and render nothing usable, so the guarantee the floor
/// exists to make is asserted in the same file that asserts the bound.
#[test]
fn an_uncontended_pane_still_receives_a_full_size_image_whole() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // The full per-capture maximum, not merely the floor: a lone pane
    // receiving a large image is the common case, and it must be untouched by
    // the ceiling existing.
    let payload_len = MAX_MEDIA_PAYLOAD_BYTES;
    let chunk = whole_image_chunk(payload_len);

    let mut parser = Parser::new(Grid::new(80, 24));
    let events = parser.advance(&chunk);

    let len =
        media_payload(&events).expect("a terminated kitty sequence must produce a media event");

    assert!(
        len >= payload_len - 16,
        "a lone pane's image must arrive whole: got {len} of {payload_len} bytes"
    );
}

/// Every pane the pools guarantee must complete an ordinary image.
///
/// This is the property the floor exists to protect, stated as a number rather
/// than as the unbounded promise it used to be. Panes are not bounded anywhere
/// in the workspace, so "every pane, however many are active" was never
/// something a fixed ceiling could promise. What can be promised is a count,
/// and every pane inside it must actually get its image.
///
/// Asserted against the exported constant rather than against `ceiling /
/// floor`, because those differ: the growth pool is held back so a lone
/// capture can still reach the per-capture maximum. Recomputing the promise
/// from the ceiling here would be the same defect this file exists to catch —
/// a figure checked against the number it was derived from.
#[test]
fn every_guaranteed_pane_completes_an_ordinary_image() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let guaranteed = GUARANTEED_CONCURRENT_CAPTURES;

    // The promise has to be worth making. A change that held the ceiling by
    // guaranteeing one pane would satisfy every other test in this file.
    assert!(
        guaranteed >= 8,
        "the guarantee must cover a plausible working session, not just a pane or two \
         (guaranteed {guaranteed})"
    );

    let payload_len = ORDINARY_IMAGE_BYTES;
    let chunk = whole_image_chunk(payload_len);

    // Open every capture first, so they are all live at once and each one is
    // admitted under maximum contention rather than into a quiet process.
    let mut parsers: Vec<Parser> =
        (0..guaranteed).map(|_| Parser::new(Grid::new(80, 24))).collect();
    for parser in parsers.iter_mut() {
        parser.advance(b"\x1b_G");
    }
    let live: usize = parsers.iter().map(Parser::live_capture_count).sum();
    assert_eq!(live, guaranteed, "precondition: every capture is open at once");

    // Finish them one at a time. Each still sees every other capture live.
    let mut whole = 0usize;
    for parser in parsers.iter_mut() {
        let events = parser.advance(&chunk[3..]);
        if let Some(len) = media_payload(&events) {
            if len >= payload_len - 16 {
                whole += 1;
            }
        }
    }

    assert_eq!(
        whole, guaranteed,
        "the pools guarantee {guaranteed} concurrent panes an ordinary image, but only \
         {whole} received one"
    );

    drop(parsers);
}

/// A refused capture must render nothing rather than something broken.
///
/// Past the guarantee the pools have nothing left to hand out, and the choice
/// is between a capture staged at some reduced size and no capture at all. A
/// reduced one decodes to nothing for Kitty and iTerm2, and for Sixel to a
/// silently cut-off picture — the broken picture the floor exists to prevent.
/// So a refused capture dispatches no event, which is the same choice
/// `cancel_capture` already makes for a partial one.
#[test]
fn a_refused_capture_dispatches_no_broken_picture() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Commit the whole floor pool to captures that are open and staying open.
    let mut holders: Vec<Parser> =
        (0..GUARANTEED_CONCURRENT_CAPTURES).map(|_| Parser::new(Grid::new(80, 24))).collect();
    for parser in holders.iter_mut() {
        parser.advance(b"\x1b_G");
    }

    // One more pane than the pools can guarantee.
    let payload_len = ORDINARY_IMAGE_BYTES;
    let chunk = whole_image_chunk(payload_len);
    let mut refused = Parser::new(Grid::new(80, 24));
    let events = refused.advance(&chunk);

    assert!(
        media_payload(&events).is_none(),
        "a refused capture must dispatch no media event: a partial payload renders as a \
         cut-off picture, which is worse than no picture"
    );
    assert_eq!(
        refused.retained_amount().bytes,
        0,
        "and it must hold no staging, or the refusal bought nothing"
    );

    // Releasing the holders must return the pool, so the next capture is
    // admitted again. A refusal that outlived the pressure that caused it
    // would degrade the session permanently.
    drop(holders);

    let mut recovered = Parser::new(Grid::new(80, 24));
    let events = recovered.advance(&chunk);
    let len =
        media_payload(&events).expect("once the pool is released a capture must be admitted again");
    assert!(len >= payload_len - 16, "and must receive its image whole");
}
