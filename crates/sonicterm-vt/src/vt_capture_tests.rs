use super::*;
use crate::vt::vt_test_support::*;

#[test]
fn v120_parser_media_capture_shares_one_budget() {
    let pool = CaptureStagingPool::new();
    // The inventory recorded this as "independent parser limits only", and the
    // two capture slots do sit on separate structs with separate ceilings. But
    // beginning any escape family cancels a capture already in flight, so they
    // are alternatives rather than addends: the parser cannot hold both.
    //
    // Measured before writing this — feeding an 8 MiB DCS capture and then an
    // 8 MiB APC capture grows resident memory once, not twice. So the budget
    // that needs guarding is not their sum but the exclusivity that keeps a sum
    // from arising, and that is what this asserts.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    assert_eq!(parser.live_capture_count(), 0, "a fresh parser holds no capture");
    assert_eq!(parser.retained_amount().items, 0);

    // Open a DCS Sixel capture and leave it unterminated.
    let mut dcs = b"\x1bPq".to_vec();
    dcs.extend(std::iter::repeat_n(b'#', 64 * 1024));
    parser.advance(&dcs);
    let during_dcs = parser.retained_amount();
    assert_eq!(during_dcs.items, 1, "one capture is live");
    assert!(during_dcs.bytes >= 64 * 1024, "the capture reports what it is holding");

    // Open an APC capture while the DCS one is still open.
    let mut apc = b"\x1b_G".to_vec();
    apc.extend(std::iter::repeat_n(b'A', 64 * 1024));
    parser.advance(&apc);

    // Measured: the escape terminates the capture in flight, and vte's DCS
    // unhook drains it into a completed media event before the next family
    // begins. Exclusivity is therefore upheld by the vendored state machine
    // rather than by anything here, which is worth stating plainly — this
    // asserts the property holds, not that SonicTerm is what holds it.
    assert!(
        parser.live_capture_count() <= 1,
        "two captures live at once is the state a summed budget would guard, \
         and it must stay unreachable; saw {}",
        parser.live_capture_count()
    );

    // Terminating returns the parser to holding nothing.
    parser.advance(b"\x1b\\");
    assert_eq!(parser.live_capture_count(), 0, "a terminated capture is released");
    assert_eq!(
        parser.retained_amount().items,
        0,
        "retention falls to zero once no capture is in flight"
    );
}

#[test]
fn parser_retention_excludes_the_grid_it_owns() {
    // A pane composes the parser's figure with the grid's rather than one
    // restating the other. Filling the grid must not change what the parser
    // reports holding, or a pane charging both would double count every cell.
    let mut parser = Parser::new(Grid::new(80, 24));
    let empty = parser.retained_amount();

    parser.advance(b"hello world");
    for _ in 0..200 {
        parser.advance(b"filling the scrollback with rows\r\n");
    }

    assert!(parser.grid().retained_amount().bytes > 0, "the grid holds cells");
    assert_eq!(
        parser.retained_amount(),
        empty,
        "grid growth belongs to the grid's figure, not the parser's"
    );
}

/// The three retention seams a pane must add up, and the fact that none of
/// them restates another.
///
/// `Parser::retained_amount`, `Grid::retained_amount`, and
/// `HyperlinkRegistry::retained_bytes` each report only what they own. That
/// is deliberate — the registry's own docs say it is "exposed so a governor
/// charges what the registry actually admits rather than a second estimate of
/// it" — and it means the pane owner charges the sum of all three.
///
/// The failure mode this guards is silent under-charging: an integrator who
/// composes grid + parser and stops there misses up to
/// `MAX_HYPERLINK_METADATA_BYTES` (8 MiB) per parser, because hyperlink
/// metadata hangs off the parser rather than being reported by it. Measured
/// here: 200 interned links retain ~18 KB that neither of the other two
/// figures moves by a single byte.
#[test]
fn the_three_retention_seams_are_disjoint_and_must_be_summed() {
    let mut parser = Parser::new(Grid::new(80, 24));

    let parser_before = parser.retained_amount().bytes;
    let grid_before = parser.grid().retained_amount().bytes;
    let links_before = parser.hyperlinks().retained_bytes();
    assert_eq!(links_before, 0, "a fresh parser interns no hyperlinks");

    // OSC 8 hyperlinks: ESC ] 8 ; id=N ; URI ESC \ ... ESC ] 8 ; ; ESC \
    for index in 0..200u32 {
        let sequence = format!(
            "\x1b]8;id={index};https://example.com/a-fairly-long-path/{index}\x1b\\link\x1b]8;;\x1b\\"
        );
        parser.advance(sequence.as_bytes());
    }

    let parser_after = parser.retained_amount().bytes;
    let links_after = parser.hyperlinks().retained_bytes();

    assert_eq!(parser.hyperlinks().len(), 200, "every link interned");
    assert!(links_after > 0, "interning links must retain bytes");

    // The parser does not restate hyperlink bytes. If it did, a pane summing
    // all three would charge them twice.
    assert_eq!(
        parser_after, parser_before,
        "Parser::retained_amount must not move when only hyperlinks are interned; \
         it reports capture and OSC accumulator bytes only"
    );

    // The grid moves, and must: a linked cell allocates a 40-byte
    // `Box<FatAttributes>` that the grid owns. What the registry meters is the
    // URI *strings* — `(id + uri) * 2` — which is a different allocation. The
    // two are disjoint, so both are charged exactly once.
    //
    // This assertion previously required the grid figure not to move at all,
    // using that as a proxy for "no double counting". The proxy was wrong in
    // the direction that matters: it also holds when the grid fails to count
    // something it genuinely owns, which is the defect it was meant to
    // exclude. Measured before the fix: 1.85 MiB reported against 3.09 MiB
    // held on a populated scrollback.
    let grid_after = parser.grid().retained_amount().bytes;
    assert!(
        grid_after > grid_before,
        "linked cells allocate grid-owned attribute boxes, which the grid must count"
    );

    // Disjointness, asserted directly rather than through a proxy: the grid's
    // increase is bounded by what the boxes can cost, so it cannot be
    // restating the registry's string bytes.
    let linked_cells: usize = parser
        .grid()
        .rows_iter()
        .chain(parser.grid().scrollback_iter())
        .map(|row| row.iter().filter(|cell| cell.hyperlink().is_some()).count())
        .sum();
    let max_box_bytes = linked_cells * std::mem::size_of::<sonicterm_types::FatAttributes>();
    assert!(
        grid_after - grid_before <= max_box_bytes,
        "the grid's increase ({}) exceeds what {linked_cells} attribute boxes can cost \
         ({max_box_bytes}) — it is restating registry bytes",
        grid_after - grid_before
    );

    // Therefore the registry's bytes are reachable from exactly one place, and
    // a pane that omits this term under-charges by that much.
    assert!(
        links_after >= 10_000,
        "200 links with long URIs should retain a meaningful amount, got {links_after}"
    );
}

/// Every capture inside the guarantee is admitted; the next one is refused.
///
/// This is the admission boundary itself. The old policy had no boundary — it
/// divided a share and clamped it at a floor, so the `N + 1`th capture was
/// admitted at the floor exactly like the first, and the sum grew without
/// limit. What replaced it must actually stop.
#[test]
fn captures_are_admitted_up_to_the_guarantee_and_refused_past_it() {
    let pool = CaptureStagingPool::new();

    let admitted: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(&pool, MediaProtocol::Kitty, String::new()))
        .collect();

    for (index, capture) in admitted.iter().enumerate() {
        assert!(
            capture.admitted(),
            "capture {index} is inside the guarantee of {GUARANTEED_CONCURRENT_CAPTURES} and \
             must be admitted"
        );
    }

    let refused = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    assert!(
        !refused.admitted(),
        "the capture past the guarantee must be refused, or the ceiling is not a ceiling"
    );

    drop(refused);
    drop(admitted);
}

/// A refused capture must keep no bytes at all.
///
/// Admission is what bounds the total, so a refused capture that still
/// accumulated would make the bound decorative.
#[test]
fn a_refused_capture_stages_nothing() {
    let pool = CaptureStagingPool::new();

    let held: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(&pool, MediaProtocol::Kitty, String::new()))
        .collect();

    let mut refused = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    assert!(!refused.admitted(), "precondition: the pool is committed");

    for byte in b"a-payload-that-must-not-be-staged" {
        refused.append_byte(*byte);
    }

    assert_eq!(refused.data.len(), 0, "a refused capture must keep no bytes");
    assert_eq!(refused.retained_bytes(), 0, "and hold no allocation");
    assert!(refused.truncated, "and know that it dropped what it was given");

    drop(refused);
    drop(held);
}

/// Releasing a capture must return its bytes to the pools.
///
/// A pool that is never returned to would refuse everything after the first
/// burst of captures, which is a permanent degradation rather than a bound.
#[test]
fn releasing_a_capture_returns_its_pool_bytes() {
    let pool = CaptureStagingPool::new();

    let held: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(&pool, MediaProtocol::Kitty, String::new()))
        .collect();
    assert!(
        !MediaCapture::new(&pool, MediaProtocol::Kitty, String::new()).admitted(),
        "precondition: the pool is committed"
    );

    drop(held);

    let after = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    assert!(after.admitted(), "a capture must be admitted once the pool is released");
}

/// A capture must grow to the per-capture maximum when the pool allows.
///
/// The growth path is what keeps the common case whole, and it runs only when
/// a capture fills its floor. A fix that bounded the total by never growing
/// would pass every ceiling test and quietly cap every image at the floor.
#[test]
fn an_admitted_capture_grows_to_the_per_capture_maximum() {
    let pool = CaptureStagingPool::new();

    let mut capture = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    assert!(capture.admitted(), "precondition: admitted into a free pool");

    for _ in 0..MAX_MEDIA_PAYLOAD_BYTES {
        capture.append_byte(b'A');
    }

    assert_eq!(
        capture.data.len(),
        MAX_MEDIA_PAYLOAD_BYTES,
        "a capture growing alone must reach the per-capture maximum"
    );
    assert!(!capture.truncated, "and must not report truncation at the maximum");
}

/// Growth must stop at the per-capture maximum.
///
/// The floor keeps small images whole; this keeps one capture from taking the
/// whole growth pool and then continuing past the cap the app charges panes
/// against.
#[test]
fn growth_stops_at_the_per_capture_maximum() {
    let pool = CaptureStagingPool::new();

    let mut capture = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    for _ in 0..(MAX_MEDIA_PAYLOAD_BYTES + 4096) {
        capture.append_byte(b'A');
    }

    assert_eq!(capture.data.len(), MAX_MEDIA_PAYLOAD_BYTES, "growth must stop at the maximum");
    assert!(capture.truncated, "and past it the capture must know it was cut");
    assert!(
        capture.retained_bytes() <= MAX_MEDIA_PAYLOAD_BYTES + capture.metadata.capacity(),
        "the allocation must not exceed the budget the pools granted: held {}",
        capture.retained_bytes()
    );
}

/// Captures alive at the same time must contend for the same pools.
///
/// This is what fails if the reservation is never taken: with nothing charged
/// to the pools, every capture is admitted and claims the full 16 MiB, which
/// is exactly the 20 × 16 MiB = 320 MiB composition this package exists to
/// close.
#[test]
fn concurrent_captures_contend_for_the_same_pools() {
    let pool = CaptureStagingPool::new();

    let held: Vec<MediaCapture> =
        (0..32).map(|_| MediaCapture::new(&pool, MediaProtocol::Kitty, String::new())).collect();

    let admitted = held.iter().filter(|capture| capture.admitted()).count();
    assert_eq!(
        admitted, GUARANTEED_CONCURRENT_CAPTURES,
        "32 concurrent captures must be admitted up to the guarantee and no further"
    );

    let staged: usize = held.iter().map(MediaCapture::retained_bytes).sum();
    assert!(
        staged <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "32 concurrent captures hold {staged} bytes against a ceiling of \
         {MAX_PROCESS_CAPTURE_STAGING_BYTES}"
    );

    drop(held);
}

/// Ending a capture must return its share to everyone else.
///
/// This is what fails if the charge is taken but never released — including if
/// `into_kitty_event`/`into_event` stopped dropping the guard when they move
/// the payload out. A leak there is invisible in any single-capture test: the
/// budget only misbehaves once the phantom count accumulates.
///
/// The count is read from a private pool, so it is attributable to this parser
/// alone, and a fresh pool starts it at zero.
#[test]
fn a_finished_capture_returns_its_share() {
    let pool = CaptureStagingPool::new();
    let baseline = pool.live_captures();
    assert_eq!(baseline, 0, "a fresh private pool must hold no captures; it holds {baseline}");

    // Drive real captures to completion through the parser, so the release
    // path under test is the production one rather than a bare `drop`.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    for _ in 0..64 {
        parser.advance(b"\x1b_Gf=100;payload\x1b\\");
    }
    assert_eq!(parser.live_capture_count(), 0, "precondition: no capture left in flight");

    let after = pool.live_captures();
    assert_eq!(
        after, baseline,
        "64 completed captures left the live count at {after} instead of {baseline}; a capture \
         that ends must release its share or the budget decays permanently"
    );
}

/// Arriving captures must be bounded by what the pools have left.
///
/// The measurement that motivated this package fed 20 parsers an unterminated
/// APC introducer plus 16 MiB and found 320 MiB pinned, every parser
/// individually compliant. This is that measurement, asserted.
///
/// Each parser is fed once and then never again — the stalled shape, which is
/// the one no growth policy can fix, because a stalled capture never runs
/// again to be re-budgeted. Admission is what bounds it: a capture that the
/// pools cannot floor is refused at birth rather than admitted at a size that
/// would put the total over.
#[test]
fn arriving_captures_are_bounded_by_what_the_pools_have_left() {
    let pool = CaptureStagingPool::new();
    const PANES: usize = 20;

    let mut parsers: Vec<Parser> = (0..PANES)
        .map(|_| Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone()))
        .collect();

    // An APC introducer with no terminator: the stalled-transfer shape.
    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 3, b'A');

    for parser in parsers.iter_mut() {
        parser.advance(&chunk);
    }

    // Every parser is still mid-capture — otherwise any total below would be
    // small for the wrong reason.
    for (idx, parser) in parsers.iter().enumerate() {
        assert_eq!(parser.live_capture_count(), 1, "parser {idx} must still hold its capture");
    }

    // Against the ceiling itself, not against the naive product. `total <
    // MAX_MEDIA_PAYLOAD_BYTES * PANES` is satisfied by any policy that divides
    // at all, including one whose sum grows without limit — it was what this
    // assertion said while the process held 80 MiB against a stated 64 MiB.
    let total: usize = parsers.iter().map(|parser| parser.retained_amount().bytes).sum();
    assert!(
        total <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "{PANES} stalled captures hold {} MiB against a ceiling of {} MiB",
        total / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024)
    );

    // And the ones past the guarantee took nothing at all, rather than a floor
    // the ceiling could not back.
    let staged = parsers.iter().filter(|parser| parser.retained_amount().bytes > 0).count();
    assert!(
        staged <= GUARANTEED_CONCURRENT_CAPTURES,
        "{staged} captures were staged, but the pools can only guarantee \
         {GUARANTEED_CONCURRENT_CAPTURES}"
    );
}

/// A stalled capture is reclaimed by the host, not by the parser.
///
/// The parser has no clock and cannot tell a stalled transfer from a slow one.
/// It exposes progress so the host can tell, and cancellation so the host can
/// act. Neither half is useful alone, so both are asserted together.
#[test]
fn a_stalled_capture_is_visible_as_progress_and_releasable_by_cancel() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 3, b'A');
    parser.advance(&chunk);

    assert_eq!(parser.live_capture_count(), 1, "precondition: a capture is in flight");
    let held = parser.retained_amount().bytes;
    assert!(held > 0, "precondition: it is holding staging");

    // Two samples with no bytes in between: this is how a host distinguishes a
    // stalled capture from a slow one. Equal readings mean nothing arrived.
    let first = parser.capture_progress();
    let second = parser.capture_progress();
    assert_eq!(first, second, "progress must not move when no bytes arrive");
    assert!(first > 0, "progress must have advanced while bytes were arriving");

    // Feeding more must move it, or a host would cancel live transfers.
    parser.advance(b"BBBB");
    assert!(
        parser.capture_progress() > second,
        "progress must advance when bytes arrive, or a slow transfer reads as stalled"
    );

    // Having judged it stalled, the host cancels and gets the allocation back.
    let released = parser.cancel_capture();
    assert!(released > 0, "cancelling a live capture must release its staging");
    assert_eq!(parser.live_capture_count(), 0, "the capture must be gone");
    assert_eq!(parser.retained_amount().bytes, 0, "and its bytes with it");

    // Cancelling again is harmless — a host polling on a timer will do this.
    assert_eq!(parser.cancel_capture(), 0, "cancelling with nothing in flight must be a no-op");

    // The parser must still work afterwards; cancel resets state, not the
    // session. The abandoned transfer's tail is swallowed first — printing it
    // would put megabytes of base64 on the user's screen — so the parser
    // resumes at the newline that marks the end of that tail.
    parser.advance(b"tail-of-the-abandoned-payload\nhello");
    assert!(row_text(&parser, 1).starts_with("hello"), "the parser must still print after cancel");
    assert!(
        !row_text(&parser, 0).contains("tail-of-the-abandoned-payload"),
        "but the abandoned tail must not reach the screen"
    );
}

/// Cancelling must not dispatch the partial payload.
///
/// A fragment of an image decodes to nothing useful, so surfacing it would
/// trade memory for a broken picture rather than no picture.
#[test]
fn cancelling_a_capture_emits_no_media_event() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    parser.advance(b"\x1b_Gf=100;partial-payload-with-no-terminator");
    assert_eq!(parser.live_capture_count(), 1, "precondition: capture in flight");

    parser.cancel_capture();

    let events = parser.advance(b"x");
    assert!(
        !events.iter().any(|event| matches!(event, VtEvent::Media(_))),
        "a cancelled capture must not surface a truncated media event"
    );
}

/// Interleaved captures must hold the ceiling without help from the host.
///
/// Round-robin is the shape of real concurrent transfers. The bound asserted
/// here is the ceiling itself; it used to be
/// `MAX_PROCESS_CAPTURE_STAGING_BYTES.max(MIN_CAPTURE_STAGING_BYTES * PANES)`,
/// which grows with the pane count and so accommodated exactly the
/// unboundedness it looked like it was checking. A bound that widens to fit
/// the measurement is not a bound.
#[test]
fn interleaved_captures_hold_the_ceiling_without_a_reclaim_pass() {
    let pool = CaptureStagingPool::new();
    const PANES: usize = 20;
    const BLOCK: usize = 256 * 1024;

    let mut parsers: Vec<Parser> = (0..PANES)
        .map(|_| Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone()))
        .collect();

    for parser in parsers.iter_mut() {
        parser.advance(b"\x1b_G");
    }

    let block = vec![b'A'; BLOCK];
    for _ in 0..(MAX_MEDIA_PAYLOAD_BYTES / BLOCK) {
        for parser in parsers.iter_mut() {
            parser.advance(&block);
        }
    }

    let total: usize = parsers.iter().map(|parser| parser.retained_amount().bytes).sum();
    assert!(
        total <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "interleaved captures hold {} MiB against a ceiling of {} MiB",
        total / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024)
    );
}

/// A pane receiving a large image must get it whole.
///
/// The first operating principle is to give the user what they asked for. A
/// bound that quietly truncates an ordinary image would be a regression
/// dressed as a fix.
///
/// Sized against the floor rather than the per-capture maximum, because the
/// floor is what an admitted pane is guaranteed regardless of contention, and
/// guaranteeing it is the point.
#[test]
fn a_pane_receives_a_payload_up_to_the_guaranteed_floor_whole() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    let payload_len = MIN_CAPTURE_STAGING_BYTES;
    let mut chunk = Vec::with_capacity(payload_len + 16);
    chunk.extend_from_slice(b"\x1b_Gf=100;");
    chunk.resize(payload_len, b'A');
    chunk.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&chunk);
    let media = events
        .iter()
        .find_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .expect("a terminated kitty sequence must produce a media event");

    assert!(
        media.data.len() >= payload_len - 16,
        "a payload at the guaranteed floor must arrive whole: got {} of {payload_len} bytes",
        media.data.len()
    );
}

/// A capture cut by the per-capture maximum must not be dispatched.
///
/// A payload larger than a capture may hold arrives missing its tail, and the
/// tail is the rest of the image. Base64 protocols fail to decode it outright;
/// Sixel paints the fraction that arrived and reports it as a whole, shorter
/// picture. Neither is the image the user asked for, so neither is surfaced.
#[test]
fn a_payload_past_the_per_capture_maximum_is_not_dispatched() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 64);
    chunk.extend_from_slice(b"\x1b_Gf=100;");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 32, b'A');
    chunk.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&chunk);

    assert!(
        !events.iter().any(|event| matches!(event, VtEvent::Media(_))),
        "a payload past the per-capture maximum must not surface a cut-off picture"
    );
}

// `MediaCapture` must not be `Clone`: a copy would duplicate staged bytes that
// its cloned reservation never covered. Two blanket impls make the path below
// ambiguous, so this fails to compile as soon as `MediaCapture` implements `Clone`.
const _: fn() = || {
    trait AmbiguousIfClone<Marker> {
        fn check() {}
    }
    impl<T> AmbiguousIfClone<()> for T {}
    #[allow(dead_code)]
    struct ImplementsClone;
    impl<T: Clone> AmbiguousIfClone<ImplementsClone> for T {}
    let _ = <MediaCapture as AmbiguousIfClone<_>>::check;
};

/// A lone pane is entitled to the full per-capture maximum, not merely the
/// floor.
///
/// It runs on a private pool because it asserts the uncontended outcome: a
/// capture holding that pool's growth would legitimately lower it. It stays
/// separate from the floor test above so each claim is checked on its own pool.
#[test]
fn a_lone_capture_is_entitled_to_the_full_per_capture_maximum() {
    let pool = CaptureStagingPool::new();

    let mut capture = MediaCapture::new(&pool, MediaProtocol::Kitty, String::new());
    assert!(capture.admitted(), "a lone capture must be admitted");
    assert_eq!(pool.live_captures(), 1, "precondition: this is the only capture in the pool");

    for _ in 0..MAX_MEDIA_PAYLOAD_BYTES {
        capture.append_byte(b'A');
    }

    assert_eq!(
        capture.data.len(),
        MAX_MEDIA_PAYLOAD_BYTES,
        "a lone capture must be able to grow to the full per-capture maximum"
    );
    assert!(!capture.truncated, "and must not be truncated on the way there");
}

/// Cancelling a stalled capture must not turn the rest of the transfer into
/// text on the user's screen.
///
/// The host cancels a capture whose byte count stopped moving, but the sender
/// is not told, so the remaining payload keeps arriving. Base64 is entirely
/// within the printable-ASCII fast path, so a parser returned to ground with
/// nothing set to swallow the tail prints megabytes of it into the grid: the
/// user gets no image *and* a destroyed screen.
#[test]
fn cancelled_capture_does_not_print_its_remaining_payload() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());

    // A Kitty transfer that has begun but not terminated.
    parser.advance(b"\x1b_Gf=100,a=T;AAAABBBBCCCC");
    assert_eq!(parser.live_capture_count(), 1, "capture must be in flight");

    // The host observes no progress and reclaims the staging.
    parser.cancel_capture();

    // The sender knows nothing about the cancellation and keeps streaming.
    parser.advance(b"DDDDEEEEFFFF\x1b\\");

    assert_eq!(
        row_text(&parser, 0),
        "                ",
        "the abandoned payload must not be printed as text"
    );
}

/// A sender that *died* mid-transfer sends no terminator, so the discard armed
/// by cancellation has to end on its own or the pane is wedged: the shell's
/// next prompt would be swallowed forever. A newline is the discriminator —
/// media payloads carry none, shell output is full of them.
#[test]
fn cancelled_capture_stops_discarding_at_the_first_newline() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 3), None, pool.clone());

    parser.advance(b"\x1b_Gf=100,a=T;AAAABBBB");
    parser.cancel_capture();

    // No terminator ever comes. The shell prints a prompt instead.
    parser.advance(b"CCCC\nuser@host$ ");

    assert_eq!(row_text(&parser, 0), "                ", "the tail is still swallowed");
    assert_eq!(
        row_text(&parser, 1),
        "user@host$      ",
        "but output after the newline must reach the user"
    );
}

#[test]
fn a_decrqss_query_is_not_captured_as_a_sixel_image() {
    let pool = CaptureStagingPool::new();
    // `q` is the final byte of three unrelated DCS sequences, told apart only
    // by their intermediate:
    //
    //   Sixel      DCS <params> q      no intermediate
    //   DECRQSS    DCS $ q             intermediate 0x24
    //   XTGETTCAP  DCS + q             intermediate 0x2B
    //
    // Classifying on the final byte alone captures a terminal *query* as
    // image data. nvim sends DECRQSS on startup to read the current SGR, and
    // its payload byte `m` lands in the Sixel data range `?`..=`~`, so the
    // decoder renders it: `bits = 0x6D - 63 = 0x2E`, setting rows 1, 2, 3 and
    // 5 of one six-row column. That is a 1x6 image, and with no colour
    // introducer it takes the decoder's default palette entry — pure white.
    //
    // The result is a stray white pixel at the pane content origin, redrawn
    // every frame from retained media, surviving every repaint path.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(b"\x1bP$qm\x1b\\");

    let media: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();

    assert!(
        media.is_empty(),
        "DECRQSS (DCS $ q) is a status query, not an image; capturing it as \
         Sixel renders the reply payload as pixels: {media:?}"
    );
}

#[test]
fn an_xtgetcap_query_is_not_captured_as_a_sixel_image() {
    let pool = CaptureStagingPool::new();
    // The other `q`-terminated query nvim sends. Its hex-digit payload mostly
    // falls outside the Sixel data range, so it decodes to nothing visible
    // more often than DECRQSS does — but it is still a query being read as an
    // image, and `b`..`f` in a hex string are inside the range.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(b"\x1bP+q626365\x1b\\");

    let media: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();

    assert!(media.is_empty(), "XTGETTCAP (DCS + q) is a capability query, not an image: {media:?}");
}

#[test]
fn a_real_sixel_without_intermediates_is_still_captured() {
    let pool = CaptureStagingPool::new();
    // The other half of the fix: rejecting intermediates must not reject
    // actual Sixel, which carries parameters but no intermediate byte.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(b"\x1bP0;0;0q#0;2;0;0;0#0~~@@vv@@~~@@~~$\x1b\\");

    let media = events
        .into_iter()
        .find_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .expect("a genuine Sixel payload must still be captured");
    assert_eq!(media.protocol, MediaProtocol::Sixel);
}

#[test]
fn a_decscusr_query_reply_is_not_captured_as_a_sixel_image() {
    let pool = CaptureStagingPool::new();
    // The defect is not specific to one query. DECSCUSR — `DCS $ q SP q ST`,
    // the cursor-shape request — carries the same `$` intermediate, and its
    // reply byte `q` (0x71) is itself inside the Sixel data range: `bits =
    // 0x71 - 63 = 0b110010` would paint rows 1, 4 and 5 of a six-row column.
    //
    // Any `$`-intermediate query whose reply lands in `?`..=`~` produces a
    // mark, so the fix has to gate on the intermediate rather than enumerate
    // the queries that happen to be known.
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(b"\x1bP$q q\x1b\\");

    let media: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .collect();

    assert!(
        media.is_empty(),
        "DECSCUSR (DCS $ q SP q) is a cursor-shape query, not an image: {media:?}"
    );
}

/// Parsers built by the default constructors stage in the one process-default
/// pool; the injecting constructor stages in the pool it is given.
#[test]
fn default_constructors_share_the_process_staging_pool() {
    let default = CaptureStagingPool::process_default();
    let (tx, _rx) = crossbeam_channel::unbounded();
    let plain = Parser::new(Grid::new(8, 2));
    let replying = Parser::new_with_reply(Grid::new(8, 2), tx);
    assert!(Arc::ptr_eq(&plain.performer.staging_pool, &default));
    assert!(Arc::ptr_eq(&replying.performer.staging_pool, &default));

    let private = CaptureStagingPool::new();
    let injected = Parser::new_with_staging_pool(Grid::new(8, 2), None, private.clone());
    assert!(Arc::ptr_eq(&injected.performer.staging_pool, &private));
}

/// `Parser::live_capture_count` stays parser-local while the pool observer
/// counts every capture held by the parsers that share the pool.
#[test]
fn live_capture_count_is_per_parser_while_the_pool_counts_all_sharers() {
    let pool = CaptureStagingPool::new();
    let mut sixel = Parser::new_with_staging_pool(Grid::new(8, 2), None, pool.clone());
    let mut kitty = Parser::new_with_staging_pool(Grid::new(8, 2), None, pool.clone());

    sixel.advance(b"\x1bPq#");
    kitty.advance(b"\x1b_GA");
    assert_eq!((sixel.live_capture_count(), kitty.live_capture_count()), (1, 1));
    assert_eq!(pool.live_captures(), 2, "the pool counts both parsers' captures");

    sixel.advance(b"\x1b\\");
    assert_eq!(sixel.live_capture_count(), 0);
    assert_eq!(pool.live_captures(), 1, "only the kitty capture is still live");
}
