//! Renderer assemble contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// Frame assembly starts one glyph cache pass before the pane loop; each pane's pin phase pins
/// every emitted row's key before its first admission; the cache-hit and miss paths of
/// `emit_row_glyphs` each record exactly one span, through the shared projection; and the tab
/// titles are appended after the cursor recolors and before search recolor.
#[test]
fn row_spans_viewports_and_title_order_follow_the_assembly() {
    let core: String = crate::core::CORE_SOURCE.split_whitespace().collect();
    let glyph_phase: String =
        include_str!("core_assemble_glyph_rows.rs").split_whitespace().collect();
    let begin = glyph_phase
        .find("begin_glyph_pass(&mutself.row_glyph_cache,&mutself.row_ink,")
        .expect("one pass start");
    let pane_loop = glyph_phase
        .find("for(pane_index,pv)inpane_views.iter().enumerate().filter(|(_,pane)|pane.planned.full_clip.is_some()){")
        .expect("per-pane loop");
    assert!(begin < pane_loop, "the pass starts before any pane pins or admits");
    let seam = core.find("pub(crate)fnassemble_pane_glyph_rows(").expect("pane seam");
    let pin = seam + core[seam..].find("shaping.row_cache.pin(pane_id,keys);").expect("pin");
    let first_emit = seam + core[seam..].find("emit_row_glyphs(").expect("emit");
    assert!(pin < first_emit, "every key is pinned before the first admission");
    // One projection site records a row's span; the other is the test row-glyph seam's own.
    assert_eq!(core.matches("row_spans.push(RowGlyphSpan::new(").count(), 2);
    let injected = core.find("fnpush_injected_row_glyph(").expect("row glyph seam");
    assert!(core[injected..].find("row_spans.push(RowGlyphSpan::new(").is_some());
    let project = core.find("pub(crate)fnproject_cached_row(").expect("shared projection");
    assert!(core[project..].find("frame.row_spans.push(RowGlyphSpan::new(").is_some());
    let emit = core.find("pub(crate)fnemit_row_glyphs(").expect("emit_row_glyphs");
    let body = &core[emit..];
    let hit = body.find("project_cached_row(cached,&at,").expect("hit projection");
    let hit_return = body.find("returntrue;").expect("hit return");
    let miss = body.find("project_cached_row(&records,&at,").expect("miss projection");
    let insert = body.find("row_cache.insert(").expect("miss admission");
    assert!(hit < hit_return && hit_return < miss && miss < insert, "one span on each path");
    let calls: Vec<usize> = core
        .match_indices("recolor_cursor_glyphs_in(&mut*glyph_instances")
        .map(|(at, _)| at)
        .collect();
    let assembly = method_body(crate::core::CORE_SOURCE, "    pub(super) fn assemble_frame(");
    let selection = assembly.find("self.assemble_selection(").unwrap();
    let titles = assembly.find("self.assemble_tabs(").unwrap();
    let search = assembly.find("self.assemble_search(").unwrap();
    assert!(include_str!("core_assemble_tabs.rs")
        .contains("glyph_instances.extend(final_layout.glyphs);"));
    assert_eq!(calls.len(), 3);
    assert!(selection < titles && titles < search, "cursor recolors, titles, then search");
}

/// The test glyph seam is appended after every terminal row and the scrollbar and before the
/// selection, copy-mode and block-cursor recolors, so those recolors see it as a real glyph; the
/// presented-damage readout is written beside the frame key, so it describes presented pixels.
/// Line endings are normalized before scanning.
#[test]
fn the_injected_glyph_precedes_every_cursor_recolor_and_damage_is_read_beside_the_key() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let assembly =
        source.split_once("    pub(super) fn assemble_frame(").expect("assemble_frame exists").1;
    let backgrounds = include_str!("core_assemble_backgrounds.rs");
    let scrollbar = backgrounds.find("emit_pane_scrollbar(").expect("scrollbar emit");
    let inject = backgrounds.find("self.push_injected_test_glyph(").expect("the seam is called");
    let selection = assembly.find("self.assemble_selection(").expect("selection phase");
    let first_recolor = include_str!("core_assemble_selection.rs")
        .find("recolor_cursor_glyphs_in(")
        .expect("a cursor recolor");
    let background_call = assembly.find("self.assemble_backgrounds(").unwrap();
    assert!(scrollbar < inject && background_call < selection);
    assert!(first_recolor > 0);
    let finish =
        source.split_once("    pub(super) fn finish_successful_frame(").expect("finish exists").1;
    let readout = finish.find("self.presented_damage.record(").expect("damage readout");
    // The key is kept by the settlement seam, which finishing calls right after the readout.
    let key = finish.find("settle_retained_frame(").expect("key settled");
    assert!(readout < key, "the readout is written beside the frame key");
}

/// Production wiring of tab-title ink: assembly measures the glyphs the tab bar emitted, widens
/// the plan's damage by the last presented and the current title ink before the layers carry it,
/// and keeps the current ink only after the `Presented` guard, beside the recolor record. Line
/// endings are normalized before scanning.
#[test]
fn tab_title_ink_is_measured_widened_and_kept_only_by_a_presented_frame() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let assembly =
        source.split_once("    pub(super) fn assemble_frame(").expect("assemble_frame exists").1;
    let tabs = include_str!("core_assemble_tabs.rs");
    let start = tabs.find("let tab_glyph_start = glyph_instances.len();").expect("start");
    let bar = tabs.find("        if self.tab_bar_visible {\n").expect("tab bar block");
    let measure = tabs.find("glyph_ink_bounds(&glyph_instances[tab_glyph_start..]").expect("ink");
    let search = assembly.find("self.assemble_search(").expect("search phase");
    let tab_phase = assembly.find("self.assemble_tabs(").unwrap();
    assert!(start < bar && bar < measure && tab_phase < search);
    let handoff = assembly.find("previous_tab_ink: self.last_tab_ink,").expect("handed off");
    assert!(assembly.contains("current_tab_ink: tab_ink,"));
    let call = assembly.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    assert!(handoff < call, "the title ink reaches the shared pass-end helper");
    let helper = method_body(&source, "    pub(super) fn finish_assembly_pass(");
    let widen = helper
        .find("plan.widen_for_tab_ink(pass.previous_tab_ink, pass.current_tab_ink);")
        .expect("widen");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").expect("receipts");
    assert!(widen < receipts, "damage is widened before the layers carry it");
    let present = source.split_once("    pub(super) fn present_layers(").expect("present_layers").1;
    let guard = present.find("if !matches!(outcome, PresentOutcome::Presented)").expect("guard");
    let kept = present.find("self.last_tab_ink = tab_ink;").expect("ink kept");
    assert!(guard < kept, "only a presented frame keeps its title ink");
}

/// A Partial plan whose widened damage reaches an unemitted row falls back through the
/// production orchestration and production pass helpers (`begin_glyph_pass` and
/// `finish_assembly_pass`), with real admissions, real atlas checks and real receipts, for the
/// last frame's cursor recolor and for its tab-title ink over slot 0. The
/// first pass admits and stages the edited row but commits nothing; the second pass's own
/// start discards that stage; its layers carry the forced-Full plan and `presented_receipts`
/// gives that plan's `All` receipt for the fixture grid; and the one settlement commits the
/// second pass's keys. A real atlas reset or growth during the first pass (one assembly call)
/// or the second (two calls) settles as `AtlasRetry`: no receipt, committed keys unchanged and
/// no surviving stage.
#[test]
fn a_partial_fallback_never_commits_its_first_pass() {
    use sonicterm_render_model::{AckReceipt, AckRows};
    for widening in [Widening::Recolor, Widening::TabInk] {
        let mut fixture = FallbackFixture::new();
        assert_eq!(fixture.plan(false).mode, RenderMode::Partial, "{widening:?}: plans Partial");
        let mut calls = 0;
        let assembled = assemble_with_fallback(|force_full| {
            calls += 1;
            fixture.assemble(force_full, AtlasChange::Unchanged, widening)
        });
        assert_eq!(calls, 2, "{widening:?}: the partial pass fell back and was assembled again");
        let Ok(Assembled::Layers(layers)) = assembled else {
            panic!("{widening:?}: the forced-Full pass presents layers");
        };
        let AssembledLayers { plan, receipts, .. } = *layers;
        assert_eq!(plan.mode, RenderMode::Full, "{widening:?}: the layers carry the Full plan");
        assert_eq!(fixture.stages_at_pass_start[0], vec![0; 6], "{widening:?}: starts empty");
        assert_ne!(fixture.staged()[2], 0, "{widening:?}: the second pass staged the edit");
        assert_eq!(
            fixture.stages_at_pass_start[1],
            vec![0; 6],
            "{widening:?}: the second start discards the first stage"
        );
        for (pass, committed) in fixture.committed_after_pass.iter().enumerate() {
            assert_eq!(*committed, fixture.committed_before, "{widening:?}: pass {pass} commits");
        }
        let expected_receipts = vec![AckReceipt::of(0, 7, &fixture.grid, AckRows::All)];
        assert_eq!(receipts, expected_receipts, "{widening:?}: the Full plan acknowledges all");
        let staged = fixture.staged();
        let mut key = Some(fixture.warm.key.clone());
        let settled = settle_retained_frame(
            &mut key,
            &mut fixture.ink,
            &mut fixture.rig.cache,
            &PresentOutcome::Presented,
            Some(plan),
            receipts,
        );
        assert_eq!(settled, expected_receipts, "{widening:?}: one settlement returns them");
        assert_eq!(fixture.committed(), staged, "{widening:?}: the second pass's keys commit");
        assert_ne!(fixture.committed()[2], fixture.committed_before[2]);
        assert_eq!(fixture.staged(), vec![0; 6], "{widening:?}: no stage survives the commit");
    }

    for retry_pass in [1usize, 2] {
        for change in [AtlasChange::Reset, AtlasChange::Growth] {
            let case = format!("{change:?} in pass {retry_pass}");
            let mut fixture = FallbackFixture::new();
            let mut calls = 0;
            let assembled = assemble_with_fallback(|force_full| {
                calls += 1;
                let pass_change = if calls == retry_pass { change } else { AtlasChange::Unchanged };
                fixture.assemble(force_full, pass_change, Widening::TabInk)
            });
            assert_eq!(calls, retry_pass, "{case}: assembly calls");
            assert!(matches!(assembled, Ok(Assembled::AtlasRetry { .. })), "{case}: AtlasRetry");
            let mut key = Some(fixture.warm.key.clone());
            let settled = settle_retained_frame(
                &mut key,
                &mut fixture.ink,
                &mut fixture.rig.cache,
                &PresentOutcome::AtlasRetry,
                None,
                Vec::new(),
            );
            assert!(settled.is_empty() && key.is_none(), "{case}: no receipt and no key");
            assert_eq!(fixture.committed(), fixture.committed_before, "{case}: unchanged");
            assert_eq!(fixture.staged(), vec![0; 6], "{case}: no surviving stage");
        }
    }
}

/// The pane seam records each slot at the moment its row is emitted, in ascending order and
/// exactly as the plan's mask says: every slot for a Full plan, and only the masked slots for
/// the Partial edits of F0 and F20. The record comes from the emission loop, not the plan, so a
/// loop that skips, adds or reorders a row fails here on every host.
#[test]
fn the_pane_seam_records_exactly_the_slots_it_emits() {
    for (pad, mask) in [(0.0, vec![2u16]), (20.0, vec![0, 1, 2, 3, 4])] {
        let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
        let mut rig = GlyphRig::new(false);
        let mut ink = crate::row_ink::RowInkTable::default();
        let warm = policy_plan(6, pad, 1, Vec::new(), Vec::new(), None);
        begin_pass(&mut rig, &mut ink, &grid, &warm);
        let (_, _, full_slots) = emit_pass_recording(&mut rig, &mut ink, &grid, &warm);
        assert_eq!(full_slots, (0..6).collect::<Vec<u16>>(), "pad {pad}: Full emits every slot");
        present_pass(&mut rig, &mut ink, &warm);
        write_row(&mut grid, 2, "EDIT");
        let edit = policy_plan(6, pad, 2, vec![2], strip_records(6), Some(&warm.key));
        assert_eq!(edit.mode, RenderMode::Partial, "pad {pad}");
        begin_pass(&mut rig, &mut ink, &grid, &edit);
        let (_, _, partial_slots) = emit_pass_recording(&mut rig, &mut ink, &grid, &edit);
        assert_eq!(partial_slots, mask, "pad {pad}: Partial emits exactly the mask");
    }
}

/// Both direct `Err` exits discard the staged glyph slot keys: the assembly error arm and the
/// presenter `Err` handled where `present_layers` returns, besides the partial-fallback arm.
/// The assembly fault fires after the pane loop staged its rows. Exercised on a real renderer
/// in the Windows partial-assembly suite.
#[test]
fn both_error_exits_discard_staged_glyph_slots() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let releasing = method_body(&source, "    pub fn render_releasing(");
    assert_eq!(releasing.matches("self.row_glyph_cache.discard_staged();").count(), 3);
    let error_arm = releasing.find("Err(error) => {").unwrap();
    assert!(releasing[error_arm..error_arm + 400].contains("discard_staged()"));
    let layers = releasing.find("self.present_layers(*layers).unwrap_or_else(|error| {").unwrap();
    assert!(releasing[layers..layers + 200].contains("discard_staged()"));
    let assemble = method_body(&source, "    pub(super) fn assemble_frame(");
    let staged = assemble.find("self.assemble_glyph_rows(").unwrap();
    assert!(include_str!("core_assemble_glyph_rows.rs").contains("staged_ranges.push((pv.pane_id,"));
    let fault = assemble.find("std::mem::take(&mut self.fault_assembly_error)").unwrap();
    assert!(staged < fault, "the assembly fault fires after rows were staged");
}

/// Plans ignore the glyph cache: the same facts, inputs and previous key give the same plan
/// whatever the cache holds, and the planner's source never names the cache.
#[test]
fn plans_ignore_the_glyph_cache() {
    let warm = policy_plan(6, 20.0, 1, Vec::new(), Vec::new(), None);
    let before = policy_plan(6, 20.0, 2, vec![2], strip_records(6), Some(&warm.key));
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
    assemble_pass(&mut rig, &mut ink, &grid, &warm);
    let after = policy_plan(6, 20.0, 2, vec![2], strip_records(6), Some(&warm.key));
    assert_eq!(before.key, after.key);
    assert_eq!(before.mode, after.mode);
    assert_eq!(before.damage, after.damage);
    assert_eq!(before.panes[0].emit_rows, after.panes[0].emit_rows);
    let planner = include_str!("frame_plan.rs");
    assert!(!planner.contains("row_glyph_cache") && !planner.contains("RowGlyphCache"));
}

#[test]
fn run_flush_clones_no_cells() {
    // Style runs borrow the grid's cells: the row walk records runs as slices of borrowed cells
    // and the run builder finds a cluster's lead cell by column without a cloned lookup table.
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let emit = item_body(&source, "pub(crate) fn emit_row_glyphs(");
    assert!(!emit.contains("(*cell).clone()"), "emit_row_glyphs clones no cell into a run");
    assert!(!emit.contains("Vec<(u16, Cell)>"), "emit_row_glyphs keeps no owned run");
    let build = item_body(&source, "fn build_shape_run(");
    assert!(!build.contains("cell_by_col"), "build_shape_run builds no column-to-cell table");
    assert!(!build.contains("c.clone()"), "build_shape_run clones no cell");
}
