//! Renderer render contracts, sharing one root fixture owner.

use super::super::core_tests::*;

#[test]
fn successful_frame_counter_advances_only_after_native_presentation() {
    // Protect runtime smoke from accepting a skipped, occluded, outdated, lost, or failed frame.
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    assert!(CORE_SRC.contains("pub fn successful_frame_count(&self) -> u64"));
    let finish = finish_successful_frame_source(CORE_SRC);
    assert!(finish.contains("self.successful_frame_count ="));
    assert!(finish.contains("saturating_add(1)"));
    assert_eq!(finish.matches("saturating_add(1);").count(), 1);
}

/// The chrome readout follows the terminal rows' missing list frame for frame: `assemble_frame`
/// opens one missing-chrome scope for the whole frame and hands its list to the presented frame,
/// which publishes it beside the rows' list. A frame that does not present publishes neither, and
/// the preedit cache replays its tofu with its glyphs.
#[test]
fn chrome_tofu_is_published_only_by_a_presented_frame() {
    let core_src = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let core_src = core_src.as_str();
    assert!(core_src.contains("pub fn last_missing_chrome(&self) -> &[char]"));
    let assemble = method_body(core_src, "    pub(super) fn assemble_frame(");
    // One frame scope; the preedit cache opens a nested one to capture its own run.
    assert_eq!(
        assemble
            .matches("let missing_chrome_scope = chrome_text::MissingChromeScope::enter();")
            .count(),
        1
    );
    assert!(assemble.contains("missing_chrome_chars: missing_chrome_scope.finish()"));
    assert!(
        include_str!("core_assemble_preedit.rs").contains("cached.missing_chrome_chars"),
        "a preedit cache hit replays its tofu"
    );
    let finish = finish_successful_frame_source(crate::core::CORE_SOURCE);
    assert!(finish.contains("self.last_missing_chrome_chars = missing_chrome_chars"));
}

/// A Windows checkout with CRLF line endings reads the same as an LF one: the method slice ends at
/// the next method, never at the end of the file.
#[test]
fn the_finish_slice_reads_a_crlf_checkout_as_an_lf_one() {
    let lf_source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let crlf_source = lf_source.replace('\n', "\r\n");
    assert_eq!(
        finish_successful_frame_source(&crlf_source).replace("\r\n", "\n"),
        finish_successful_frame_source(&lf_source)
    );
}

/// Source pin (the renderer path needs a device): every presented frame feeds the completeness
/// certificate, marked `Full` only by its plan's mode, with the plan's scene and the atlas stamp read
/// at present, and before the missing lists move into the renderer's latest readout.
#[test]
fn every_presented_frame_feeds_the_completeness_certificate() {
    let finish = finish_successful_frame_source(crate::core::CORE_SOURCE);
    let record = finish.find("crate::completeness::record_presented(").expect("records the frame");
    for field in [
        "full: render_mode == RenderMode::Full,",
        "scene: plan.key.scene(),",
        "missing_terminal: &missing_chars_this_frame,",
        "missing_chrome: &missing_chrome_chars,",
    ] {
        assert!(finish[record..].contains(field), "{field}");
    }
    let publish =
        finish.find("self.last_missing_chars = missing_chars_this_frame;").expect("publish");
    assert!(record < publish, "the certificate reads the lists before they move");
}

/// Row emission reads the plan's per-slot bitset: no dirty-row scan and no whole-frame flag is
/// left, and both the glyph and the background row loop test `emit_rows[` right after they
/// start. Checked on an LF and a CRLF checkout, as Windows CI checks it out.
#[test]
fn both_row_loops_emit_by_the_planned_bitset() {
    for source in
        [crate::core::CORE_SOURCE.to_owned(), crate::core::CORE_SOURCE.replace('\n', "\r\n")]
    {
        let source = source.replace("\r\n", "\n");
        assert!(!source.contains("dirty_rows.contains("), "no dirty-row scan decides emission");
        assert!(!source.contains("dirty_slots.contains("), "no dirty-slot scan decides emission");
        assert!(!source.contains("emit_full_rows"), "no whole-frame emission flag");
        for marker in
            ["    for (slot, _) in planned.rows() {", "for (r, row_abs) in pv.planned.rows()"]
        {
            let start = source.find(marker).unwrap_or_else(|| panic!("{marker}"));
            let head: String = source[start..].lines().take(3).collect();
            assert!(head.contains("planned.emit_rows["), "{marker} tests emit_rows: {head}");
        }
    }
}

/// Ink records describe presented pixels only. Assembly opens a fresh stage before the row loops;
/// the one commit, the one partial-frame count and the key are in the settlement seam's
/// `Presented` arm; a presented frame reaches it through `finish_successful_frame`, after the
/// `Presented` guard, and every other presenter outcome and the atlas retry settle through the
/// same seam. What each outcome settles is asserted by behaviour in
/// `every_frame_outcome_settles_records_receipts_counts_and_the_key`.
#[test]
fn ink_records_and_partial_frames_commit_only_on_a_presented_frame() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    assert_eq!(source.matches("row_ink.commit(").count(), 1, "one commit site");
    assert_eq!(source.matches("row_glyphs.commit_slots();").count(), 1, "one glyph slot commit");
    assert_eq!(source.matches("note_partial_frame(").count(), 1, "one partial-frame count");
    let settle = source.split_once("fn settle_retained_frame(").expect("settlement seam").1;
    let presented = settle.find("(PresentOutcome::Presented, Some(plan)) => {").expect("arm");
    let commit = settle.find("row_ink.commit(").expect("commit");
    let key = settle.find("*last_frame_key = Some(plan.key);").expect("key");
    let glyph_commit = settle.find("row_glyphs.commit_slots();").expect("glyph slot commit");
    let glyph_discard = settle.find("row_glyphs.discard_staged();").expect("glyph slot discard");
    assert!(presented < commit && commit < glyph_commit && glyph_commit < key);
    assert!(key < glyph_discard, "every other outcome discards the staged glyph keys");
    assert_eq!(
        source.matches("settle_retained_frame(\n").count(),
        4,
        "seam, finish, unpresented, retry"
    );
    let finish = method_body(&source, "    pub(super) fn finish_successful_frame(");
    assert!(finish.contains("&PresentOutcome::Presented,"), "finishing settles as presented");
    let present = method_body(&source, "    pub(super) fn present_layers(");
    let guard = present.find("return Ok(FrameOutcome::without_receipts(outcome));").unwrap();
    let unpresented = present.find("settle_retained_frame(").unwrap();
    let finish_call = present.find("self.finish_successful_frame(").unwrap();
    assert!(unpresented < guard && guard < finish_call, "only a presented frame finishes");
    let assemble = method_body(&source, "    pub(super) fn assemble_frame(");
    assert!(assemble.contains("self.assemble_glyph_rows("));
    let glyph_phase = include_str!("core_assemble_glyph_rows.rs");
    let begin = glyph_phase.find("begin_glyph_pass(").expect("a fresh stage per assembly");
    let pass_start = source.split_once("pub(crate) fn begin_glyph_pass<").expect("helper").1;
    let pass_start = &pass_start[..pass_start.find("\n}\n").expect("helper end")];
    assert!(pass_start.contains("row_ink.begin_frame();"), "the shared pass start stages afresh");
    let glyph_loop = glyph_phase.find("assemble_pane_glyph_rows(").unwrap();
    assert!(begin < glyph_loop);
}

/// The post-assembly check runs after both widenings and before the layers carry the damage; a
/// partial plan whose final damage reaches a non-emitted row is reported back, and
/// `render_releasing` reassembles that frame `Full` in the same call.
#[test]
fn a_partial_plan_reaching_unemitted_ink_is_reassembled_full() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let helper = method_body(&source, "    pub(super) fn finish_assembly_pass(");
    let widen = helper.find("plan.widen_for_tab_ink(").unwrap();
    let check = helper.find("plan.partial_reaches_unemitted_ink()").expect("checked");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").unwrap();
    assert!(widen < check && check < receipts);
    assert!(helper.contains("return Err(Assembled::PartialFallback);"));
    let assemble = method_body(&source, "    pub(super) fn assemble_frame(");
    let call = assemble.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    let layers = assemble.find("Ok(Assembled::Layers(Box::new(AssembledLayers {").unwrap();
    assert!(call < layers, "the pass ends in the shared helper before the layers are built");
    assert!(assemble.contains("return Ok(early_exit);"), "its early exit leaves assembly");
    assert!(assemble.contains("plan.force_full();"), "the second pass plans Full");
    let releasing = method_body(&source, "    pub fn render_releasing(");
    assert!(releasing.contains("assemble_with_fallback(|force_full|"), "one orchestration");
    let orchestration = source.split_once("fn assemble_with_fallback(").unwrap().1;
    assert!(orchestration.contains("Ok(Assembled::PartialFallback) =>"), "the fallback is caught");
}

/// The GDI presenter composes every batch into the whole frame and never reads damage, so it
/// must never receive a partial frame; a debug assertion in `present_software_frame` pins it.
#[test]
fn the_software_presenter_asserts_it_never_receives_a_partial_frame() {
    let source = include_str!("present.rs").replace("\r\n", "\n");
    let body = method_body(&source, "    fn present_software_frame(");
    assert!(body.contains("debug_assert!(!layers.partial"), "{body}");
}

/// The seams that fail a frame after its plan keep the frame key when armed, so the failing frame
/// plans against the last presented key and can be partial; the failure paths clear it themselves.
/// The submission seam arms the same probe as `GpuFaultKind::FrameValidation`, which clears the key.
#[test]
fn partial_failure_seams_keep_the_frame_key_when_armed() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    for signature in
        ["    pub fn __fail_next_surface_acquire(", "    pub fn __fail_next_frame_submission("]
    {
        let body = method_body(&source, signature);
        assert!(!body.contains("last_frame_key"), "{signature} touches the key");
        assert!(!body.contains("invalidate_retained_frame"), "{signature} clears the key");
    }
    let submission = method_body(&source, "    pub fn __fail_next_frame_submission(");
    assert!(submission.contains("create_frame_fault_probe(&self.device)"));
}

/// The row glyph seam joins its row: inside the glyph row loop it is pushed after that row's own
/// glyphs and before the row's ink is computed, so it is in the row's span and committed record and
/// is drawn only when that row is emitted.
#[test]
fn an_injected_row_glyph_joins_its_rows_span_and_record() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let start = source.find("pub(crate) fn assemble_pane_glyph_rows(").expect("pane seam");
    let pane = &source[start..start + source[start..].find("\n}\n").expect("seam end")];
    let row_loop = pane.find("    for (slot, _) in planned.rows() {").unwrap();
    let emit = row_loop + pane[row_loop..].find("emit_row_glyphs(").unwrap();
    let inject = pane.find("push_injected_row_glyph(\n").expect("the seam is pushed");
    let ink = pane.find("crate::row_ink::emitted_row_ink(").unwrap();
    assert!(row_loop < emit && emit < inject && inject < ink);
}
