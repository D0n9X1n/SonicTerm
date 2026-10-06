//! Title, chrome-run and palette caches: keys, admission, clears and reported bytes.

use super::*;
use crate::frame_stats::{CollectGuard, FrameStats, FrameStatsSink};

/// Serialize tests that shape with the tracked font, as the other chrome tests do.
fn font_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run `work` inside a counting scope and return its output with the counters it moved.
fn counted<Output>(work: impl FnOnce() -> Output) -> (Output, FrameStats) {
    let sink = FrameStatsSink::default();
    let output = {
        let _collect = CollectGuard::enter(Some(&sink));
        work()
    };
    (output, sink.snapshot())
}

/// A title probe for `text` at 15 px with `text_px` of room, under font key 1 and `epoch`.
fn probe(text: &str, text_px: f32, epoch: u64) -> TitleProbe<'_> {
    TitleProbe { text, font_key: 1, fallback_epoch: epoch, raster_px: 15.0, text_px }
}

/// The body-stack key of a regular 15 px run.
fn body_key() -> ChromeRunKey {
    ChromeRunKey::new(ChromeStack::Body, ChromeAttrs::default(), 15.0, 15.0)
}

/// Bytes of one kept glyph, independent of the cache's own constant.
fn glyph_bytes() -> usize {
    CHROME_SHAPED_GLYPH_BYTES
}

#[test]
fn title_table_is_fixed_at_64_slots() {
    // The title table never grows: 21, 42, 63 and 70 tabs all use one 64-slot table whose
    // reported table bytes are constant, slots at or past the tab count are dropped, and a tab
    // at position 64 or later is drawn but never kept.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let table_bytes = std::mem::size_of::<[Option<PreparedTitle>; TITLE_SLOTS]>();
    let titles: Vec<String> = (0..70).map(|index| format!("t{index}")).collect();
    for tab_count in [21, 42, 63, 70, 21] {
        cache.retain_tabs(tab_count);
        for (position, title) in titles.iter().enumerate().take(tab_count) {
            let _ = cache.prepare(position, &probe(title, 400.0, 0), &stack, true);
        }
        assert_eq!(cache.slot_count(), TITLE_SLOTS, "{tab_count} tabs: one 64-slot table");
        for position in tab_count..TITLE_SLOTS {
            assert!(!cache.is_stored(position), "{tab_count} tabs: slot {position} is empty");
        }
        assert!(!cache.is_stored(64) && !cache.is_stored(69), "tab 65 and later are never kept");
        let kept = tab_count.min(TITLE_SLOTS);
        assert_eq!(cache.len(), kept, "{tab_count} tabs keep {kept} titles");
        let payload: usize = titles
            .iter()
            .take(kept)
            .map(|title| {
                let glyphs = stack.shape_text_for_frame(title, false, false).unwrap().len();
                title.len() + title.len() + glyphs * glyph_bytes()
            })
            .sum();
        assert_eq!(cache.retained_bytes(), table_bytes + payload, "{tab_count} tabs: exact bytes");
    }
}

/// The renderer's title fit before titles were cached, kept as a frozen oracle: every measure
/// goes through `shaped_advances`, a failed ellipsis counts 0 px, a failed cut counts its
/// estimated width, and a failed whole title draws nothing.
fn frozen_title_fit(
    stack: &sonicterm_engine::FontStack,
    text: &str,
    font_size_px: f32,
    available_px: f32,
) -> (String, f32) {
    let measure = |candidate: &str| {
        crate::chrome_text::shaped_advances(
            stack,
            candidate,
            ChromeAttrs::default(),
            font_size_px,
            font_size_px,
        )
    };
    let Some(advances) = measure(text) else {
        return (String::new(), 0.0);
    };
    let whole_px: f32 = advances.iter().map(|(_, advance)| advance).sum();
    if whole_px <= available_px + TITLE_FIT_TOLERANCE_PX {
        return (text.to_string(), whole_px);
    }
    let ellipsis_px: f32 =
        measure("…").map_or(0.0, |ellipsis| ellipsis.iter().map(|(_, advance)| advance).sum());
    let mut budget_px = available_px;
    for _ in 0..=text.len() {
        let fitted = fit_title_to_width(text, &advances, ellipsis_px, budget_px);
        let drawn_px: f32 = measure(fitted.text.as_str())
            .map_or(fitted.width_px, |cut| cut.iter().map(|(_, advance)| advance).sum());
        if fitted.text.is_empty() || drawn_px <= available_px + TITLE_FIT_TOLERANCE_PX {
            return (fitted.text, drawn_px);
        }
        budget_px = fitted.width_px - TITLE_FIT_TOLERANCE_PX - 0.01;
    }
    (String::new(), 0.0)
}

/// Fit `text` into `available_px` with one shape failure after `successes` good shapes, and
/// check the title draws the text and width the frozen pre-cache fit draws under the same
/// failure, is not kept, and is kept on the next frame at the same epoch as an unfailed fit.
fn check_failed_fit(name: &str, text: &str, available_px: f32, successes: usize) {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    crate::chrome_text::fail_chrome_shape_after_for_test(successes);
    let frozen = frozen_title_fit(&stack, text, 15.0, available_px);
    let mut cache = TitleCache::default();
    crate::chrome_text::fail_chrome_shape_after_for_test(successes);
    let draw = cache.prepare(0, &probe(text, available_px, 0), &stack, true);
    let drawn = cache.drawn(&draw);
    assert_eq!((drawn.text.to_string(), drawn.width_px), frozen, "{name}: drawn as before");
    assert!(!cache.is_stored(0), "{name}: a failed fit is not kept");
    let draw = cache.prepare(0, &probe(text, available_px, 0), &stack, true);
    assert!(matches!(draw, TitleDraw::Cached(0)), "{name}: the next frame keeps it");
    let healthy = frozen_title_fit(&stack, text, 15.0, available_px);
    let drawn = cache.drawn(&draw);
    assert_eq!((drawn.text.to_string(), drawn.width_px), healthy, "{name}: as an unfailed fit");
}

/// A title long enough to be cut into a 120 px tab.
const LONG_TITLE: &str = "a fairly long tab title that has to be cut";

#[test]
fn a_failed_whole_title_draws_nothing_and_is_not_kept() {
    // The whole-title measure fails: nothing is drawn, as before, and nothing is kept.
    check_failed_fit("whole title", "fits", 400.0, 0);
}

#[test]
fn a_failed_ellipsis_draws_its_cut_but_is_not_kept() {
    // The ellipsis measure fails: the cut is fitted with a 0 px ellipsis and its final run shapes,
    // so the title is drawable, yet the incomplete fit is not kept.
    check_failed_fit("ellipsis", LONG_TITLE, 120.0, 1);
}

#[test]
fn a_failed_first_cut_draws_its_estimate_and_is_not_kept() {
    // The first cut's measure fails: it counts its estimated width, as before, and is not kept.
    check_failed_fit("first cut", LONG_TITLE, 120.0, 2);
}

#[test]
fn the_empty_title_is_kept() {
    // An empty title shapes to a valid empty run, so it is complete and kept.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let draw = cache.prepare(0, &probe("", 400.0, 0), &stack, true);
    assert!(matches!(draw, TitleDraw::Cached(0)), "the empty title is kept");
    assert_eq!(cache.drawn(&draw).text, "");
}

#[test]
fn title_admission_stops_at_the_text_and_glyph_limits() {
    // A title is kept at exactly the text-byte and glyph limits and drawn but not kept one past
    // either of them.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let at_limit = "x".repeat(MAX_CACHED_TEXT_BYTES);
    let over_limit = "x".repeat(MAX_CACHED_TEXT_BYTES + 1);
    let _ = cache.prepare(0, &probe(&at_limit, 100_000.0, 0), &stack, true);
    assert!(cache.is_stored(0), "256 text bytes are kept");
    let _ = cache.prepare(1, &probe(&over_limit, 100_000.0, 0), &stack, true);
    assert!(!cache.is_stored(1), "257 text bytes are not");
    let mut few = TitleCache::default();
    few.set_limits_for_test(AdmissionLimits { text_bytes: MAX_CACHED_TEXT_BYTES, glyphs: 4 });
    let _ = few.prepare(0, &probe("abcd", 400.0, 0), &stack, true);
    assert!(few.is_stored(0), "a title at the glyph limit is kept");
    let draw = few.prepare(1, &probe("abcde", 400.0, 0), &stack, true);
    assert!(!few.is_stored(1), "one glyph past it is not");
    assert_eq!(few.drawn(&draw).text, "abcde", "but it is drawn");
}

#[test]
fn a_warm_title_is_reused_without_shaping_and_a_key_change_prepares() {
    // A kept title with an unchanged key draws with no shaping request and counts one reuse;
    // changing any key part counts one prepare and shapes again; reuse off never keeps.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let (_, cold) = counted(|| cache.prepare(0, &probe("shell", 400.0, 0), &stack, true));
    assert_eq!((cold.tab_title_prepares, cold.tab_title_reuses), (1, 0));
    assert!(cold.shape_requests >= 1, "a cold title shapes");
    let (_, warm) = counted(|| cache.prepare(0, &probe("shell", 400.0, 0), &stack, true));
    assert_eq!((warm.tab_title_prepares, warm.tab_title_reuses, warm.shape_requests), (0, 1, 0));
    for changed in [
        probe("shell2", 400.0, 0),
        probe("shell", 401.0, 0),
        probe("shell", 400.0, 1),
        TitleProbe { font_key: 2, ..probe("shell", 400.0, 0) },
        TitleProbe { raster_px: 16.0, ..probe("shell", 400.0, 0) },
    ] {
        let (_, stats) = counted(|| cache.prepare(0, &changed, &stack, true));
        assert_eq!((stats.tab_title_prepares, stats.tab_title_reuses), (1, 0), "{changed:?}");
    }
    let mut off = TitleCache::default();
    let _ = off.prepare(0, &probe("shell", 400.0, 0), &stack, false);
    assert!(!off.is_stored(0), "reuse off keeps nothing");
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

/// Pause `stack`'s fallback worker after it appended and unlocked, before it completes the
/// notice; returns the latch it opens on arrival and the latch that releases it.
fn pause_before_completion(
    stack: &sonicterm_engine::FontStack,
) -> (std::sync::Arc<Latch>, std::sync::Arc<Latch>) {
    let (arrived, release) =
        (std::sync::Arc::new(Latch::default()), std::sync::Arc::new(Latch::default()));
    let (worker_arrived, worker_release) =
        (std::sync::Arc::clone(&arrived), std::sync::Arc::clone(&release));
    stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
        before_completion: Some(std::sync::Arc::new(move || {
            worker_arrived.open();
            worker_release.wait();
        })),
        ..Default::default()
    });
    (arrived, release)
}

/// Wait, bounded, until `stack`'s notice publishes `generation`.
fn wait_for_generation(stack: &sonicterm_engine::FontStack, generation: u64) {
    let started = std::time::Instant::now();
    while stack.fallback_notice().generation() < generation {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "no generation {generation}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn pending_fallback_title_resolves_after_the_epoch_bump() {
    // A title with a fallback-only character is kept with notdef at epoch E. While the worker
    // has appended and unlocked but not completed, the title still hits (notdef): the one-frame
    // contract the rows keep too. After completion the next preparation bumps the epoch, so the
    // title misses, reshapes with the merged face and is kept again; an unchanged epoch keeps it.
    let fixture = crate::lib_tests::fallback_stack("title-pending");
    let stack = &fixture.stack;
    let (arrived, release) = pause_before_completion(stack);
    let mut cache = TitleCache::default();
    let draw = cache.prepare(0, &probe("é", 400.0, 0), stack, true);
    assert!(matches!(draw, TitleDraw::Cached(0)), "a notdef title is kept");
    assert_eq!(cache.drawn(&draw).view.unwrap().glyph_ids_for_test(), vec![0]);
    arrived.wait();
    let (draw, stats) = counted(|| cache.prepare(0, &probe("é", 400.0, 0), stack, true));
    assert_eq!(stats.tab_title_reuses, 1, "before completion the title still hits");
    assert_eq!(cache.drawn(&draw).view.unwrap().glyph_ids_for_test(), vec![0], "with notdef");
    release.open();
    wait_for_generation(stack, 1);
    let (draw, stats) = counted(|| cache.prepare(0, &probe("é", 400.0, 1), stack, true));
    assert_eq!(stats.tab_title_prepares, 1, "the bumped epoch misses");
    assert_ne!(cache.drawn(&draw).view.unwrap().glyph_ids_for_test(), vec![0], "and resolves");
    let (_, stats) = counted(|| cache.prepare(0, &probe("é", 400.0, 1), stack, true));
    assert_eq!(stats.tab_title_reuses, 1, "an unchanged epoch keeps the resolved title");
}

#[test]
fn same_key_face_replacement_misses_after_a_clear() {
    // A face replacement can keep every key part (same family, size, epoch). A cleared cache then
    // misses and draws the replacement stack's own glyph ids; both caches clear the same way.
    let _lock = font_lock();
    let first = crate::lib_tests::tracked_font_stack(15.0);
    let fixture = crate::lib_tests::fallback_stack("same-key");
    let replacement = &fixture.stack;
    let mut titles = TitleCache::default();
    let mut runs = ChromeRunCache::default();
    let _ = titles.prepare(0, &probe("abc", 400.0, 0), &first, true);
    let _ = runs.prepare(&first, "abc", body_key(), true);
    titles.clear();
    runs.clear();
    let (draw, stats) = counted(|| titles.prepare(0, &probe("abc", 400.0, 0), replacement, true));
    assert_eq!(stats.tab_title_prepares, 1, "a cleared title misses");
    let expected: Vec<u32> = replacement
        .shape_text_for_frame("abc", false, false)
        .unwrap()
        .iter()
        .map(|glyph| glyph.glyph_pos)
        .collect();
    assert_eq!(titles.drawn(&draw).view.unwrap().glyph_ids_for_test(), expected);
    let (handle, stats) = counted(|| runs.prepare(replacement, "abc", body_key(), true));
    assert_eq!(stats.chrome_run_prepares, 1, "a cleared run misses");
    assert_eq!(runs.view(&handle).unwrap().glyph_ids_for_test(), expected);
}

/// Total string bytes of every `Hex("…")` in `palette`'s debug form, independent of the cache.
fn palette_hex_bytes(palette: &Palette) -> usize {
    let debug = format!("{palette:?}");
    debug.split("Hex(\"").skip(1).map(|tail| tail.find('"').expect("a closing quote")).sum()
}

#[test]
fn chrome_cache_reports_exact_bytes() {
    // The reported bytes are the two tables, each kept title's key text and drawn text plus its
    // glyphs, each kept run's one text plus its glyphs, and the palette's color strings, counted
    // independently here. The envelope bounds full tables of maximal entries.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let theme = Theme::default();
    let mut caches = ChromeCaches::new(&theme);
    let glyphs = |text: &str| stack.shape_text_for_frame(text, false, false).unwrap().len();
    let long = "a fairly long tab title that has to be cut";
    let _ = caches.titles.prepare(0, &probe("shell", 400.0, 0), &stack, true);
    let cut = caches.titles.prepare(1, &probe(long, 120.0, 0), &stack, true);
    let cut_text = caches.titles.drawn(&cut).text.to_string();
    let _ = caches.runs.prepare(&stack, "\u{f002}", body_key(), true);
    let _ = caches.runs.prepare(&stack, "search: foo", body_key(), true);
    let expected = std::mem::size_of::<[Option<PreparedTitle>; TITLE_SLOTS]>()
        + ("shell".len() * 2 + glyphs("shell") * glyph_bytes())
        + (long.len() + cut_text.len() + glyphs(&cut_text) * glyph_bytes())
        + std::mem::size_of::<[Option<ChromeRunEntry>; CHROME_RUN_SLOTS]>()
        + ("\u{f002}".len() + glyphs("\u{f002}") * glyph_bytes())
        + ("search: foo".len() + glyphs("search: foo") * glyph_bytes())
        + palette_hex_bytes(&theme.colors);
    assert_eq!(caches.retained_bytes(), expected);
    assert_eq!(caches.items(), 4);

    let mut full = ChromeCaches::new(&theme);
    let widest: String = "W".repeat(MAX_CACHED_TEXT_BYTES);
    for position in 0..TITLE_SLOTS {
        let text = format!("{position:03}{}", &widest[3..]);
        let _ = full.titles.prepare(position, &probe(&text, 100_000.0, 0), &stack, true);
    }
    for index in 0..CHROME_RUN_SLOTS {
        let text = format!("{index:03}{}", &widest[3..]);
        let _ = full.runs.prepare(&stack, &text, body_key(), true);
    }
    assert_eq!((full.titles.len(), full.runs.len()), (TITLE_SLOTS, CHROME_RUN_SLOTS));
    assert!(full.titles.retained_bytes() <= title_cache_envelope_bytes());
    assert!(full.runs.retained_bytes() <= chrome_run_cache_envelope_bytes());
}

#[test]
fn palette_is_seeded_and_equality_guarded() {
    // The palette cache is seeded from the constructor theme, so the first lookup derives
    // nothing; a theme whose colors differ derives once and returns its own palette; repeated
    // lookups of equal colors derive nothing.
    let theme_a = Theme::default();
    let mut theme_b = Theme::default();
    theme_b.colors.background =
        sonicterm_render_model::boundary::cfg::theme::Hex("#123456".to_string());
    let mut cache = PaletteCache::seeded(&theme_a);
    assert_eq!(cache.palette_for(&theme_a), UiPalette::from_theme(&theme_a));
    assert_eq!(cache.computes(), 0, "the seeded theme derives nothing");
    assert_eq!(cache.palette_for(&theme_b), UiPalette::from_theme(&theme_b));
    assert_eq!(cache.computes(), 1, "a changed theme derives once");
    for _ in 0..3 {
        assert_eq!(cache.palette_for(&theme_b), UiPalette::from_theme(&theme_b));
    }
    assert_eq!(cache.computes(), 1, "equal colors derive nothing");
    let mut renamed = theme_b.clone();
    renamed.name = "renamed".to_string();
    let _ = cache.palette_for(&renamed);
    assert_eq!(cache.computes(), 1, "only the colors are compared");
}

#[test]
fn chrome_runs_hit_by_exact_key_and_text() {
    // A chrome run hits only on an equal key and equal text; each key part alone and the text
    // alone make a miss. A full table replaces its least recently used entry. A failed shape and
    // a run over either admission limit are drawn but not kept. Each kept run holds one text.
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = ChromeRunCache::default();
    let _ = cache.prepare(&stack, "label", body_key(), true);
    let (_, stats) = counted(|| cache.prepare(&stack, "label", body_key(), true));
    assert_eq!((stats.chrome_run_reuses, stats.shape_requests), (1, 0), "an exact hit");
    let bold = ChromeAttrs { bold: true, italic: false };
    for (name, key, text) in [
        ("attrs", ChromeRunKey::new(ChromeStack::Body, bold, 15.0, 15.0), "label"),
        ("size", ChromeRunKey::new(ChromeStack::Body, ChromeAttrs::default(), 16.0, 15.0), "label"),
        (
            "native em",
            ChromeRunKey::new(ChromeStack::Body, ChromeAttrs::default(), 15.0, 16.0),
            "label",
        ),
        (
            "stack",
            ChromeRunKey::new(ChromeStack::TabTitle, ChromeAttrs::default(), 15.0, 15.0),
            "label",
        ),
        ("text", body_key(), "labe1"),
    ] {
        let mut probe_cache = ChromeRunCache::default();
        let _ = probe_cache.prepare(&stack, "label", body_key(), true);
        let (_, stats) = counted(|| probe_cache.prepare(&stack, text, key, true));
        assert_eq!(stats.chrome_run_prepares, 1, "{name} alone misses");
    }

    let mut cache = ChromeRunCache::default();
    let texts: Vec<String> = (0..CHROME_RUN_SLOTS).map(|index| format!("run {index}")).collect();
    for text in &texts {
        let _ = cache.prepare(&stack, text, body_key(), true);
    }
    let _ = cache.prepare(&stack, &texts[0], body_key(), true);
    let _ = cache.prepare(&stack, "run 32", body_key(), true);
    assert_eq!(cache.len(), CHROME_RUN_SLOTS);
    let (_, stats) = counted(|| cache.prepare(&stack, &texts[0], body_key(), true));
    assert_eq!(stats.chrome_run_reuses, 1, "the recently used entry survives");
    let (_, stats) = counted(|| cache.prepare(&stack, &texts[1], body_key(), true));
    assert_eq!(stats.chrome_run_prepares, 1, "the oldest entry was evicted");

    for text in ["\u{f002}", "search: foo 1/3", "検索 テキスト", "=> != ->"] {
        let mut cache = ChromeRunCache::default();
        let handle = cache.prepare(&stack, text, body_key(), true);
        let measured = stack.measure_text_width_for_frame(text).unwrap();
        assert_eq!(cache.view(&handle).unwrap().raw_width_px().to_bits(), measured.to_bits());
    }

    let mut cache = ChromeRunCache::default();
    crate::chrome_text::fail_chrome_shape_after_for_test(0);
    assert!(matches!(cache.prepare(&stack, "label", body_key(), true), ChromeRunHandle::Failed));
    assert_eq!(cache.len(), 0, "a failed shape keeps nothing");
    let at_limit = "x".repeat(MAX_CACHED_TEXT_BYTES);
    let over_limit = "x".repeat(MAX_CACHED_TEXT_BYTES + 1);
    assert!(matches!(
        cache.prepare(&stack, &over_limit, body_key(), true),
        ChromeRunHandle::Fresh(_)
    ));
    assert!(matches!(
        cache.prepare(&stack, &at_limit, body_key(), true),
        ChromeRunHandle::Cached(_)
    ));
    let mut few_glyphs = ChromeRunCache::default();
    few_glyphs
        .set_limits_for_test(AdmissionLimits { text_bytes: MAX_CACHED_TEXT_BYTES, glyphs: 4 });
    assert!(matches!(
        few_glyphs.prepare(&stack, "abcde", body_key(), true),
        ChromeRunHandle::Fresh(_)
    ));
    assert!(matches!(
        few_glyphs.prepare(&stack, "abcd", body_key(), true),
        ChromeRunHandle::Cached(_)
    ));

    let mut one = ChromeRunCache::default();
    let _ = one.prepare(&stack, "label", body_key(), true);
    let glyphs = stack.shape_text_for_frame("label", false, false).unwrap().len();
    let table = std::mem::size_of::<[Option<ChromeRunEntry>; CHROME_RUN_SLOTS]>();
    assert_eq!(one.retained_bytes(), table + "label".len() + glyphs * glyph_bytes(), "one text");
}

#[test]
fn pending_chrome_run_resolves_after_apply() {
    // A fallback-only label is kept as notdef. The body stack has no epoch, so it keeps hitting
    // after the face is published until the applied generation clears the cache; the next
    // lookup then prepares and returns the resolved glyph.
    let fixture = crate::lib_tests::fallback_stack("run-pending");
    let stack = &fixture.stack;
    let mut cache = ChromeRunCache::default();
    let handle = cache.prepare(stack, "é", body_key(), true);
    assert_eq!(cache.view(&handle).unwrap().glyph_ids_for_test(), vec![0], "kept as notdef");
    wait_for_generation(stack, 1);
    let (handle, stats) = counted(|| cache.prepare(stack, "é", body_key(), true));
    assert_eq!(stats.chrome_run_reuses, 1, "it hits until the apply clears it");
    assert_eq!(cache.view(&handle).unwrap().glyph_ids_for_test(), vec![0]);
    cache.clear();
    let (handle, stats) = counted(|| cache.prepare(stack, "é", body_key(), true));
    assert_eq!(stats.chrome_run_prepares, 1, "after the apply the lookup prepares");
    assert_ne!(cache.view(&handle).unwrap().glyph_ids_for_test(), vec![0], "and resolves");
}

#[test]
fn an_oversized_palette_is_derived_and_never_kept() {
    // The kept palette's color strings never pass the allowance: a theme over it, whether its
    // strings are valid colors padded with whitespace or malformed text, is derived on every
    // request (the same palette `UiPalette::from_theme` gives) and never stored, and a theme
    // over it at construction seeds bounded colors instead.
    let mut padded = Theme::default();
    padded.colors.background = sonicterm_render_model::boundary::cfg::theme::Hex(format!(
        "{}#123456",
        " ".repeat(2 * 1024 * 1024)
    ));
    let mut malformed = Theme::default();
    malformed.colors.tab.active_fg =
        sonicterm_render_model::boundary::cfg::theme::Hex("not a color ".repeat(1024));
    for (name, theme) in [("padded", &padded), ("malformed", &malformed)] {
        let mut cache = PaletteCache::seeded(&Theme::default());
        for request in 1..=3 {
            assert_eq!(cache.palette_for(theme), UiPalette::from_theme(theme), "{name}");
            assert_eq!(cache.computes(), request, "{name}: derived on every request");
            assert!(cache.retained_bytes() <= PALETTE_ALLOWANCE_BYTES, "{name}: never kept");
        }
        let seeded = PaletteCache::seeded(theme);
        assert!(
            seeded.retained_bytes() <= PALETTE_ALLOWANCE_BYTES,
            "{name}: seeding stays bounded"
        );
    }
}

/// Releasing the runs frees both tables and leaves only the palette's bytes; the palette and its
/// derivation count are kept, and the next title and run prepare again. `clear_runs` keeps both
/// tables allocated.
#[test]
fn release_runs_keeps_only_the_palette() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let theme = Theme::default();
    let fill = |caches: &mut ChromeCaches| {
        let _ = caches.titles.prepare(0, &probe("shell", 400.0, 0), &stack, true);
        let _ = caches.runs.prepare(&stack, "search: foo", body_key(), true);
    };
    let mut cleared = ChromeCaches::new(&theme);
    fill(&mut cleared);
    cleared.clear_runs();
    let palette = palette_hex_bytes(&theme.colors);
    assert!(cleared.retained_bytes() > palette, "clear_runs keeps both tables");

    let mut caches = ChromeCaches::new(&theme);
    fill(&mut caches);
    let computes = caches.palette.computes();
    caches.release_runs();
    assert_eq!(caches.retained_bytes(), palette);
    assert_eq!(caches.items(), 0);
    assert_eq!(caches.palette.computes(), computes, "the palette is kept, not derived again");
    assert_eq!(caches.palette.palette_for(&theme), UiPalette::from_theme(&theme));
    fill(&mut caches);
    assert_eq!(caches.items(), 2, "the next title and run prepare again");
}

/// Prepare `text` at tab 0 with `available_px` of room and draw it as the tab bar does, inside a
/// missing-chrome scope. `fail_prepare` and `fail_draw` arm one shape failure after that many good
/// shapes before each step. Returns the drawn text and every chrome character reported missing.
fn draw_title(
    cache: &mut TitleCache,
    stack: &sonicterm_engine::FontStack,
    text: &str,
    available_px: f32,
    fail_prepare: Option<usize>,
    fail_draw: Option<usize>,
) -> (String, Vec<char>) {
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(256, 256);
    let scope = crate::chrome_text::MissingChromeScope::enter();
    if let Some(successes) = fail_prepare {
        crate::chrome_text::fail_chrome_shape_after_for_test(successes);
    }
    let draw = cache.prepare(0, &probe(text, available_px, 0), stack, true);
    let drawn = cache.drawn(&draw);
    crate::chrome_text::disarm_chrome_shape_failure_for_test();
    if let Some(successes) = fail_draw {
        crate::chrome_text::fail_chrome_shape_after_for_test(successes);
    }
    let placement = TitlePlacement {
        color: ChromeColor::WHITE,
        raster_px: 15.0,
        origin: (0.0, 20.0),
        screen: (400.0, 100.0),
        clip: None,
    };
    let _layout = layout_title(stack, &mut raster, &mut atlas, &drawn, placement);
    crate::chrome_text::disarm_chrome_shape_failure_for_test();
    (drawn.text.to_string(), scope.finish())
}

/// A title whose whole text cannot be shaped draws nothing, so its visible characters are reported
/// missing exactly once, at the title-draw boundary.
#[test]
fn a_title_that_never_shapes_is_reported_once() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let (drawn, missing) = draw_title(&mut cache, &stack, "fits", 400.0, Some(0), None);
    assert_eq!(drawn, "", "nothing is drawn");
    assert_eq!(missing, vec!['f', 'i', 't', 's'], "the whole title is reported once");
}

/// A cut that did not shape while fitting is shaped again at draw time; when that succeeds the
/// title drew, so nothing is reported.
#[test]
fn a_cut_whose_draw_time_retry_shapes_reports_nothing() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    // Whole title and ellipsis shape; the first cut fails while fitting.
    let (drawn, missing) = draw_title(&mut cache, &stack, LONG_TITLE, 120.0, Some(2), None);
    assert!(!drawn.is_empty(), "a cut is drawn");
    assert!(missing.is_empty(), "the retried cut drew: {missing:?}");
}

/// A cut that fails both while fitting and at draw time is reported once, by the draw-time retry,
/// never a second time at the title-draw boundary.
#[test]
fn a_cut_whose_draw_time_retry_fails_is_reported_once() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let (drawn, missing) = draw_title(&mut cache, &stack, LONG_TITLE, 120.0, Some(2), Some(0));
    let expected: Vec<char> =
        drawn.chars().filter(|character| !character.is_whitespace()).collect();
    assert!(!expected.is_empty(), "the cut has visible characters");
    assert_eq!(missing, expected, "the failed cut is reported exactly once");
}

/// An ellipsis that fails to measure leaves a cut that still shapes, so the title draws and nothing
/// is reported.
#[test]
fn a_failed_ellipsis_measure_with_a_shaped_cut_reports_nothing() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let (drawn, missing) = draw_title(&mut cache, &stack, LONG_TITLE, 120.0, Some(1), None);
    assert!(!drawn.is_empty(), "a cut is drawn");
    assert!(missing.is_empty(), "the shaped cut drew: {missing:?}");
}

/// A kept title draws from its run without shaping, so an armed failure never reports it; once its
/// key changes it is fitted again, and a whole-title failure then is reported once.
#[test]
fn a_kept_title_whose_key_changes_and_then_fails_is_reported_once() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let (_, healthy) = draw_title(&mut cache, &stack, "fits", 400.0, None, None);
    assert!(healthy.is_empty() && cache.is_stored(0), "the healthy title is kept");
    let (_, hit) = draw_title(&mut cache, &stack, "fits", 400.0, Some(0), None);
    assert!(hit.is_empty(), "a kept title never shapes, so it is never reported: {hit:?}");
    let (drawn, changed) = draw_title(&mut cache, &stack, "fits", 390.0, Some(0), None);
    assert_eq!(drawn, "", "the refitted title failed to shape");
    assert_eq!(changed, vec!['f', 'i', 't', 's'], "and is reported once");
}

/// With reuse off every title draws from a fresh fit; a whole title that shaped and fits draws, so
/// nothing is reported. This is the path a healthy fit's own outcome reaches the draw boundary on.
#[test]
fn a_fresh_title_that_shaped_reports_nothing() {
    let _lock = font_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut cache = TitleCache::default();
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(256, 256);
    let scope = crate::chrome_text::MissingChromeScope::enter();
    let draw = cache.prepare(0, &probe("fits", 400.0, 0), &stack, false);
    assert!(matches!(draw, TitleDraw::Fresh(_)), "reuse off draws a fresh fit");
    let drawn = cache.drawn(&draw);
    let placement = TitlePlacement {
        color: ChromeColor::WHITE,
        raster_px: 15.0,
        origin: (0.0, 20.0),
        screen: (400.0, 100.0),
        clip: None,
    };
    let layout = layout_title(&stack, &mut raster, &mut atlas, &drawn, placement);
    assert!(!layout.glyphs.is_empty(), "the title draws");
    assert!(scope.finish().is_empty(), "and nothing is reported");
}
