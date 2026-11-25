use super::*;
use crate::app::osc52_stream::{ClipState, ClipboardStaging};
use crate::app::{App, FrontmostKind};
use sonicterm_cfg::config::{Config, Osc52Policy, Osc52ReadPolicy};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_grid::grid::{CellFlags, Color};
use sonicterm_ui::selection::Selection;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

/// What the fake clipboard holds and records, shared by the test and the worker thread.
#[derive(Default)]
struct FakeState {
    /// The current clipboard text.
    value: String,
    /// The change counter: every write and every external change advances it.
    sequence: u32,
    /// Every text written through the sink, in order.
    writes: Vec<String>,
    /// Writes that reached the native call, including one still held at the gate.
    entered: usize,
    /// A value put back right after the next write, as a failing helper's cleanup does.
    revert_to: Option<String>,
    /// Advance the counter during the next comparison, as a newer write racing it would.
    race_comparison: bool,
    /// Offers published through the sink, each one native call.
    offers: usize,
    /// Refuse the next offers, as a failed `SetClipboardData` does.
    refuse_offer: bool,
}

/// A fake native clipboard; while the test holds `gate`, a write waits inside the native call.
#[derive(Clone, Default)]
struct FakeClipboard {
    state: Arc<Mutex<FakeState>>,
    gate: Arc<Mutex<()>>,
}

impl FakeClipboard {
    /// A fake whose clipboard holds `value`.
    fn holding(value: &str) -> Self {
        let fake = Self::default();
        fake.state().value = value.to_owned();
        fake
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    /// Another program changes the clipboard to `value`.
    fn set_external(&self, value: &str) {
        let mut state = self.state();
        state.value = value.to_owned();
        state.sequence += 1;
    }

    /// Wait until `count` writes have reached the native call.
    fn wait_entered(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.state().entered < count {
            assert!(Instant::now() < deadline, "the worker never reached the native call");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl ClipboardSink for FakeClipboard {
    fn write(&mut self, text: &str, targets: ClipTargets) -> bool {
        self.state().entered += 1;
        let _held = self.gate.lock().unwrap();
        let mut state = self.state();
        state.writes.push(text.to_owned());
        if targets.clipboard {
            state.value = text.to_owned();
            state.sequence += 1;
        }
        if let Some(previous) = state.revert_to.take() {
            state.value = previous;
            state.sequence += 1;
        }
        true
    }

    fn sequence(&mut self) -> Option<u32> {
        Some(self.state().sequence)
    }

    fn publish_offer(&mut self, _offer: &crate::app::kitty_clipboard_offer::NativeOffer) -> bool {
        let mut state = self.state();
        state.offers += 1;
        !state.refuse_offer
    }

    fn snapshot(&mut self, max_block_bytes: usize) -> Option<Snapshot> {
        let state = self.state();
        let mut units: Vec<u16> = state.value.encode_utf16().collect();
        units.shrink_to_fit();
        // A `CF_UNICODETEXT` block holds the units and a terminator; an empty clipboard has none.
        let block_bytes = units.len() * 2 + 2;
        (!units.is_empty() && block_bytes <= max_block_bytes)
            .then_some(Snapshot { block_bytes, units })
    }

    fn holds(&mut self, snapshot: &Snapshot) -> bool {
        let mut state = self.state();
        let units: Vec<u16> = state.value.encode_utf16().collect();
        if std::mem::take(&mut state.race_comparison) {
            state.sequence += 1;
        }
        units.len() * 2 + 2 == snapshot.block_bytes && units == snapshot.units
    }
}

/// A headless App whose test buffer holds `previous`, with a private staging and a worker over
/// `fake` that reasserts after `reassert_delay`.
fn app_with_worker(previous: &str, fake: &FakeClipboard, reassert_delay: Option<Duration>) -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_set_memory_clipboard(previous);
    app.clip_state = ClipState::new(ClipboardStaging::with_budget(64 << 20, 64 << 20));
    let sink = Box::new(fake.clone());
    assert!(app.clip_state.domain.start_worker(sink, reassert_delay, Box::new(|| {})));
    app
}

/// Apply the worker's results until `done` holds.
fn wait_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "the clipboard worker did not settle");
        app.drain_clipboard_completions();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Apply the worker's results until nothing is in flight.
fn settle(app: &mut App) {
    wait_until(app, |app| app.clip_state.domain.in_flight() == 0);
}

/// Select three written cells of row 0 on `pane_id`'s alternate screen, a selection a confirmed
/// copy consumes.
fn select_alt_row(app: &mut App, pane_id: u64) -> winit::window::WindowId {
    let window_id = app.registry.main_window_id.expect("seeded main window");
    let selection = {
        let window = app.registry.windows.get(&window_id).unwrap();
        let mut parser = window.panes.get(&pane_id).unwrap().parser.lock();
        let grid = parser.grid_mut();
        grid.enter_alt_screen();
        grid.goto(0, 0);
        for character in "abc".chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        Selection {
            start: (0, 0),
            end: (0, 3),
            anchored: true,
            pane_id: Some(pane_id),
            content_seq: grid.content_seq(),
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: 0,
            content_fingerprint: None,
        }
        .with_content_fingerprint(grid)
    };
    app.registry.windows.get_mut(&window_id).unwrap().selection = Some(selection);
    window_id
}

/// Two user copies made while a program write holds the worker land after it, oldest first: none
/// is written on the event loop out of order, and each follow-up hears its own result.
#[test]
fn two_user_copies_while_program_write_holds_worker_land_in_order() {
    let fake = FakeClipboard::holding("before");
    let mut app = app_with_worker("before", &fake, None);
    let held = fake.gate.lock().unwrap();
    let job = ClipboardJob::for_test(&app.clip_state.staging, "program");
    assert_eq!(app.submit_program_clipboard(1, job, false), None, "posted to the worker");
    fake.wait_entered(1);
    let results = Rc::new(RefCell::new(Vec::new()));
    for text in ["first", "second"] {
        let results = results.clone();
        let follow_up = move |_: &mut App, written| results.borrow_mut().push((text, written));
        assert!(app.copy_text_then(text.into(), follow_up));
    }
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("before"), "nothing ran inline");
    assert!(results.borrow().is_empty(), "no copy reported before the program write returned");
    drop(held);
    settle(&mut app);
    assert_eq!(fake.state().writes, ["program", "first", "second"]);
    assert_eq!(fake.state().value, "second", "the newest user copy wins");
    assert_eq!(*results.borrow(), [("first", true), ("second", true)]);
}

/// With the user-copy queue full, one more copy is refused before anything is posted: its
/// follow-up hears the refusal at once, nothing reaches the worker, and a refused selection copy
/// keeps its selection.
#[test]
fn user_copy_queue_full_refused_before_post_keeps_selection() {
    let fake = FakeClipboard::holding("before");
    let mut app = app_with_worker("before", &fake, None);
    let pane_id = app.__test_seed_tab("source");
    let held = fake.gate.lock().unwrap();
    let job = ClipboardJob::for_test(&app.clip_state.staging, "program");
    let _ = app.submit_program_clipboard(pane_id, job, false);
    fake.wait_entered(1);
    for _ in 0..MAX_USER_CLIPBOARD_JOBS {
        assert!(app.set_clipboard_text("queued".into()));
    }
    let in_flight = app.clip_state.domain.in_flight();
    let refused = Rc::new(Cell::new(None));
    let seen = refused.clone();
    assert!(!app.copy_text_then("one too many".into(), move |_, written| seen.set(Some(written))));
    assert_eq!(refused.get(), Some(false), "the refusal is reported at once");
    assert_eq!(app.clip_state.domain.in_flight(), in_flight, "nothing more was posted");
    let window_id = select_alt_row(&mut app, pane_id);
    app.copy_selection_for_kind(FrontmostKind::Main);
    assert!(app.registry.windows[&window_id].selection.is_some(), "the selection is kept");
    drop(held);
    settle(&mut app);
    let writes = fake.state().writes.len();
    assert_eq!(writes, 1 + MAX_USER_CLIPBOARD_JOBS, "refused copies never reached the worker");
}

/// A program write's lease and slot stay reserved while the worker's native call runs, and are
/// released when it returns; only Linux keeps the part covering arboard's retained copy.
#[test]
fn lease_held_until_worker_native_call_returns() {
    let fake = FakeClipboard::holding("before");
    let mut app = app_with_worker("before", &fake, None);
    let staging = app.clip_state.staging.clone();
    let job = ClipboardJob::for_test(&staging, "leased text");
    let reserved = staging.reserved_bytes();
    let held = fake.gate.lock().unwrap();
    let _ = app.submit_program_clipboard(1, job, false);
    fake.wait_entered(1);
    assert_eq!(staging.reserved_bytes(), reserved, "the lease is held during the native call");
    assert_eq!(staging.program_slots_in_use(), 1, "and so is the program slot");
    drop(held);
    settle(&mut app);
    let native = native_clipboard_bytes("leased text");
    let retained = if cfg!(target_os = "linux") { native } else { 0 };
    assert_eq!(staging.reserved_bytes(), retained);
    assert_eq!(staging.program_slots_in_use(), 0);
}

/// The event-loop thread never makes a native write past [`INLINE_WRITE_LIMIT_BYTES`]: with no
/// worker a large program write is refused before any native call; with one, every program write
/// and a large user copy go to it, and only a small idle user copy is written here.
#[test]
fn event_loop_never_writes_natively_past_inline_limit() {
    let large = "x".repeat(INLINE_WRITE_LIMIT_BYTES + 1);
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.clipboard = None;
    app.clip_state = ClipState::new(ClipboardStaging::with_budget(64 << 20, 64 << 20));
    let staging = app.clip_state.staging.clone();
    let job = ClipboardJob::for_test(&staging, &large);
    assert_eq!(app.submit_program_clipboard(1, job, false), Some(ClipWriteOutcome::TooLarge));
    assert_eq!((staging.reserved_bytes(), staging.program_slots_in_use()), (0, 0));
    assert!(app.may_write_inline(INLINE_WRITE_LIMIT_BYTES, false));
    assert!(!app.may_write_inline(large.len(), false));
    let user_inline = app.may_write_inline(large.len(), true);
    assert_eq!(user_inline, cfg!(target_os = "macos"), "only a macOS user copy");

    let fake = FakeClipboard::holding("before");
    let mut app = app_with_worker("before", &fake, None);
    assert!(app.set_clipboard_text(large.clone()));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("before"), "posted, not written");
    settle(&mut app);
    let job = ClipboardJob::for_test(&app.clip_state.staging, "small program");
    assert_eq!(
        app.submit_program_clipboard(1, job, false),
        None,
        "a program write is always posted"
    );
    settle(&mut app);
    assert_eq!(fake.state().writes, [large, "small program".to_owned()]);
    assert!(app.set_clipboard_text("small user".into()));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("small user"), "written here");
    assert_eq!(fake.state().writes.len(), 2);
}

/// A program write posted before a reload to `osc52 = "deny"`, or to `"ask"`, which it was never
/// approved under, is refused at the worker's commit point, even after a reload back to allow,
/// while the write already in its native call lands.
#[test]
fn policy_revoked_while_queued_refused_at_worker_commit() {
    for policy in [Osc52Policy::Deny, Osc52Policy::Ask] {
        let fake = FakeClipboard::holding("before");
        let mut app = app_with_worker("before", &fake, None);
        let held = fake.gate.lock().unwrap();
        let first = ClipboardJob::for_test(&app.clip_state.staging, "committed");
        let _ = app.submit_program_clipboard(3, first, false);
        fake.wait_entered(1);
        let second = ClipboardJob::for_test(&app.clip_state.staging, "queued");
        let posted = app.submit_program_clipboard(3, second, false);
        assert_eq!(posted, None, "{policy:?}: posted before the reload");
        let mut reload = Config::default();
        reload.terminal.osc52 = policy;
        app.apply_new_config(reload);
        app.apply_new_config(Config::default());
        drop(held);
        settle(&mut app);
        assert_eq!(fake.state().writes, ["committed"], "{policy:?}: the queued write was refused");
        assert_eq!(app.clip_state.staging.program_slots_in_use(), 0);
    }
}

/// Every reload hands `[terminal] osc52_read` and `osc52` to the staging the VT workers read, so a
/// reload to `deny` silences reads from the next request on, a reload back restores `ask`, and
/// `osc52 = "ask"` reaches the workers that admit writes.
#[test]
fn reload_publishes_both_policies_to_staging() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    // A private staging keeps this reload off the process default sibling tests read.
    app.clip_state = ClipState::new(ClipboardStaging::with_budget(1 << 20, 1 << 20));
    let mut deny = Config::default();
    deny.terminal.osc52_read = Osc52ReadPolicy::Deny;
    app.apply_new_config(deny);
    assert_eq!(app.clip_state.staging.read_policy(), Osc52ReadPolicy::Deny);
    app.apply_new_config(Config::default());
    assert_eq!(app.clip_state.staging.read_policy(), Osc52ReadPolicy::Ask);
    assert_eq!(app.clip_state.staging.write_policy(), Osc52Policy::Allow);
    let mut ask = Config::default();
    ask.terminal.osc52 = Osc52Policy::Ask;
    app.apply_new_config(ask);
    assert_eq!(app.clip_state.staging.write_policy(), Osc52Policy::Ask);
}

/// Commit `text` over a fake clipboard holding `previous`, with any reassertion due at once.
fn committed_over(
    previous: &str,
    text: &str,
) -> (FakeClipboard, Arc<ClipboardStaging>, Option<PendingReassert>) {
    let mut fake = FakeClipboard::holding(previous);
    let staging = ClipboardStaging::with_budget(64 << 20, 64 << 20);
    let job = ClipboardJob::for_test(&staging, text);
    let delay = Some(Duration::ZERO);
    let (committed, pending) = commit_program(&mut fake, job, 0, &AtomicU64::new(0), delay);
    assert_eq!(committed.outcome, ClipWriteOutcome::Written);
    (fake, staging, pending)
}

/// A write a failing helper's cleanup reverted to the exact previous value is reasserted once,
/// and the settled reassertion releases its lease.
#[test]
fn reassert_restores_a_reverted_write_once() {
    let (mut fake, staging, pending) = committed_over("old", "program");
    let pending = pending.expect("a write over a readable value arms one reassertion");
    fake.set_external("old");
    assert!(reassert(&mut fake, pending, &AtomicU64::new(0)));
    assert_eq!(fake.state().value, "program");
    assert_eq!(fake.state().writes, ["program", "program"]);
    assert_eq!(staging.reserved_bytes(), 0, "the settled reassertion released its lease");
}

/// A newer clipboard value always wins: a different value is never overwritten, and a write
/// that still holds is not written again.
#[test]
fn reassert_cancelled_by_newer_value() {
    let (mut fake, _staging, pending) = committed_over("old", "program");
    fake.set_external("newer external copy");
    assert!(!reassert(&mut fake, pending.expect("armed"), &AtomicU64::new(0)));
    assert_eq!(fake.state().value, "newer external copy");
    let (mut fake, _staging, pending) = committed_over("old", "program");
    assert!(!reassert(&mut fake, pending.expect("armed"), &AtomicU64::new(0)));
    assert_eq!(fake.state().writes, ["program"], "a write that still holds is not repeated");
}

/// A change counter that moves during the comparison means a newer value arrived, so the
/// reassertion is cancelled even though the compared text matched.
#[test]
fn reassert_cancelled_by_sequence_change() {
    let (mut fake, _staging, pending) = committed_over("old", "program");
    fake.set_external("old");
    fake.state().race_comparison = true;
    assert!(!reassert(&mut fake, pending.expect("armed"), &AtomicU64::new(0)));
    assert_eq!(fake.state().value, "old");
    assert_eq!(fake.state().writes, ["program"]);
}

/// A policy epoch that moved after the write, a reload to `osc52 = "deny"`, cancels the
/// reassertion.
#[test]
fn reassert_cancelled_by_policy_revocation() {
    let (mut fake, _staging, pending) = committed_over("old", "program");
    fake.set_external("old");
    assert!(!reassert(&mut fake, pending.expect("armed"), &AtomicU64::new(1)));
    assert_eq!(fake.state().value, "old");
}

/// A reassertion is armed only for a non-empty write within 262,144 UTF-16 units over a readable
/// value whose block is at most 512 KiB, and its lease then covers the text, its native copy and
/// the snapshot.
#[test]
fn reassert_armed_only_within_bounds() {
    assert!(committed_over("", "program").2.is_none(), "an unreadable value");
    let oversized = "x".repeat(REASSERT_MAX_UTF16_UNITS);
    assert!(committed_over(&oversized, "program").2.is_none(), "a block over 512 KiB");
    let over_text = "x".repeat(REASSERT_MAX_UTF16_UNITS + 1);
    assert!(committed_over("old", &over_text).2.is_none(), "a write over the unit bound");
    assert!(committed_over("old", "").2.is_none(), "a clear");
    let (_fake, staging, pending) = committed_over("old", "program");
    assert!(pending.is_some());
    let covered = "program".len() + native_clipboard_bytes("program") + "old".len() * 2;
    assert_eq!(staging.reserved_bytes(), covered);
}

/// On the worker a due reassertion restores a reverted write; while one is armed the domain
/// stays busy, so a newer user copy queues behind it, cancels it and wins.
#[test]
fn worker_reassertion_runs_when_due_and_newer_copy_cancels_it() {
    let fake = FakeClipboard::holding("old");
    fake.state().revert_to = Some("old".into());
    let mut app = app_with_worker("old", &fake, Some(Duration::from_millis(1)));
    let job = ClipboardJob::for_test(&app.clip_state.staging, "program");
    let _ = app.submit_program_clipboard(1, job, false);
    settle(&mut app);
    assert_eq!(fake.state().value, "program", "the due reassertion restored the write");
    assert_eq!(fake.state().writes, ["program", "program"]);

    let fake = FakeClipboard::holding("old");
    let mut app = app_with_worker("old", &fake, Some(Duration::from_secs(3600)));
    let job = ClipboardJob::for_test(&app.clip_state.staging, "program");
    let _ = app.submit_program_clipboard(1, job, false);
    wait_until(&mut app, |app| !app.clip_state.domain.program_pending(1));
    assert_eq!(app.clip_state.domain.in_flight(), 1, "the armed reassertion keeps it busy");
    fake.set_external("old");
    assert!(app.set_clipboard_text("user".into()));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("old"), "queued, not written");
    settle(&mut app);
    assert_eq!(fake.state().value, "user");
    assert_eq!(fake.state().writes, ["program", "user"]);
}

/// A test job carrying a text and HTML offer instead of text.
fn offer_job(staging: &Arc<ClipboardStaging>) -> ClipboardJob {
    use crate::app::kitty_clipboard_offer::{plan, OfferPlan};
    let entries =
        vec![("text/plain".to_owned(), b"t".to_vec()), ("text/html".to_owned(), b"h".to_vec())];
    let OfferPlan::Offer(offer) = plan(entries, &[], &|_, _| true) else { panic!("an offer") };
    let mut job = ClipboardJob::for_test(staging, "");
    job.offer = Some(offer);
    job
}

/// Contract: an offer job commits as one native publish of every representation, never a text
/// write or a reassertion: written when the sink took them all, failed (answered `EIO`) when it
/// refused; its lease ends when the call returns. A posted offer runs on the worker, not inline.
#[test]
fn offer_job_publishes_once_on_worker_eio_on_refusal() {
    for refuse in [false, true] {
        let mut fake = FakeClipboard::holding("before");
        fake.state().refuse_offer = refuse;
        let staging = ClipboardStaging::with_budget(64 << 20, 64 << 20);
        let job = offer_job(&staging);
        let delay = Some(Duration::ZERO);
        let (committed, pending) = commit_program(&mut fake, job, 0, &AtomicU64::new(0), delay);
        let expected = if refuse { ClipWriteOutcome::Failed } else { ClipWriteOutcome::Written };
        assert_eq!(committed.outcome, expected);
        assert!(pending.is_none(), "an offer is never reasserted");
        assert_eq!(committed.bytes, 2, "the offer's representations");
        let state = fake.state();
        assert_eq!((state.offers, state.writes.len()), (1, 0));
        drop(state);
        drop(committed);
        assert_eq!(staging.reserved_bytes(), 0, "the lease ended with the call");
    }
    let fake = FakeClipboard::holding("before");
    let mut app = app_with_worker("before", &fake, None);
    let job = offer_job(&app.clip_state.staging);
    assert_eq!(app.submit_program_clipboard(1, job, false), None, "posted to the worker");
    settle(&mut app);
    assert_eq!(fake.state().offers, 1);
}
