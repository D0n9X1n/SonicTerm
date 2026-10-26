//! Renderer contracts contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// Only a presented plan's exact revision can clear dirt; a subsequent mutation or replacement stays dirty.
#[test]
fn planned_acknowledgement_rejects_newer_grid_and_replacement() {
    use sonicterm_render_model::{CursorStyle, PaneRender};
    let mut grid = Grid::new(8, 2);
    let plan = revision_plan(7, grid.revision());
    grid.put_char('X', Color::Default, Color::Default, CellFlags::empty());
    let mut panes = [PaneRender {
        id: 7,
        rect_px: PixelRect { x: 0, y: 0, w: 80, h: 40 },
        grid: &mut grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::default(),
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    acknowledge_plan(&plan, &mut panes);
    assert!(panes[0].grid.dirty_count() > 0);
    let current = revision_plan(7, panes[0].grid.revision());
    panes[0].id = 8;
    acknowledge_plan(&current, &mut panes);
    assert!(panes[0].grid.dirty_count() > 0);
    panes[0].id = 7;
    acknowledge_plan(&current, &mut panes);
    assert_eq!(panes[0].grid.dirty_count(), 0);
    panes[0].grid.mark_all_dirty();
    let mut noop = revision_plan(7, panes[0].grid.revision());
    noop.mode = RenderMode::Noop;
    acknowledge_plan(&noop, &mut panes);
    assert!(panes[0].grid.dirty_count() > 0, "unpresented plans cannot acknowledge dirt");
}

/// Production retry exits precede plan acknowledgement and both presenters finish only after their success boundary.
#[test]
fn production_frame_decisions_use_one_plan_and_preserve_retry_boundaries() {
    let assembly = include_str!("core_assemble.rs");
    let render = concat!(
        include_str!("core_render.rs"),
        include_str!("core_assemble.rs"),
        include_str!("core_assemble_backgrounds.rs"),
        include_str!("core_assemble_inline_images.rs")
    );
    assert!(assembly.contains("FramePlan::build("));
    assert_eq!(render.matches("FramePlan::build(").count(), 1);
    for redundant in [
        "Self::resolved_view_top_abs(",
        "pane_damage_rect_with_ink_pad(",
        "decide_render_mode(",
        "copy_mode.cloned()",
    ] {
        assert!(!render.contains(redundant), "production recomputes {redundant}");
    }
    assert!(render.contains("plan.damage"));
    assert!(render.contains("pv.planned.content_clip"));
    assert!(render.contains("pv.planned.rows()"));
    // The frame finishes once, and only after its presenter reports `Presented`; the strip layout
    // settles at the presenter's outcome, before an error propagates.
    let presented = render.find("self.present_frame(&layers, &mut gpu_timing);").unwrap();
    let settled = render
        .find("self.status_line_layouts.settle(matches!(outcome, Ok(PresentOutcome::Presented)));")
        .unwrap();
    let handoff = render.find("let outcome = outcome?;").unwrap();
    assert!(presented < settled && settled < handoff);
    let guard = render[handoff..].find("PresentOutcome::Presented)").unwrap() + handoff;
    let finish = render.find("self.finish_successful_frame(").unwrap();
    assert_eq!(render.matches("self.finish_successful_frame(").count(), 1);
    assert!(handoff < guard && guard < finish);
    // Each presenter in `present.rs` reports `Presented` only after its success
    // boundary, never acknowledges a plan itself, and every surface exit precedes
    // submission.
    let presenters = include_str!("present.rs").replace("\r\n", "\n");
    assert!(!presenters.contains("finish_successful_frame"));
    // Atlases clear their own dirty rects; a presenter never clears grid dirt.
    for grid_clear in ["acknowledge_receipts", "grid.clear_dirty()", "clear_dirty_rows("] {
        assert!(!presenters.contains(grid_clear), "a presenter calls {grid_clear}");
    }
    assert_eq!(presenters.matches("Ok(PresentOutcome::Presented)").count(), 2);
    let software = presenters
        .find("crate::software_windows::present_frame(frame, &self.window)?;\n        lap(timing, \"software_present\");")
        .unwrap();
    let software_done =
        presenters[software..].find("Ok(PresentOutcome::Presented)").unwrap() + software;
    let submit = presenters.find("self.queue.submit(").unwrap();
    let present = presenters.find("self.queue.present(frame);").unwrap();
    let done = presenters[present..].find("Ok(PresentOutcome::Presented)").unwrap() + present;
    assert!(
        software < software_done && software_done < submit && submit < present && present < done
    );
    for state in ["Timeout", "Occluded", "Outdated", "Suboptimal", "Lost", "Validation"] {
        let branch = presenters.find(&format!("wgpu::CurrentSurfaceTexture::{state}")).unwrap();
        assert!(branch < submit, "{state} must leave the frame before submission");
    }
}

/// The compatibility wrapper's acknowledgement applies receipts through the borrowed grids: a subset
/// receipt clears only its rows, a receipt naming another pane clears nothing, a receipt taken before a
/// later mark applies but keeps every row that mark dirtied, and an `All` receipt clears every row.
#[test]
fn borrowed_acknowledgement_clears_only_matching_receipts_and_their_rows() {
    use sonicterm_render_model::{AckReceipt, AckRows, CursorStyle, PaneRender};
    let mut grid = Grid::new(8, 3);
    grid.mark_all_dirty();
    let subset = AckReceipt::of(0, 7, &grid, AckRows::Rows([1].into_iter().collect()));
    let other = AckReceipt { pane_id: 8, ..AckReceipt::of(0, 7, &grid, AckRows::All) };
    let mut panes = [PaneRender {
        id: 7,
        rect_px: PixelRect { x: 0, y: 0, w: 80, h: 60 },
        grid: &mut grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::default(),
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    assert_eq!(acknowledge_receipts(&[other], &mut panes), 0, "another pane's receipt");
    assert_eq!(panes[0].grid.dirty_count(), 3);
    assert_eq!(acknowledge_receipts(std::slice::from_ref(&subset), &mut panes), 1);
    assert_eq!(panes[0].grid.dirty_rows().collect::<Vec<_>>(), [0, 2]);
    panes[0].grid.mark_all_dirty();
    assert_eq!(acknowledge_receipts(&[subset], &mut panes), 1, "the receipt still applies");
    assert_eq!(panes[0].grid.dirty_count(), 3, "a later mark keeps all dirt");
    let all = AckReceipt::of(0, 7, &*panes[0].grid, AckRows::All);
    assert_eq!(acknowledge_receipts(&[all], &mut panes), 1);
    assert_eq!(panes[0].grid.dirty_count(), 0);
}

/// The compatibility wrapper's settlement keeps the borrowed grid's dirt for every outcome but
/// `Presented`, even when the frame carries a matching receipt; a surface retry is the case a
/// renderer-free host cannot otherwise reach, so it is checked for each retry reason.
#[test]
fn settling_a_borrowed_frame_clears_dirt_only_when_presented() {
    use sonicterm_render_model::{AckReceipt, AckRows, CursorStyle, PaneRender};
    let not_presented = || {
        vec![
            PresentOutcome::SurfaceRetry(SurfaceRetryReason::Occluded),
            PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout),
            PresentOutcome::SurfaceRetry(SurfaceRetryReason::Outdated),
            PresentOutcome::AtlasRetry,
            PresentOutcome::CachedReblit,
            PresentOutcome::Skipped(SkipReason::Noop),
            PresentOutcome::Failed(anyhow::anyhow!("presenter failed")),
        ]
    };
    let mut cases: Vec<(PresentOutcome, bool)> =
        not_presented().into_iter().map(|outcome| (outcome, false)).collect();
    cases.push((PresentOutcome::Presented, true));
    for (outcome, clears) in cases {
        let label = format!("{outcome:?}");
        let mut grid = Grid::new(8, 3);
        grid.mark_all_dirty();
        // A receipt that matches every identity, so only the outcome decides whether it applies.
        let receipt = AckReceipt::of(0, 7, &grid, AckRows::All);
        let mut panes = [PaneRender {
            id: 7,
            rect_px: PixelRect { x: 0, y: 0, w: 80, h: 60 },
            grid: &mut grid,
            viewport_top_abs: None,
            is_active: true,
            cursor_style: CursorStyle::default(),
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: Vec::new(),
        }];
        let frame = FrameOutcome { outcome, receipts: vec![receipt] };
        let settled = settle_borrowed_frame(frame, &mut panes);
        assert_eq!(format!("{settled:?}"), label, "settlement returns the frame's outcome");
        let expected_dirty = if clears { 0 } else { 3 };
        assert_eq!(panes[0].grid.dirty_count(), expected_dirty, "{label}");
    }
}

/// The compatibility wrapper settles through `settle_borrowed_frame` and applies no receipt of its
/// own, so the settlement rule above is the rule the real `render_with_outcome` path follows.
#[test]
fn the_compatibility_wrapper_settles_only_through_settle_borrowed_frame() {
    let core = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let wrapper = core.split_once("    pub fn render_with_outcome(").expect("wrapper").1;
    let wrapper = wrapper.split_once("\n    }\n").expect("wrapper body").0;
    assert!(wrapper.trim_end().ends_with("settle_borrowed_frame(frame, panes)"), "{wrapper}");
    assert!(!wrapper.contains("acknowledge_receipts"), "the wrapper acknowledges on its own");
}

/// One call assembles inside `lend` and presents only after it returns: no public split API exists,
/// `Assembled` is private and borrows nothing, and assembly decides the empty and stopped exits
/// first, in their existing order, without reaching the device or a presenter.
#[test]
fn render_releasing_lends_once_and_presents_after_release() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    for split in ["pub fn assemble", "present_assembled", "AssembledFrame", "pub enum Assembled"] {
        assert!(!source.contains(split), "a split API remains: {split}");
    }
    assert!(
        source.contains("\npub(super) enum Assembled {\n"),
        "Assembled is private and has no lifetime"
    );
    let call = source.split_once("    pub fn render_releasing(").unwrap().1;
    let call = call.split_once("\n    }\n").unwrap().0;
    // The call lends through `lend_and_assemble` once, and that function lends its source once.
    assert_eq!(call.matches("lend_and_assemble(").count(), 1);
    let seam = source.split_once("\npub(super) fn lend_and_assemble(").unwrap().1;
    let seam = seam.split_once("\n}\n").unwrap().0;
    assert_eq!(seam.matches("source.lend(").count(), 1);
    let lend = call.find("lend_and_assemble(").unwrap();
    for after in [
        "self.flush_image_upload_rebuild();",
        "self.present_layers(",
        "self.prepare_cached_present()",
        "self.reset_glyph_atlas_after_invalidation(",
        "self.rendering_unavailable()",
    ] {
        assert!(call.find(after).is_some_and(|at| at > lend), "{after} runs after release");
    }
    let assemble = include_str!("core_assemble.rs");
    assert!(call.contains("lend_and_assemble(source, accepts_gpu_work,"), "exits decided first");
    assert!(
        !assemble.contains("Assembled::NoPanes") && !assemble.contains("Assembled::Unavailable")
    );
    for presenting in [
        "present_frame(",
        "prepare_cached_present(",
        "enter_gpu_work(",
        "effective_subpixel_aa_mode(",
    ] {
        assert!(!assemble.contains(presenting), "assembly calls {presenting}");
    }
}

/// The glyph texture is resized only once the frame source has released its parser guards: in
/// `render_releasing` the rebuild comes after `lend_and_assemble` returns and before any present,
/// never ahead of the lend, so recreating a texture never blocks PTY parsing.
#[test]
fn render_releasing_resizes_the_glyph_texture_after_release_and_before_present() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let call = source.split_once("    pub fn render_releasing(").unwrap().1;
    let call = call.split_once("\n    }\n").unwrap().0;
    let code: String = call
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let lend = code.find("lend_and_assemble(").expect("the lend");
    let rebuilds: Vec<usize> =
        code.match_indices("self.rebuild_glyph_upload_if_needed();").map(|(at, _)| at).collect();
    assert_eq!(rebuilds.len(), 1, "one rebuild seam in render_releasing");
    assert!(rebuilds[0] > lend, "the rebuild runs after the parser guards are released");
    for present in ["self.present_layers(", "self.prepare_cached_present()"] {
        assert!(rebuilds[0] < code.find(present).unwrap(), "the rebuild precedes {present}");
    }
}

/// A stopped device finalizes its growth episodes before the stopped report can return early, so
/// the App's non-rendering stopped path abandons a pending episode even when it never assembles.
#[test]
fn the_stopped_report_finalizes_growth_episodes_before_any_early_return() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let body = source.split_once("    pub fn take_stopped_render_outcome(").unwrap().1;
    let body = body.split_once("\n    }\n").unwrap().0;
    let finalize = body
        .find("self.finalize_growth_episodes_if_device_stopped();")
        .expect("the stop finalizes");
    let first_return = body.find("return None;").expect("the early return");
    assert!(finalize < first_return, "finalization precedes the early return");
}

/// The renderer-free exits of the one releasing call, through the production seam: an empty source
/// is skipped as `NoPanes` with no receipts whether or not the device accepts work, a stopped
/// device with panes is `Unavailable` and never assembles, and in every case the source is lent
/// once and dropped before the call returns.
#[test]
fn an_empty_source_is_no_panes_and_is_dropped_before_the_call_returns() {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let source = |grids: Vec<Grid>| OwningSource { grids, log: std::rc::Rc::clone(&log) };
    for accepts_gpu_work in [true, false] {
        log.borrow_mut().clear();
        let assembled = lend_and_assemble(source(Vec::new()), accepts_gpu_work, |_| {
            panic!("an empty source never assembles")
        })
        .unwrap();
        assert_eq!(*log.borrow(), ["lend", "drop"], "dropped before the call returns");
        let outcome = settle_without_renderer(assembled).ok().expect("an empty source is settled");
        assert!(
            matches!(outcome.outcome, PresentOutcome::Skipped(SkipReason::NoPanes)),
            "accepts={accepts_gpu_work}: {:?}",
            outcome.outcome
        );
        assert!(outcome.receipts.is_empty());
    }
    log.borrow_mut().clear();
    let stopped = lend_and_assemble(source(vec![Grid::new(8, 2)]), false, |_| {
        panic!("a stopped device never assembles")
    })
    .unwrap();
    assert!(matches!(stopped, Assembled::Unavailable));
    assert_eq!(*log.borrow(), ["lend", "drop"]);
    assert!(
        settle_without_renderer(stopped).is_err(),
        "a stopped device needs the renderer's report"
    );
    let mut assembled_panes = 0;
    let usable = lend_and_assemble(source(vec![Grid::new(8, 2), Grid::new(8, 2)]), true, |panes| {
        assembled_panes = panes.len();
        Ok(Assembled::Unavailable)
    });
    assert!(usable.is_ok());
    assert_eq!(assembled_panes, 2, "a usable device assembles the lent panes once");
}

/// The production two-pass orchestration: a first pass that reports a partial fallback is
/// assembled again as forced Full. As `assemble_frame` plans them, the first pass reads valid
/// records and plans `Partial`; the forced pass reads none, so its build already plans `Full`.
/// The frame counts one fallback and one full frame, and one assembly sample of both passes'
/// summed time (40 + 30 = 70 us, in the 50-100 bucket).
#[test]
fn a_partial_fallback_is_counted_once_with_one_summed_assembly_sample() {
    let first =
        FramePlan::build(fallback_facts(), [fallback_pane(1, Vec::new(), Vec::new())], None);
    let key = first.key.clone();
    let empty = PixelRect { x: 0, y: 0, w: 0, h: 0 };
    let sink = crate::frame_stats::FrameStatsSink::default();
    let mut passes = Vec::new();
    let result = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        assemble_with_fallback(|force_full| {
            passes.push(force_full);
            let records = if force_full { Vec::new() } else { vec![Some(empty); 4] };
            let mut plan = FramePlan::build(
                fallback_facts(),
                [fallback_pane(2, vec![1], records)],
                Some(&key),
            );
            if force_full {
                plan.force_full();
            }
            crate::frame_stats::note_assembly_us(if force_full { 30 } else { 40 });
            if force_full {
                assert_eq!(plan.mode, RenderMode::Full);
                Ok(Assembled::Unchanged { focus_flash: false })
            } else {
                // When: the first pass is the partial plan, its final damage reached unemitted ink.
                assert_eq!(plan.mode, RenderMode::Partial);
                Ok(Assembled::PartialFallback)
            }
        })
    };
    assert_eq!(passes, [false, true]);
    assert!(matches!(result, Ok(Assembled::Unchanged { focus_flash: false })));
    let stats = sink.snapshot();
    assert_eq!((stats.partial_fallbacks, stats.full_frames, stats.partial_frames), (1, 1, 0));
    assert_eq!(stats.assembly_buckets, [0, 0, 1, 0, 0, 0, 0]);
    assert_eq!(stats.assembly_sum_us, 70);
}

/// An ordinary frame is one pass: no fallback, and one assembly sample of its own time. A pass
/// timed outside a counting scope leaves nothing pending for the next counted frame.
#[test]
fn an_ordinary_frame_records_one_assembly_sample_and_no_fallback() {
    // Timed with the gate off and never closed: none of it may reach the next counted frame.
    crate::frame_stats::note_assembly_us(500);
    let sink = crate::frame_stats::FrameStatsSink::default();
    let mut passes = Vec::new();
    let result = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        assemble_with_fallback(|force_full| {
            passes.push(force_full);
            crate::frame_stats::note_assembly_us(40);
            Ok(Assembled::Unchanged { focus_flash: false })
        })
    };
    assert_eq!(passes, [false]);
    assert!(matches!(result, Ok(Assembled::Unchanged { .. })));
    let stats = sink.snapshot();
    assert_eq!(stats.partial_fallbacks, 0);
    assert_eq!(stats.assembly_buckets, [0, 1, 0, 0, 0, 0, 0]);
    assert_eq!(stats.assembly_sum_us, 40);
}

/// A frame's outcome settles what it leaves retained, through the one production seam. Starting
/// from committed records and a staged replacement of a partial plan: `Presented` commits the
/// replacement, counts one partial frame, keeps the plan's key and returns its receipts, and the
/// next edit plans `Partial` against that key. Timeout, `AtlasRetry` and `RenderingUnavailable`
/// keep the committed records, discard the staged one, count nothing, return no receipt and clear
/// the key, so the retry plans a `Full` first frame.
#[test]
fn every_frame_outcome_settles_records_receipts_counts_and_the_key() {
    use crate::device_errors::{DeviceGate, DeviceState};
    use crate::row_ink::{RowInk, RowInkTable};
    use sonicterm_render_model::{AckReceipt, AckRows};
    let strip = |slot: i32| PixelRect { x: 0, y: 2 + 20 * slot, w: 100, h: 20 };
    let records = || (0..4).map(|slot| Some(strip(slot))).collect::<Vec<_>>();
    let first =
        FramePlan::build(fallback_facts(), [fallback_pane(1, Vec::new(), Vec::new())], None);
    let edit = |key: Option<&FrameKey>, revision| {
        FramePlan::build(fallback_facts(), [fallback_pane(revision, vec![1], records())], key)
    };
    let replacement = PixelRect { x: 0, y: 12, w: 100, h: 40 };
    let retained = || {
        let mut table = RowInkTable::default();
        table.begin_frame();
        for slot in 0..4u16 {
            let rect = strip(i32::from(slot));
            table.stage(7, slot, RowInk { rect, abs_row: u64::from(slot), content_seq: Some(1) });
        }
        table.commit(&[(7, 4)]);
        table.begin_frame();
        table.stage(7, 1, RowInk { rect: replacement, abs_row: 1, content_seq: Some(2) });
        table
    };
    // The glyph cache mirrors the ink table: slot 1 committed with key 11, key 22 staged for it.
    let glyph_slots = || {
        let mut cache = sonicterm_text::row_glyph_cache::RowGlyphCache::new();
        cache.begin_frame(&[(7, 4, 8)]);
        cache.stage_slot(7, 1, 11);
        cache.commit_slots();
        cache.begin_frame(&[(7, 4, 8)]);
        cache.stage_slot(7, 1, 22);
        cache
    };
    let receipts =
        || vec![AckReceipt::of(0, 7, &Grid::new(8, 4), AckRows::Rows([1].into_iter().collect()))];
    let stopped = || {
        PresentOutcome::RenderingUnavailable(SuspendedContext {
            generation: 1,
            gate: DeviceGate { state: DeviceState::Unusable, destroy_requested: false },
            reports_stop: true,
        })
    };
    let unpresented: [(&str, fn() -> PresentOutcome); 2] = [
        ("timeout", || PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout)),
        ("atlas retry", || PresentOutcome::AtlasRetry),
    ];
    let mut cases: Vec<(&str, PresentOutcome)> =
        unpresented.iter().map(|(name, outcome)| (*name, outcome())).collect();
    cases.push(("rendering unavailable", stopped()));
    for (name, outcome) in cases {
        let plan = edit(Some(&first.key), 2);
        assert_eq!(plan.mode, RenderMode::Partial, "{name}: the failing frame is partial");
        let mut key = Some(first.key.clone());
        let mut table = retained();
        let mut glyphs = glyph_slots();
        let sink = crate::frame_stats::FrameStatsSink::default();
        let settled = {
            let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
            settle_retained_frame(&mut key, &mut table, &mut glyphs, &outcome, None, receipts())
        };
        assert_eq!(glyphs.staged_slot(7, 1), Some(0), "{name}: the staged glyph key is discarded");
        assert_eq!(glyphs.committed_slot(7, 1), Some(11), "{name}: the committed key stays");
        assert!(settled.is_empty(), "{name}: no receipt");
        assert_eq!(key, None, "{name}: the key is cleared");
        assert_eq!(sink.snapshot().partial_frames, 0, "{name}: nothing presented");
        assert_eq!(table.committed_rect(7, 1), Some(strip(1)), "{name}: records unchanged");
        table.commit(&[(7, 4)]);
        assert_eq!(table.committed_rect(7, 1), Some(strip(1)), "{name}: the stage is discarded");
        let retry = edit(key.as_ref(), 2);
        assert!(retry.first_frame && retry.mode == RenderMode::Full, "{name}: the retry is Full");
    }

    let plan = edit(Some(&first.key), 2);
    let plan_key = plan.key.clone();
    let mut key = Some(first.key.clone());
    let mut table = retained();
    let mut glyphs = glyph_slots();
    let sink = crate::frame_stats::FrameStatsSink::default();
    let settled = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        settle_retained_frame(
            &mut key,
            &mut table,
            &mut glyphs,
            &PresentOutcome::Presented,
            Some(plan),
            receipts(),
        )
    };
    assert_eq!(settled, receipts(), "a presented frame returns its receipts");
    assert_eq!(key.as_ref(), Some(&plan_key), "the presented plan's key is kept");
    assert_eq!(sink.snapshot().partial_frames, 1);
    assert_eq!(table.committed_rect(7, 1), Some(replacement), "the replacement commits");
    assert_eq!(glyphs.committed_slot(7, 1), Some(22), "the staged glyph key commits");
    let next = edit(key.as_ref(), 3);
    assert!(!next.first_frame && next.mode == RenderMode::Partial, "the next edit is partial");
}

/// The row glyph seam draws whatever was emitted before it: in a partial frame its owner row can
/// be a blank row emitted first, with no glyph in the frame yet, and the seam must still append
/// one glyph of its rectangle as the row's own span, with ink, so the row's record bounds it.
#[test]
fn an_injected_row_glyph_draws_in_a_blank_row_emitted_first() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let seam = InjectedRowGlyph {
        pane_id: 7,
        slot: 10,
        rect_px: (96.0, 200.0, 12.0, 19.0),
        color: [1.0, 0.0, 1.0, 1.0],
    };
    let surface = (640.0, 480.0);
    let mut glyphs = Vec::new();
    let mut row_spans = Vec::new();
    push_injected_row_glyph(&mut atlas, Some(seam), 7, 10, &mut glyphs, &mut row_spans, surface);
    assert_eq!(glyphs.len(), 1, "the seam draws with no glyph emitted before it");
    assert_eq!(row_spans.len(), 1);
    assert_eq!(row_spans[0].ink_px, Some([96.0, 200.0, 108.0, 219.0]));
    let uv = glyphs[0].uv;
    assert!(uv[2] > uv[0] && uv[3] > uv[1], "the glyph samples a resident tile: {uv:?}");
    push_injected_row_glyph(&mut atlas, Some(seam), 7, 11, &mut glyphs, &mut row_spans, surface);
    assert_eq!(glyphs.len(), 1, "another row draws nothing");
}
