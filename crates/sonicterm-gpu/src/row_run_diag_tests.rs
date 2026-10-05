use super::*;

/// The key of test text number `number` in the normal style, its hash forced to `hash`.
fn key(hash: u64, number: u32) -> RowRunKey {
    RowRunKey { hash, len: number, style: 0 }
}

/// A face identity that stays put across a call.
const FACE: FaceKey = (7, 3);

/// One stable successful call of `key` taking `ns`.
fn call(table: &mut RowRunTable, key: RowRunKey, ns: u64) -> Option<RunClass> {
    table.observe_call(key, FACE, FACE, true, ns)
}

/// Run one pass of `keys` and settle it, committed when `presented`; returns each call's class.
fn pass(table: &mut RowRunTable, keys: &[RowRunKey], presented: bool) -> Vec<Option<RunClass>> {
    table.begin_pass();
    let classes = keys.iter().map(|key| call(table, *key, 10)).collect();
    table.end_pass(presented);
    classes
}

/// The storage layout the accounting figure rests on: 48 B slots, 24 B pending records and
/// 16 B style identities, and 4,096 × 48 + 8,192 × 24 = 393,216 B of heap once allocated.
#[test]
fn storage_sizes_and_the_retained_heap_figure() {
    assert_eq!(std::mem::size_of::<Slot>(), 48);
    assert_eq!(std::mem::size_of::<PendingRecord>(), 24);
    assert_eq!(std::mem::size_of::<StyleIdentity>(), 16);
    let mut table = RowRunTable::new();
    assert_eq!(table.retained_heap_bytes(), 0, "nothing is allocated before a counted call");
    pass(&mut table, &[key(1, 1)], true);
    assert_eq!(table.retained_heap_bytes(), 393_216);
}

/// A key shaped twice in one pass is a same-pass repeat, counted as a repeat when it commits.
#[test]
fn a_key_shaped_twice_in_a_pass_is_a_same_pass_repeat() {
    let mut table = RowRunTable::new();
    let classes = pass(&mut table, &[key(5, 1), key(5, 1)], true);
    assert_eq!(classes, [Some(RunClass::First), Some(RunClass::SamePass)]);
    let counts = table.take_counts();
    assert_eq!((counts.first, counts.repeats, counts.same_pass_repeats), (1, 1, 1));
}

/// A committed sighting stays in the window while fewer than eight committed passes followed it:
/// at distance 7 (seven committed passes since) the key repeats, at distance 8 it is first again.
#[test]
fn the_window_holds_distance_seven_and_not_eight() {
    for (distance, expected) in [(7u32, RunClass::Window), (8, RunClass::First)] {
        let mut table = RowRunTable::new();
        pass(&mut table, &[key(5, 1)], true);
        for other in 0..distance {
            pass(&mut table, &[key(100 + u64::from(other), 2)], true);
        }
        let classes = pass(&mut table, &[key(5, 1)], true);
        assert_eq!(classes, [Some(expected)], "distance {distance}");
    }
}

/// The retry sequence: a unique call in a rejected pass, the same call in the successful retry
/// (a retry repeat, not a window repeat), a later ordinary repeat, and unrelated work in a failed
/// pass that stays unpresented only.
#[test]
fn a_retried_call_is_a_retry_repeat_then_an_ordinary_repeat() {
    let mut table = RowRunTable::new();
    assert_eq!(pass(&mut table, &[key(9, 1)], false), [Some(RunClass::First)]);
    assert_eq!(pass(&mut table, &[key(9, 1)], true), [Some(RunClass::Retry)]);
    assert_eq!(pass(&mut table, &[key(9, 1)], true), [Some(RunClass::Window)]);
    pass(&mut table, &[key(77, 3)], false);
    let counts = table.take_counts();
    assert_eq!(counts.retry_repeats, 1);
    assert_eq!(counts.repeats, 1, "only the later ordinary repeat is a repeat");
    assert_eq!(counts.first, 0, "the rejected first sighting never committed");
    assert_eq!((counts.unpresented_calls, counts.unpresented_ns), (2, 20));
}

/// A partial-fallback reassembly is a second pass: the first is superseded when the second
/// begins, so it settles uncommitted, and the retried call in the second is a retry repeat.
#[test]
fn a_superseded_first_pass_settles_as_not_presented() {
    let mut table = RowRunTable::new();
    table.begin_pass();
    call(&mut table, key(4, 1), 10);
    table.begin_pass();
    assert_eq!(call(&mut table, key(4, 1), 10), Some(RunClass::Retry));
    table.end_pass(true);
    let counts = table.take_counts();
    assert_eq!((counts.unpresented_calls, counts.retry_repeats), (1, 1));
}

/// A skipped attempt opens no pass, so it does not advance the window: settling without an open
/// pass changes nothing.
#[test]
fn a_skipped_attempt_does_not_advance_the_window() {
    let mut table = RowRunTable::new();
    pass(&mut table, &[key(5, 1)], true);
    for _ in 0..20 {
        table.end_pass(true);
    }
    assert_eq!(pass(&mut table, &[key(5, 1)], true), [Some(RunClass::Window)]);
}

/// A failed or unstable call never touches the table. The unstable call adopts the merged face, so
/// a later call that reads the old face first is a transition too: it clears the style again and
/// its key, committed before, is first.
#[test]
fn failed_and_unstable_calls_touch_nothing_and_a_face_change_clears_the_style() {
    let mut table = RowRunTable::new();
    pass(&mut table, &[key(5, 1)], true);
    table.begin_pass();
    assert_eq!(table.observe_call(key(5, 1), FACE, FACE, false, 10), None);
    assert_eq!(table.observe_call(key(5, 1), FACE, (7, 4), true, 10), None);
    assert_eq!(
        call(&mut table, key(5, 1), 10),
        Some(RunClass::First),
        "the style was cleared again"
    );
    table.end_pass(true);
    let counts = table.take_counts();
    assert_eq!((counts.calls, counts.ok, counts.failed, counts.unstable), (4, 3, 1, 1));
    assert_eq!(counts.identity_resets, 2);
}

/// A key shaped before a fallback merge and shaped again after it, stably, is a new key: the
/// merge reset the style, so the earlier entry is dead.
#[test]
fn a_key_committed_before_a_merge_is_not_a_repeat_after_it() {
    let mut table = RowRunTable::new();
    pass(&mut table, &[key(5, 1)], true);
    table.begin_pass();
    table.observe_call(key(6, 2), FACE, (7, 4), true, 10);
    assert_eq!(table.observe_call(key(5, 1), (7, 4), (7, 4), true, 10), Some(RunClass::First));
    table.end_pass(true);
}

/// Insertion never displaces a live entry: with all eight candidates holding recently committed
/// keys, a ninth key overflows, is classified first and timed, and records nothing.
#[test]
fn a_ninth_key_overflows_when_all_candidates_are_live() {
    let mut table = RowRunTable::new();
    let keys: Vec<RowRunKey> = (0..8).map(|number| key(0, number + 1)).collect();
    pass(&mut table, &keys, true);
    assert_eq!(pass(&mut table, &[key(0, 99)], true), [Some(RunClass::First)]);
    let counts = table.take_counts();
    assert_eq!(counts.overflows, 1);
    assert_eq!(counts.first, 9, "the overflowed call still counts as first");
    assert_eq!(
        pass(&mut table, &keys, true),
        vec![Some(RunClass::Window); 8],
        "no live entry was displaced"
    );
}

/// A failing pass never displaces a live entry either: its overflowed key leaves every committed
/// key in place.
#[test]
fn a_failing_pass_never_displaces_a_live_entry() {
    let mut table = RowRunTable::new();
    let keys: Vec<RowRunKey> = (0..8).map(|number| key(0, number + 1)).collect();
    pass(&mut table, &keys, true);
    pass(&mut table, &[key(0, 99)], false);
    assert_eq!(pass(&mut table, &keys, true), vec![Some(RunClass::Window); 8]);
}

/// A slot freed before a key in its probe range does not hide that key: the whole range is
/// scanned, so the later key is still found.
#[test]
fn a_cleared_hole_does_not_hide_a_later_key() {
    let mut table = RowRunTable::new();
    let (earlier, later) = (RowRunKey { hash: 0, len: 1, style: 1 }, key(0, 2));
    table.begin_pass();
    table.observe_call(earlier, FACE, FACE, true, 10);
    call(&mut table, later, 10);
    table.end_pass(true);
    // A face change for style 1 kills the earlier key's slot, leaving a hole before the later key.
    table.begin_pass();
    table.observe_call(RowRunKey { hash: 3, len: 9, style: 1 }, FACE, (8, 1), true, 10);
    assert_eq!(call(&mut table, later, 10), Some(RunClass::Window));
    table.end_pass(true);
}

/// Ten thousand distinct keys over many committed passes keep the table at 4,096 slots.
#[test]
fn ten_thousand_keys_keep_four_thousand_ninety_six_slots() {
    let mut table = RowRunTable::new();
    for chunk in 0..100u64 {
        let keys: Vec<RowRunKey> = (0..100).map(|number| key(chunk * 100 + number, 1)).collect();
        pass(&mut table, &keys, true);
    }
    assert_eq!(table.slot_capacity(), SLOT_COUNT);
    assert_eq!(table.retained_heap_bytes(), 393_216);
}

/// A full pending list refuses further records but keeps the earlier ones: they settle normally
/// and no pending marker survives the pass end.
#[test]
fn a_full_pending_list_keeps_earlier_records_and_leaves_no_marker() {
    let mut table = RowRunTable::new();
    table.begin_pass();
    for _ in 0..PENDING_CAPACITY + 1 {
        call(&mut table, key(5, 1), 1);
    }
    table.end_pass(true);
    let counts = table.take_counts();
    assert_eq!(counts.pass_overflows, 1);
    assert_eq!(
        counts.first + counts.repeats,
        PENDING_CAPACITY as u64 + 1,
        "every call is classified"
    );
    assert_eq!(table.pending_markers(), 0);
    assert_eq!(
        pass(&mut table, &[key(5, 1)], true),
        [Some(RunClass::Window)],
        "the record settled"
    );
}

/// A slot whose generation would wrap clears the whole table, so no earlier key can match.
#[test]
fn a_wrapping_generation_clears_the_table() {
    let mut table = RowRunTable::new();
    pass(&mut table, &[key(0, 1)], true);
    table.set_generation(1, u32::MAX);
    pass(&mut table, &[key(1, 2)], true);
    assert_eq!(
        pass(&mut table, &[key(0, 1)], true),
        [Some(RunClass::First)],
        "the clear dropped the earlier key"
    );
}

/// A style epoch that would wrap clears the whole table and restarts the epochs at 1. The entries
/// committed earlier were written at epoch 1 too, so only the clear keeps them from matching; the
/// wrap is the only transition, every call reading the merged face before and after.
#[test]
fn a_wrapping_epoch_clears_the_table() {
    let mut table = RowRunTable::new();
    pass(&mut table, &[key(0, 1), key(2, 2)], true);
    table.set_epoch(0, u32::MAX);
    let merged = (8, 1);
    table.begin_pass();
    assert_eq!(table.observe_call(key(4, 3), merged, merged, true, 10), Some(RunClass::First));
    assert_eq!(table.identities[0].epoch, 1);
    assert_eq!(
        table.observe_call(key(2, 2), merged, merged, true, 10),
        Some(RunClass::First),
        "the clear dropped the epoch-1 entry committed before the wrap"
    );
    table.end_pass(true);
    assert_eq!(table.take_counts().identity_resets, 1);
}

/// Two different texts forced to one key count as a repeat: the key is the hash, length and style.
#[test]
fn two_texts_with_one_key_count_as_a_repeat() {
    let mut table = RowRunTable::new();
    let forced = RowRunKey { hash: row_run_key("甲", false, false).hash, len: 3, style: 0 };
    pass(&mut table, &[forced], true);
    assert_eq!(pass(&mut table, &[forced], true), [Some(RunClass::Window)]);
    assert_ne!(
        row_run_key("甲", false, false),
        row_run_key("甲", true, false),
        "bold is part of the key"
    );
}

thread_local! {
    /// The injected clock's next reading, advanced 100 ns per read.
    static CLOCK_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A clock that advances 100 ns on every reading.
fn stepping_clock() -> u64 {
    CLOCK_NS.with(|clock| {
        let reading = clock.get();
        clock.set(reading + 100);
        reading
    })
}

/// With an injected clock that advances 100 ns per reading, each call's shaping time is the
/// 100 ns between its two shaping readings and is attributed to its class: the first call's to
/// `shape_ns`, the repeat's also to `repeat_ns`; the hashing, identity and settlement readings
/// go to `diag_ns`.
#[test]
fn an_injected_clock_attributes_time_to_the_right_class() {
    CLOCK_NS.with(|clock| clock.set(0));
    let mut diagnostics = RowRunDiagnostics::with_clock(stepping_clock);
    diagnostics.begin_pass(true);
    for _ in 0..2 {
        let shaped: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
        assert!(shaped.is_ok());
    }
    diagnostics.end_pass(true);
    let counts = diagnostics.take_counts();
    assert_eq!((counts.first, counts.repeats, counts.same_pass_repeats), (1, 1, 1));
    assert_eq!(counts.shape_ns, 200);
    assert_eq!(counts.repeat_ns, 100);
    // Each call reads 4 times: 100 ns before shaping, 100 after; the settlement reads twice.
    assert_eq!(counts.diag_ns, 2 * 200 + 100);
}

/// With the gate off a pass counts nothing and allocates nothing; the shape closure still runs.
#[test]
fn a_pass_with_the_gate_off_records_nothing() {
    let mut diagnostics = RowRunDiagnostics::new();
    diagnostics.begin_pass(false);
    let shaped: Result<u8, ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(7));
    assert_eq!(shaped, Ok(7));
    diagnostics.end_pass(true);
    assert_eq!(diagnostics.take_counts(), RowRunCounts::default());
    assert_eq!(diagnostics.table().retained_heap_bytes(), 0);
}

/// An attempt that unwinds mid-assembly settles its open pass as not presented before its scopes
/// close: read straight after the attempt, with no later pass begun, the collector already holds
/// the call as unpresented with its shaping and diagnostic time, and the table has no open pass.
#[test]
fn an_unwinding_attempt_settles_its_pass_before_the_attempt_closes() {
    CLOCK_NS.with(|clock| clock.set(0));
    let mut diagnostics = RowRunDiagnostics::with_clock(stepping_clock);
    let sink = crate::frame_stats::FrameStatsSink::default();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut owed_apply = false;
        let _scope = crate::frame_stats::RenderScope::enter(Some(&sink), &mut owed_apply);
        let attempt = catch_attempt(|| {
            diagnostics.begin_gated_pass();
            let _: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
            panic!("assembly unwinds");
        });
        diagnostics.settle_attempt(attempt)
    }));
    assert!(unwound.is_err());
    let stats = sink.snapshot();
    assert_eq!(stats.attempts.attempts, 1, "the attempt closed");
    let counts = stats.row_runs;
    assert_eq!((counts.calls, counts.unpresented_calls, counts.first), (1, 1, 0));
    // 100 ns between the shaping readings; 200 ns of hashing and recording plus 100 ns settling.
    assert_eq!((counts.unpresented_ns, counts.shape_ns, counts.diag_ns), (100, 0, 300));
    assert!(!diagnostics.table().is_open(), "nothing is left for a later pass to settle");
}

/// An attempt that fails (returns without presenting) settles its pass as unpresented within the
/// attempt; a following attempt whose pass presents settles it once, as committed, and the
/// epilogue never settles a committed pass a second time.
#[test]
fn a_failed_attempt_settles_unpresented_and_a_presented_one_is_settled_once() {
    let mut diagnostics = RowRunDiagnostics::new();
    let sink = crate::frame_stats::FrameStatsSink::default();
    let mut attempt = |presented: bool| {
        let mut owed_apply = false;
        let _scope = crate::frame_stats::RenderScope::enter(Some(&sink), &mut owed_apply);
        let caught = catch_attempt(|| -> Result<(), &str> {
            diagnostics.begin_gated_pass();
            let _: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
            if !presented {
                // When: the attempt fails, it returns before any settlement of its own.
                return Err("presenter failed");
            }
            diagnostics.end_pass(true);
            crate::frame_stats::note_row_runs(&diagnostics.take_counts());
            Ok(())
        });
        diagnostics.settle_attempt(caught)
    };
    assert!(attempt(false).is_err());
    let failed = sink.snapshot().row_runs;
    assert_eq!((failed.calls, failed.unpresented_calls, failed.first), (1, 1, 0));
    assert!(attempt(true).is_ok());
    let both = sink.snapshot().row_runs;
    assert_eq!(
        (both.calls, both.unpresented_calls),
        (2, 1),
        "the presented call is not unpresented"
    );
    assert_eq!((both.retry_repeats, both.first), (1, 0), "the failed call made the retry sighting");
}

/// The gated pass reads this thread's frame-counter gate: outside a counting scope the pass is
/// not opened, so a shape call counts nothing and allocates nothing; inside one it is counted.
#[test]
fn a_gated_pass_counts_only_inside_a_counting_scope() {
    let mut diagnostics = RowRunDiagnostics::new();
    diagnostics.begin_gated_pass();
    let _: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
    diagnostics.end_pass(true);
    assert_eq!(diagnostics.take_counts(), RowRunCounts::default(), "gate off: nothing counted");
    assert_eq!(diagnostics.retained_items(), 0, "gate off: no table");
    assert_eq!(diagnostics.retained_bytes(), std::mem::size_of::<RowRunDiagnostics>());
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        diagnostics.begin_gated_pass();
    }
    let _: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
    diagnostics.end_pass(true);
    let counts = diagnostics.take_counts();
    assert_eq!((counts.calls, counts.first), (1, 1), "gate on: the call is counted");
    assert_eq!(diagnostics.retained_items(), SLOT_COUNT);
    assert_eq!(diagnostics.retained_bytes(), ROW_RUN_DIAG_ENVELOPE_BYTES);
}

/// A counted pass still open when a gate-off pass begins is settled as not presented, and the
/// gate-off pass then counts nothing: a later shape call is not observed.
#[test]
fn a_pass_left_open_settles_when_the_gate_turns_off() {
    let mut diagnostics = RowRunDiagnostics::new();
    diagnostics.begin_pass(true);
    let _: Result<(), ()> = diagnostics.shape("甲", false, false, || FACE, || Ok(()));
    diagnostics.begin_pass(false);
    let _: Result<(), ()> = diagnostics.shape("乙", false, false, || FACE, || Ok(()));
    diagnostics.end_pass(true);
    let counts = diagnostics.take_counts();
    assert_eq!((counts.calls, counts.unpresented_calls, counts.first), (1, 1, 0));
}
