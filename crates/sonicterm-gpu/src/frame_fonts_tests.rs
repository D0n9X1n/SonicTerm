//! One fallback apply invalidates every placeholder holder once per generation.

use super::*;
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::glyph_key::GlyphKey;

/// A rasterizer that never resolves a glyph, so the atlas caches a missing sentinel.
struct Unresolved;

impl Rasterizer for Unresolved {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        None
    }
}

/// The apply targets with stand-in frame key and preedit values.
struct Targets {
    applied: Option<(u64, u64)>,
    rows: RowGlyphCache,
    quads: LineQuadCache,
    style_rev: u64,
    frame_key: Option<u8>,
    atlas: GlyphAtlas,
    preedit: Option<&'static str>,
    epoch: u64,
}

impl Targets {
    fn prepare(&mut self, generation: u64) -> (FrameFonts, bool) {
        self.prepare_notice(7, generation)
    }

    fn prepare_notice(&mut self, notice_id: u64, generation: u64) -> (FrameFonts, bool) {
        let (token, change) = prepare_frame_fonts(
            &mut self.applied,
            (notice_id, generation),
            FontApplyTargets {
                row_glyph_cache: &mut self.rows,
                line_quad_cache: &mut self.quads,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.frame_key,
                glyph_atlas: &mut self.atlas,
                preedit_glyph_cache: &mut self.preedit,
                fallback_epoch: &mut self.epoch,
            },
        );
        (token, change.invalidated())
    }

    /// Prepare `notice_id` at `generation` through the renderer's own seam, owing into `owed`.
    fn prepare_owing(&mut self, owed: &mut bool, notice_id: u64, generation: u64) {
        prepare_and_owe(
            &mut self.applied,
            owed,
            (notice_id, generation),
            FontApplyTargets {
                row_glyph_cache: &mut self.rows,
                line_quad_cache: &mut self.quads,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.frame_key,
                glyph_atlas: &mut self.atlas,
                preedit_glyph_cache: &mut self.preedit,
                fallback_epoch: &mut self.epoch,
            },
        );
    }

    /// What one preparation of `notice_id` at `generation` changed.
    fn classify(&mut self, notice_id: u64, generation: u64) -> FontChange {
        prepare_frame_fonts(
            &mut self.applied,
            (notice_id, generation),
            FontApplyTargets {
                row_glyph_cache: &mut self.rows,
                line_quad_cache: &mut self.quads,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.frame_key,
                glyph_atlas: &mut self.atlas,
                preedit_glyph_cache: &mut self.preedit,
                fallback_epoch: &mut self.epoch,
            },
        )
        .1
    }
}

/// Targets with nothing applied yet.
fn fresh_targets() -> Targets {
    Targets {
        applied: None,
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 0,
        frame_key: None,
        atlas: GlyphAtlas::new(16, 16),
        preedit: None,
        epoch: 0,
    }
}

#[test]
fn a_preparation_tells_a_first_or_replaced_stack_from_a_newer_generation() {
    // The first preparation and a new notice are setups; only a newer generation of the notice
    // already applied is a fallback apply. Every invalidation still counts as before.
    let mut targets = fresh_targets();
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        assert_eq!(targets.classify(7, 0), FontChange::Initial);
        assert_eq!(targets.classify(7, 0), FontChange::None);
        assert_eq!(targets.classify(7, 1), FontChange::Generation);
        assert_eq!(targets.classify(9, 1), FontChange::Initial);
        assert_eq!(targets.classify(9, 2), FontChange::Generation);
    }
    let stats = sink.snapshot();
    assert_eq!((stats.font_fallback_applies, stats.font_generation_applies), (4, 2));
}

#[test]
fn each_fallback_apply_is_attributed_to_exactly_one_render_attempt_through_the_production_seams() {
    // Preparation and rendering run through prepare_and_owe and RenderScope, as the renderer runs
    // them. Steps: setup and render (presented); a newer generation prepared twice, then rendered
    // (presented) and rendered again on the same token (presented, not an apply); an unused
    // preparation, then a render that does not present (the apply); a newer generation, then a
    // replaced stack before any render (no apply owed). A stepping clock makes every time exact:
    // each preparation and each attempt reads the clock twice, one step apart.
    use crate::frame_stats::{test_clock, AttemptStats, CollectGuard, FrameStatsSink, RenderScope};
    test_clock::install(0, 1);
    let sink = FrameStatsSink::default();
    let mut targets = fresh_targets();
    let mut owed = false;
    let prepare = |targets: &mut Targets, owed: &mut bool, notice_id: u64, generation: u64| {
        let _collect = CollectGuard::enter(Some(&sink));
        targets.prepare_owing(owed, notice_id, generation);
    };
    let render = |owed: &mut bool, presents: bool| {
        let _scope = RenderScope::enter(Some(&sink), owed);
        if presents {
            // When: the step's frame passed the present boundary, as a presenter marks it.
            crate::frame_stats::note_attempt_presented();
        }
    };
    prepare(&mut targets, &mut owed, 7, 0);
    render(&mut owed, true);
    prepare(&mut targets, &mut owed, 7, 1);
    prepare(&mut targets, &mut owed, 7, 1);
    render(&mut owed, true);
    render(&mut owed, true);
    prepare(&mut targets, &mut owed, 7, 2);
    render(&mut owed, false);
    prepare(&mut targets, &mut owed, 7, 3);
    prepare(&mut targets, &mut owed, 9, 3);
    render(&mut owed, true);
    test_clock::remove();
    let stats = sink.snapshot();
    assert_eq!((stats.font_fallback_applies, stats.font_generation_applies), (5, 3));
    assert_eq!((stats.font_prepare_ns, stats.font_generation_prepare_ns), (6, 3), "6 preparations");
    let apply = AttemptStats { attempts: 2, presented: 1, attempt_ns: 2, ..AttemptStats::ZERO };
    assert_eq!(stats.apply_attempts, apply, "generations 1 and 2, once each; 3 was replaced");
    let every = AttemptStats { attempts: 5, presented: 4, attempt_ns: 5, ..AttemptStats::ZERO };
    assert_eq!(stats.attempts, every);
}

#[test]
fn an_owed_apply_reaches_exactly_one_attempt() {
    // Repeated preparations owe one apply; the next attempt takes it, so a reused token or a retry
    // carries none; an unused preparation's apply goes to the next attempt; a stack setup clears it.
    let mut owed = false;
    let take = |owed: &mut bool| std::mem::take(owed);
    owe_apply(&mut owed, FontChange::Initial);
    assert!(!take(&mut owed), "a first stack owes no fallback apply");
    owe_apply(&mut owed, FontChange::Generation);
    owe_apply(&mut owed, FontChange::Generation);
    owe_apply(&mut owed, FontChange::None);
    assert!(take(&mut owed), "two preparations, one apply attempt");
    assert!(!take(&mut owed), "the same token rendered again carries nothing");
    owe_apply(&mut owed, FontChange::Generation);
    owe_apply(&mut owed, FontChange::None);
    assert!(take(&mut owed), "an unused preparation's apply goes to the next attempt");
    owe_apply(&mut owed, FontChange::Generation);
    owe_apply(&mut owed, FontChange::Initial);
    assert!(!take(&mut owed), "a replaced stack is not a fallback apply");
}

#[test]
fn one_apply_invalidates_each_target_and_a_repeat_in_the_same_generation_does_nothing() {
    // A newer generation drops the missing sentinel, the frame key and the preedit, bumps the
    // style revision and the tab epoch once; preparing again in that generation changes nothing.
    let missing = GlyphKey::new('é', false, false);
    let mut targets = Targets {
        applied: Some((7, 0)),
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 4,
        frame_key: Some(9),
        atlas: GlyphAtlas::new(16, 16),
        preedit: Some("cached"),
        epoch: 0,
    };
    assert!(targets.atlas.get_or_insert(missing, &mut Unresolved).unwrap().missing);
    seed_caches(&mut targets);
    let (token, applied) = targets.prepare(1);
    assert!(applied);
    assert_eq!((token.notice_id(), token.generation()), (7, 1));
    assert_eq!(targets.atlas.get(missing), None, "the missing sentinel is forgotten");
    assert!(targets.rows.is_empty(), "shaped rows may hold notdef, so they are dropped");
    assert!(targets.quads.is_empty(), "line quads were built from those rows, so they are dropped");
    assert_eq!(
        (targets.style_rev, targets.frame_key, targets.preedit, targets.epoch),
        (5, None, None, 1)
    );
    targets.frame_key = Some(3);
    targets.preedit = Some("again");
    seed_caches(&mut targets);
    assert!(!targets.prepare(1).1, "a second preparation in the same generation does nothing");
    assert_eq!(
        (targets.style_rev, targets.frame_key, targets.preedit, targets.epoch),
        (5, Some(3), Some("again"), 1)
    );
    assert_eq!((targets.rows.len(), targets.quads.len()), (1, 1), "rows cached since are kept");
}

/// Cache one shaped row and one row of line quads, as a frame drawn before the apply would.
fn seed_caches(targets: &mut Targets) {
    targets.rows.resize(4);
    targets.quads.resize(4);
    targets.rows.insert(1, 0, 11, 0, sonicterm_text::row_glyph_cache::CachedRow::default());
    targets.quads.insert(1, 0, 11, crate::row_quad_cache::CachedRowQuads::default());
    assert_eq!((targets.rows.len(), targets.quads.len()), (1, 1));
}

#[test]
fn an_apply_counts_one_fallback_apply_and_a_repeat_counts_none() {
    // font_fallback_applies counts each preparation that applied a newer generation, once,
    // inside a counting renderer's scope; a repeat in that generation and an uncounted scope add none.
    let mut targets = Targets {
        applied: Some((7, 0)),
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 0,
        frame_key: None,
        atlas: GlyphAtlas::new(16, 16),
        preedit: None,
        epoch: 0,
    };
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        assert!(targets.prepare(1).1);
        assert!(!targets.prepare(1).1);
        assert!(targets.prepare(2).1);
    }
    assert_eq!(sink.snapshot().font_fallback_applies, 2);
    assert!(targets.prepare(3).1);
    assert_eq!(sink.snapshot().font_fallback_applies, 2, "nothing counts with the gate off");
}

/// A delivered fallback wake needs a frame unless the last preparation applied exactly that
/// notice and generation; an older generation or another notice still needs one.
#[test]
fn a_fallback_frame_is_due_until_its_generation_is_applied() {
    assert!(fallback_frame_due(None, (7, 0)), "nothing applied yet");
    assert!(fallback_frame_due(Some((7, 0)), (7, 1)), "a newer generation is pending");
    assert!(fallback_frame_due(Some((6, 3)), (7, 3)), "a replaced notice is pending");
    assert!(!fallback_frame_due(Some((7, 1)), (7, 1)), "already applied");
}

/// Hold the shared font fixture even after a failed sibling test poisoned it.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A waker that records each notice id it is called with.
fn recording_waker() -> (crate::core::FontFallbackWaker, std::sync::Arc<std::sync::Mutex<Vec<u64>>>)
{
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&calls);
    (std::sync::Arc::new(move |notice_id| recorded.lock().unwrap().push(notice_id)), calls)
}

#[test]
fn a_stale_wake_is_a_no_op_and_leaves_the_current_claim_alone() {
    // An event from a replaced notice needs no frame and must not clear the current notice's
    // claim: while that claim stands, a further completion posts nothing; the current event's
    // acknowledgement clears it, is due until applied, and the next completion posts again.
    let _lock = font_fixture_lock();
    let old_stack = crate::lib_tests::tracked_font_stack(14.0);
    let stack = crate::lib_tests::tracked_font_stack(14.0);
    let (old_id, current_id) = (old_stack.fallback_notice().id(), stack.fallback_notice().id());
    assert_ne!(old_id, current_id, "each configuration has its own notice");
    let (waker, calls) = recording_waker();
    attach_fallback_waker(Some(&stack), Some(&waker));
    let applied = Some((current_id, 0));

    stack.fallback_notice().complete();
    assert!(!acknowledge_fallback_wake(Some(&stack), applied, old_id), "stale event");
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![current_id], "the claim is still posted");
    assert!(acknowledge_fallback_wake(Some(&stack), applied, current_id));
    assert!(!acknowledge_fallback_wake(Some(&stack), Some((current_id, 2)), current_id));
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![current_id, current_id]);
    assert!(!acknowledge_fallback_wake(None, applied, current_id), "no stack, no frame");
}

#[test]
fn a_reload_attaches_the_waker_to_the_new_notice_and_ignores_the_old_one() {
    // A font reload installs a new body stack through the same seam `set_font` uses, which
    // attaches the window's waker to its new notice. The old notice may still complete and post;
    // queued alone or with a current event it is a no-op, and the current event needs exactly the
    // frame that applies its generation.
    let _lock = font_fixture_lock();
    let (waker, calls) = recording_waker();
    let old_stack = crate::lib_tests::tracked_font_stack(14.0);
    let mut slot = Some(old_stack.clone());
    attach_fallback_waker(slot.as_ref(), Some(&waker));
    install_body_stack(&mut slot, Some(crate::lib_tests::tracked_font_stack(15.0)), Some(&waker));
    let stack = slot.as_ref().expect("the reload installed a stack");
    let (old_id, current_id) = (old_stack.fallback_notice().id(), stack.fallback_notice().id());
    assert_ne!(old_id, current_id, "the reloaded stack has its own notice");
    let applied = Some((current_id, 0));

    old_stack.fallback_notice().complete();
    assert!(!acknowledge_fallback_wake(Some(stack), applied, old_id), "old event alone");
    old_stack.fallback_notice().complete();
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![old_id, current_id], "one event per notice");
    assert!(!acknowledge_fallback_wake(Some(stack), applied, old_id), "old event queued first");
    assert!(acknowledge_fallback_wake(Some(stack), applied, current_id));
}

#[test]
fn a_scale_change_keeps_the_notice_and_its_waker() {
    // A scale rebuild rescales the body stack in place (`change_scaling`), so its notice and the
    // attached waker survive: a later completion still wakes the window under the same id, and
    // the frame that applies it is the only extra one.
    let _lock = font_fixture_lock();
    let (waker, calls) = recording_waker();
    let stack = crate::lib_tests::tracked_font_stack(14.0);
    attach_fallback_waker(Some(&stack), Some(&waker));
    let notice_id = stack.fallback_notice().id();
    stack.change_scaling(stack.get_font_scale(), 144);
    assert_eq!(stack.fallback_notice().id(), notice_id, "rescaling keeps the notice");
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![notice_id], "the waker is still attached");
    assert!(acknowledge_fallback_wake(Some(&stack), Some((notice_id, 0)), notice_id));
    assert!(!acknowledge_fallback_wake(Some(&stack), Some((notice_id, 1)), notice_id));
}

/// A primary face lacking é, a locator that answers every fallback request with Rec Mono, and the
/// temporary directory holding the primary face, removed on drop.
struct FallbackStack {
    stack: sonicterm_engine::FontStack,
    directory: std::path::PathBuf,
}

impl Drop for FallbackStack {
    // Lifecycle: dropping `FallbackStack` removes its temporary font `directory`.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Answers every fallback request with Rec Mono, which has é.
struct RecMonoLocator;

impl sonicterm_font::locator::FontLocator for RecMonoLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        _: &[char],
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontOrigin};
        let handle = FontDataHandle {
            source: FontDataSource::OnDisk(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
            ),
            index: 0,
            variation: 0,
            origin: FontOrigin::BuiltIn,
            coverage: None,
        };
        Ok(vec![sonicterm_font::parser::ParsedFont::from_locator(&handle)?])
    }
}

fn fallback_stack(name: &str) -> FallbackStack {
    let directory =
        std::env::temp_dir().join(format!("sonicterm-gpu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c/test.ttf"),
        directory.join("primary.ttf"),
    )
    .unwrap();
    let stack = sonicterm_engine::FontStack::try_new_with_locator_for_test(
        "Roboto",
        vec![directory.clone()],
        std::sync::Arc::new(RecMonoLocator),
        14.0,
        96,
    )
    .unwrap();
    FallbackStack { stack, directory }
}

/// A one-shot gate: `open` releases every `wait`, which gives up after ten seconds.
#[derive(Default)]
struct Latch {
    opened: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Latch {
    fn open(&self) {
        *self.opened.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let opened = self
            .changed
            .wait_timeout_while(
                self.opened.lock().unwrap(),
                std::time::Duration::from_secs(10),
                |opened| !*opened,
            )
            .unwrap()
            .0;
        assert!(*opened, "a test latch was never opened");
    }
}

#[test]
fn a_mid_frame_merge_lags_until_the_frame_that_applies_its_generation_remeasures_the_title() {
    // Production seam end to end, with a real worker: frame N applies generation 0 and measures the
    // title with notdef's width. The worker appends Rec Mono and unlocks, then pauses before
    // completing, so frames N+1 and N+2 merge the face (é shapes to a real glyph) yet apply nothing
    // and keep the stored width: the allowed lag. Completion posts one wake; acknowledging it says a
    // frame is due, and that frame applies generation 1 once, bumps the epoch, counts the apply and
    // remeasures the title with é's real advance. The next frame applies nothing.
    let _lock = font_fixture_lock();
    let fixture = fallback_stack("mid-frame");
    let stack = &fixture.stack;
    let (entered, release) =
        (std::sync::Arc::new(Latch::default()), std::sync::Arc::new(Latch::default()));
    let (worker_entered, worker_release) =
        (std::sync::Arc::clone(&entered), std::sync::Arc::clone(&release));
    stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
        before_completion: Some(std::sync::Arc::new(move || {
            worker_entered.open();
            worker_release.wait();
        })),
        ..Default::default()
    });
    let (waker, calls) = recording_waker();
    attach_fallback_waker(Some(stack), Some(&waker));
    let notice_id = stack.fallback_notice().id();
    let mut title_font = crate::core::tab_title_font::TabTitleFont::new(
        "Roboto",
        14.0,
        1.0,
        1.0,
        Some(stack.clone()),
    );
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("é"));
    let (mut rows, mut quads) = (RowGlyphCache::new(), LineQuadCache::new());
    let (mut style_rev, mut frame_key, mut preedit) = (0_u64, None::<u8>, None::<&str>);
    let mut atlas = GlyphAtlas::new(16, 16);
    let mut applied = None;
    let sink = crate::frame_stats::FrameStatsSink::default();
    let mut frame = |title_font: &mut crate::core::tab_title_font::TabTitleFont,
                     tabs: &mut sonicterm_render_model::boundary::ui::tabs::TabBar,
                     applied: &mut Option<(u64, u64)>| {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        let current = (notice_id, stack.fallback_notice().generation());
        let (_token, change) = prepare_frame_fonts(
            applied,
            current,
            FontApplyTargets {
                row_glyph_cache: &mut rows,
                line_quad_cache: &mut quads,
                style_rev: &mut style_rev,
                last_frame_key: &mut frame_key,
                glyph_atlas: &mut atlas,
                preedit_glyph_cache: &mut preedit,
                fallback_epoch: title_font.fallback_epoch_mut(),
            },
        );
        let _ = title_font.measure(tabs, false, false, std::time::Instant::now());
        (change.invalidated(), tabs.tabs()[0].content_width_px().expect("the title was measured"))
    };
    let real_glyph = |stack: &sonicterm_engine::FontStack| {
        stack.shape_text_for_frame("é", false, false).unwrap()[0].glyph_pos
    };

    let (did_apply, notdef_width) = frame(&mut title_font, &mut tabs, &mut applied);
    assert!(did_apply, "frame N applies generation 0");
    entered.wait();
    for _ in 0..2 {
        let (did_apply, width) = frame(&mut title_font, &mut tabs, &mut applied);
        assert!(!did_apply, "no newer generation is published yet");
        assert_eq!(width, notdef_width, "the stored width lags until a frame applies");
        assert_ne!(real_glyph(stack), 0, "the face merged mid-frame, so é already shapes");
    }
    assert!(calls.lock().unwrap().is_empty(), "no wake before the completion");

    release.open();
    // `complete` bumps the generation before it posts the wake, so the test waits for the delivery
    // itself, bounded, rather than for the generation.
    let started = std::time::Instant::now();
    while calls.lock().unwrap().is_empty() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the wake was never delivered"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(stack.fallback_notice().generation(), 1, "the delivered wake follows generation 1");
    assert_eq!(*calls.lock().unwrap(), vec![notice_id], "one wake for the publication");
    assert!(acknowledge_fallback_wake(Some(stack), applied, notice_id), "a frame is due");
    let epoch_before = *title_font.fallback_epoch_mut();
    let (did_apply, real_width) = frame(&mut title_font, &mut tabs, &mut applied);
    assert!(did_apply, "the woken frame applies generation 1");
    assert_eq!(*title_font.fallback_epoch_mut(), epoch_before + 1);
    assert_ne!(real_width, notdef_width, "that frame remeasures the title with é's real advance");
    assert!(!frame(&mut title_font, &mut tabs, &mut applied).0, "the next frame applies nothing");
    assert!(!acknowledge_fallback_wake(Some(stack), applied, notice_id), "no further frame is due");
    assert_eq!(sink.snapshot().font_fallback_applies, 2, "generations 0 and 1, once each");
}
