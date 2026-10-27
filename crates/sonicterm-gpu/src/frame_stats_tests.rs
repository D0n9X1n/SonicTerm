//! Pins the renderer's frame statistics: the counting gate, per-renderer collection scopes,
//! shaping counts, damage, the presenter path and the call sites that must open a scope.

pub(super) use std::collections::BTreeSet;
pub(super) use std::path::Path;

pub(super) use sonicterm_render_model::geometry::PixelRect;

use super::*;

#[test]
pub(super) fn notes_outside_a_counting_renderer_record_nothing() {
    // With no counting scope open, or only a non-counting one, no statistic is written.
    let sink = FrameStatsSink::default();
    note_shape_request();
    note_buffer_writes(64, 32);
    note_row_cache(true);
    {
        let _not_counting = CollectGuard::enter(None);
        note_frame(true);
    }
    {
        let _counting = CollectGuard::enter(Some(&sink));
    }
    assert_eq!(sink.snapshot(), FrameStats::ZERO);
}

#[test]
pub(super) fn a_counting_renderer_collects_into_its_own_sink_and_restores_the_enclosing_gate() {
    // Each note lands once in the scope's renderer; a non-counting renderer inside it records
    // nothing, and a note after the scope closes reaches no renderer.
    let sink = FrameStatsSink::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        note_buffer_writes(64, 32);
        note_damage(|| 250);
        note_frame(false);
        note_frame(true);
        note_row_cache(true);
        note_row_cache(false);
        note_row_cache(false);
        {
            let _not_counting = CollectGuard::enter(None);
            note_shape_request();
        }
        note_shape_request();
    }
    note_shape_request();
    let expected = FrameStats {
        vertex_bytes: 64,
        index_bytes: 32,
        damage_permille_sum: 250,
        damaged_frames: 1,
        software_frames: 1,
        gpu_frames: 1,
        row_cache_hits: 1,
        row_cache_misses: 2,
        shape_requests: 1,
        native_request_redraw: 0,
        ..FrameStats::ZERO
    };
    assert_eq!(sink.snapshot(), expected);
}

#[test]
pub(super) fn interleaved_renderers_on_one_thread_keep_their_own_statistics() {
    // Two windows' renderers share the event-loop thread. Each scope's notes belong to the
    // renderer that opened it, whichever renderer draws next, and a snapshot between them
    // already holds the first renderer's notes.
    let (first, second) = (FrameStatsSink::default(), FrameStatsSink::default());
    {
        let _layout = CollectGuard::enter(Some(&first));
        note_shape_request();
        note_shape_request();
    }
    assert_eq!(first.snapshot().shape_requests, 2, "the scope ended; its notes are in");
    {
        let _frame = CollectGuard::enter(Some(&second));
        note_shape_request();
        {
            // A nested scope of the other renderer keeps its own notes too.
            let _nested = CollectGuard::enter(Some(&first));
            note_row_cache(true);
        }
        note_row_cache(false);
    }
    let (first, second) = (first.snapshot(), second.snapshot());
    assert_eq!((first.shape_requests, first.row_cache_hits, first.row_cache_misses), (2, 1, 0));
    assert_eq!((second.shape_requests, second.row_cache_hits, second.row_cache_misses), (1, 0, 1));
}

#[test]
pub(super) fn a_closed_renderers_notes_never_reach_the_next_renderer() {
    // A renderer that drew and closed leaves nothing pending for the one drawing after it.
    {
        let closing = FrameStatsSink::default();
        let _layout = CollectGuard::enter(Some(&closing));
        note_shape_request();
    }
    let next = FrameStatsSink::default();
    {
        let _frame = CollectGuard::enter(Some(&next));
        note_row_cache(true);
    }
    let stats = next.snapshot();
    assert_eq!((stats.shape_requests, stats.row_cache_hits), (0, 1));
}

#[test]
pub(super) fn each_shaping_request_counts_once_failures_included() {
    // A request that fails was still made, so it counts.
    let sink = FrameStatsSink::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        let width: Result<f32, &str> = shape_request(|| Ok(12.5));
        let failed: Result<f32, &str> = shape_request(|| Err("no face"));
        assert_eq!((width, failed), (Ok(12.5), Err("no face")));
    }
    assert_eq!(sink.snapshot().shape_requests, 2);
}

#[test]
pub(super) fn a_native_redraw_request_counts_on_its_renderer() {
    // Renderer-owned redraw requests bypass the App, so the renderer counts them itself.
    let sink = FrameStatsSink::default();
    sink.note_native_request();
    sink.note_native_request();
    assert_eq!(sink.snapshot().native_request_redraw, 2);
}

#[test]
pub(super) fn damage_is_computed_only_inside_a_counting_scope() {
    // With the gate off the frame's damage share is never computed.
    note_damage(|| panic!("damage computed with no counting scope"));
    {
        let _not_counting = CollectGuard::enter(None);
        note_damage(|| panic!("damage computed for a non-counting renderer"));
    }
}

#[test]
pub(super) fn damage_is_the_damaged_share_of_the_surface_in_permille() {
    // A share of an empty surface is 0, and damage never counts above the whole surface.
    let full = PixelRect { x: 0, y: 0, w: 800, h: 600 };
    let quarter = PixelRect { x: 0, y: 0, w: 400, h: 300 };
    assert_eq!(damage_permille(&full, 800, 600), 1_000);
    assert_eq!(damage_permille(&quarter, 800, 600), 250);
    assert_eq!(damage_permille(&full, 0, 600), 0);
    assert_eq!(damage_permille(&full, 400, 300), 1_000);
}

#[test]
pub(super) fn a_frame_counts_as_software_only_where_the_software_presenter_runs() {
    // Degraded software rendering switches presenters only on Windows; elsewhere a degraded
    // frame still goes through wgpu and counts as a GPU frame.
    assert!(!presents_software_on(false, true));
    assert!(!presents_software_on(false, false));
    assert!(presents_software_on(true, true));
    assert!(!presents_software_on(true, false));
    assert_eq!(presents_software(true), cfg!(target_os = "windows"));
    assert!(!presents_software(false));
    // The presenter and the frame count read the same predicate.
    assert!(presenter_predicate_is_shared(include_str!("present.rs"), crate::core::CORE_SOURCE));
}

/// Every non-test Rust source under `dir`, recursively, as `(path, text)`.
pub(super) fn crate_sources(dir: &Path, found: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("crate sources") {
        let entry_path = entry.expect("dir entry").path();
        if entry_path.is_dir() {
            crate_sources(&entry_path, found);
            continue;
        }
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            found.push((
                entry_path.display().to_string(),
                to_lf(&std::fs::read_to_string(&entry_path).unwrap()),
            ));
        }
    }
}

#[test]
pub(super) fn every_font_stack_shaping_call_goes_through_shape_request() {
    // A call outside the wrapper goes uncounted; the wrapper counts each one exactly once.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let (wrapped, bare) = shaping_calls(&sources);
    assert!(bare.is_empty(), "uncounted FontStack shaping calls: {bare:#?}");
    // The search badge's icon and label measures are served from the chrome-run cache, which
    // shapes through `ChromeShapedRun::shape`, so they are no longer separate sites.
    assert_eq!(wrapped, 5, "the shaping sites changed; review the count");
}

/// `text` with comments, strings, raw strings and character literals blanked to spaces, so
/// braces and call names inside them never count. Lifetimes are kept.
pub(super) fn code_only(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = chars.clone();
    let mut index = 0;
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for slot in &mut out[from..to.min(chars.len())] {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    };
    while index < chars.len() {
        let rest = |offset: usize| chars.get(index + offset).copied();
        if chars[index] == '/' && rest(1) == Some('/') {
            let end = (index..chars.len()).find(|at| chars[*at] == '\n').unwrap_or(chars.len());
            blank(&mut out, index, end);
            index = end;
        } else if chars[index] == '/' && rest(1) == Some('*') {
            let end = (index + 2..chars.len())
                .find(|at| chars[*at] == '*' && chars.get(at + 1) == Some(&'/'))
                .map_or(chars.len(), |at| at + 2);
            blank(&mut out, index, end);
            index = end;
        } else if chars[index] == 'r' && (rest(1) == Some('"') || rest(1) == Some('#')) {
            let hashes = (index + 1..chars.len()).take_while(|at| chars[*at] == '#').count();
            if chars.get(index + 1 + hashes) != Some(&'"') {
                index += 1;
                continue;
            }
            let closing: Vec<char> =
                std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
            let body = index + 2 + hashes;
            let end = (body..chars.len())
                .find(|at| chars[*at..].starts_with(&closing))
                .map_or(chars.len(), |at| at + closing.len());
            blank(&mut out, index, end);
            index = end;
        } else if chars[index] == '"' {
            let mut at = index + 1;
            while at < chars.len() && chars[at] != '"' {
                at += if chars[at] == '\\' { 2 } else { 1 };
            }
            blank(&mut out, index, at + 1);
            index = at + 1;
        } else if chars[index] == '\'' && rest(1) == Some('\\') {
            let end = (index + 2..chars.len())
                .find(|at| chars[*at] == '\'')
                .map_or(chars.len(), |at| at + 1);
            blank(&mut out, index, end);
            index = end;
        } else if chars[index] == '\'' && rest(2) == Some('\'') {
            blank(&mut out, index, index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }
    out.into_iter().collect()
}

/// The `{…}` block opening at or after `from` in `code`, as a byte range including braces.
pub(super) fn block_at(code: &str, from: usize) -> Option<std::ops::Range<usize>> {
    let open = from + code[from..].find('{')?;
    let mut depth = 0_usize;
    for (offset, character) in code[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open..open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// One function: its name, whether it is a `pub fn` of `impl GpuRenderer`, and its body.
pub(super) struct FunctionBody {
    pub(super) name: String,
    renderer_entry_point: bool,
    pub(super) body: String,
}

/// Every function with a body in `sources`.
pub(super) fn function_bodies(sources: &[(String, String)]) -> Vec<FunctionBody> {
    let mut found = Vec::new();
    for (_, text) in sources {
        let code = code_only(&to_lf(text));
        let renderer_blocks: Vec<_> = code
            .match_indices("impl GpuRenderer {")
            .filter_map(|(offset, _)| block_at(&code, offset))
            .collect();
        for (offset, _) in code.match_indices("fn ") {
            if offset > 0 && is_ident(code.as_bytes()[offset - 1]) {
                continue;
            }
            let name: String = code[offset + 3..]
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            let signature_end =
                code[offset..].find(['{', ';']).map_or(code.len(), |end| offset + end);
            if name.is_empty() || code.as_bytes().get(signature_end) != Some(&b'{') {
                continue;
            }
            let Some(range) = block_at(&code, offset) else {
                continue;
            };
            let public = code[..offset].trim_end().ends_with("pub");
            let in_renderer = renderer_blocks.iter().any(|block| block.contains(&offset));
            found.push(FunctionBody {
                name,
                renderer_entry_point: public && in_renderer,
                body: code[range].to_owned(),
            });
        }
    }
    found
}

pub(super) fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `body` calls a function or method named `name`.
pub(super) fn calls(body: &str, name: &str) -> bool {
    body.match_indices(name).any(|(offset, _)| {
        let before = offset == 0 || !is_ident(body.as_bytes()[offset - 1]);
        let after = body[offset + name.len()..].trim_start();
        before && (after.starts_with('(') || after.starts_with("::<"))
    })
}

/// The renderer's public entry points that can reach a counted shaping call without opening
/// a collection scope. Reachability follows calls by name, over-approximating: a function that
/// opens a scope covers everything it calls, so the walk stops there.
pub(super) fn unscoped_entry_points(sources: &[(String, String)]) -> Vec<String> {
    let bodies = function_bodies(sources);
    // A render entry opens its collection scope through RenderScope, which enters a CollectGuard.
    let opens_scope = |body: &FunctionBody| {
        body.body.contains("CollectGuard::enter(") || body.body.contains("RenderScope::enter(")
    };
    let mut reaching: BTreeSet<String> = bodies
        .iter()
        .filter(|body| body.name != "shape_request" && calls(&body.body, "shape_request"))
        .map(|body| body.name.clone())
        .collect();
    loop {
        let grown: Vec<String> = bodies
            .iter()
            .filter(|body| !opens_scope(body) && !reaching.contains(&body.name))
            .filter(|body| reaching.iter().any(|name| calls(&body.body, name)))
            .map(|body| body.name.clone())
            .collect();
        if grown.is_empty() {
            break;
        }
        reaching.extend(grown);
    }
    let mut unscoped: Vec<String> = bodies
        .iter()
        .filter(|body| body.renderer_entry_point && !opens_scope(body))
        .filter(|body| reaching.contains(&body.name) || calls(&body.body, "shape_request"))
        .map(|body| body.name.clone())
        .collect();
    unscoped.sort();
    unscoped.dedup();
    unscoped
}

#[test]
pub(super) fn every_renderer_entry_point_that_can_shape_opens_a_collection_scope() {
    // A shaping call reached from an entry point with no scope open records into no renderer.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    assert_eq!(unscoped_entry_points(&sources), Vec::<String>::new());
    let scoped = function_bodies(&sources)
        .into_iter()
        .filter(|body| {
            body.renderer_entry_point
                && (body.body.contains("CollectGuard::enter(")
                    || body.body.contains("RenderScope::enter("))
        })
        .map(|body| body.name)
        .collect::<BTreeSet<_>>();
    for entry_point in [
        "render_releasing",
        "measure_overlay_text_width",
        "notification_layout",
        "measure_tab_widths",
    ] {
        assert!(scoped.contains(entry_point), "{entry_point} opens no scope: {scoped:?}");
    }
    // The compatibility wrapper opens no scope of its own: it reaches the one `render_releasing` opens.
    let core = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let wrapper = core.split_once("    pub fn render_with_outcome(").expect("wrapper").1;
    let wrapper = wrapper.split_once("\n    }\n").expect("wrapper body").0;
    assert!(
        !wrapper.contains("CollectGuard::enter(") && !wrapper.contains("RenderScope::enter("),
        "the wrapper opens its own scope"
    );
    assert!(wrapper.contains("self.render_releasing("), "the wrapper bypasses render_releasing");
}

#[test]
pub(super) fn the_entry_point_audit_reports_a_scope_missing_two_calls_away() {
    // Negative fixture: an entry point reaching shaping through a helper, with braces inside
    // a raw string and a char literal, is reported until it opens a scope.
    let fixture = r##"
const SHADER: &str = r#"fn fake() { shape_request(|| 0) }"#;
fn helper(stack: &Stack) -> f32 { let brace = '{'; crate::frame_stats::shape_request(|| stack.width()) }
fn middle(stack: &Stack) -> f32 { helper(stack) }
impl GpuRenderer {
    pub fn widths(&self) -> f32 { middle(&self.stack) }
    pub fn scoped(&self) -> f32 { let _collect = CollectGuard::enter(self.frame_sink.as_ref()); middle(&self.stack) }
    pub fn unrelated(&self) -> f32 { 1.0 }
}
"##;
    let sources = vec![("fixture.rs".to_owned(), fixture.to_owned())];
    assert_eq!(unscoped_entry_points(&sources), vec!["widths".to_owned()]);
}

#[test]
pub(super) fn every_renderer_redraw_request_goes_through_the_counting_helper() {
    // A renderer-owned request outside request_window_redraw would go uncounted.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let found = renderer_redraw_requests(&sources);
    assert_eq!(found.len(), 1, "{found:#?}");
    let core = code_only(&to_lf(crate::core::CORE_SOURCE));
    let helper = core.find("fn request_window_redraw(").expect("counting helper");
    let body = &core[block_at(&core, helper).expect("helper body")];
    assert!(body.contains(".request_redraw()") && body.contains("note_native_request()"), "{body}");
}

#[test]
pub(super) fn each_work_counter_moves_as_defined_inside_a_counting_scope() {
    // full_frames, recolor visits and assembly each record once per note, into the scope's
    // renderer; the glyph cache's invalidation counters have no note and stay 0. Buckets are asserted only for injected durations; a
    // real-clock sample could land in any bucket on a preempted runner.
    let sink = FrameStatsSink::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        note_full_frame(true);
        note_full_frame(false);
        note_recolor_glyphs_visited(|| 120);
        record_assembly_us(70);
        record_assembly_us(6_000);
    }
    let stats = sink.snapshot();
    assert_eq!(
        (stats.full_frames, stats.row_cache_invalidate_visits, stats.recolor_glyphs_visited),
        (1, 0, 120)
    );
    assert_eq!(stats.row_cache_invalidate_us, 0);
    assert_eq!(stats.assembly_buckets, [0, 0, 1, 0, 0, 0, 1]);
    assert_eq!(stats.assembly_sum_us, 6_070);
    // Smoke: one real-clock assembly adds exactly one sample in some bucket, and the sum never falls.
    {
        let _counting = CollectGuard::enter(Some(&sink));
        assert!(assembly_clock().is_some());
        note_assembly(assembly_clock());
        finish_assembly();
    }
    let after = sink.snapshot();
    assert_eq!(after.assembly_buckets.iter().sum::<u64>(), 3, "one sample per assembled frame");
    assert!(after.assembly_sum_us >= stats.assembly_sum_us);
}

#[test]
pub(super) fn with_the_gate_off_no_work_counter_moves_or_reads_a_clock() {
    // Outside a counting scope each counter is one check: no closure runs and no clock is read.
    let sink = FrameStatsSink::default();
    for counting in [false, true] {
        let _scope = counting.then(|| CollectGuard::enter(None));
        note_full_frame(true);
        note_recolor_glyphs_visited(|| panic!("glyphs read with the gate off"));
        assert!(assembly_clock().is_none());
        note_assembly(None);
    }
    assert_eq!(sink.snapshot(), FrameStats::ZERO);
}

#[test]
pub(super) fn assembly_buckets_use_the_microsecond_bounds_with_a_value_at_a_bound_in_its_bucket() {
    // The App exports these as a "us" histogram, so they must bucket exactly as its own do.
    assert_eq!(ASSEMBLY_BOUNDS_US, [10, 50, 100, 500, 1_000, 5_000]);
    let sink = FrameStatsSink::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        for elapsed_us in [10, 11, 5_000, 5_001] {
            record_assembly_us(elapsed_us);
        }
    }
    assert_eq!(sink.snapshot().assembly_buckets, [1, 1, 0, 0, 0, 1, 1]);
}

/// core.rs with all whitespace removed, so rustfmt's wrapping never hides a call.
pub(super) fn core_code() -> String {
    to_lf(crate::core::CORE_SOURCE).split_whitespace().collect()
}

#[test]
pub(super) fn assembly_runs_from_the_frame_key_lap_to_the_overlays_lap_after_the_noop_return() {
    // A Noop frame returns before the clock starts; an assembled frame takes one sample at the
    // overlays lap, before the atlas-retry check, upload, acquire, submit and present.
    let core = core_code();
    let noop = core.find("ifplan.mode==RenderMode::Noop{").expect("noop return");
    let key_lap = core.find("gpu_lap!(\"frame_key\");").expect("frame_key lap");
    let start =
        core.find("letassembly_started=crate::frame_stats::assembly_clock();").expect("start");
    let overlays = core.find("gpu_lap!(\"overlays\");").expect("overlays lap");
    let sample = core.find("crate::frame_stats::note_assembly(assembly_started);").expect("sample");
    let retry = overlays
        + core[overlays..]
            .find("Self::finish_assembly_pass(&mutplan,panes,pass_end)")
            .expect("retry check");
    assert!(
        noop < key_lap
            && key_lap < start
            && start < overlays
            && overlays < sample
            && sample < retry
    );
    assert_eq!(core.matches("frame_stats::assembly_clock()").count(), 1);
    assert_eq!(core.matches("frame_stats::note_assembly(").count(), 1);
}

#[test]
pub(super) fn only_the_quad_cache_drops_dirty_rows_inside_each_panes_loop() {
    // The glyph cache is keyed by content, so changed content misses by itself and nothing drops
    // a glyph row for dirt: no glyph-cache row removal, no invalidation clock and no counter note
    // remain. The quad cache still drops each dirty live row's absolute entry, once per drawn
    // pane, inside the background loop, uncounted.
    let core = core_code();
    assert_eq!(core.matches("invalidate_row_abs(").count(), 1, "only the quad wrapper drops rows");
    assert_eq!(
        core.matches("cache.invalidate_row_abs(planned.id,planned.scrollback_len+rowasu64);")
            .count(),
        1,
        "the quad cache drops the absolute row the live dirty row is stored at"
    );
    for gone in [
        "invalidate_planned_glyph_rows(",
        "fninvalidate_dirty_rows(",
        "invalidation_clock(",
        "note_row_cache_invalidate_visits(",
        "note_row_cache_invalidate_us(",
    ] {
        assert_eq!(core.matches(gone).count(), 0, "{gone} is gone");
    }
    let quad_call = "invalidate_planned_quad_rows(&mutself.line_quad_cache,pv.planned);";
    assert_eq!(core.matches(quad_call).count(), 1);
    let pane_loop = core
        .find("for(pane_index,pv)inpane_views.iter().enumerate().filter(|(_,pane)|pane.planned.full_clip.is_some()){")
        .expect("per-pane loop");
    assert!(pane_loop < core.find(quad_call).expect("quad call"), "inside a pane loop");
}

#[test]
pub(super) fn recolor_visits_count_the_main_glyph_list_and_never_an_overlay() {
    // The three recolors of glyph_instances go through the row-pruned scan and record the
    // glyphs it examined; the two field-mark recolors keep their overlay slices and are not counted.
    let core = core_code();
    let mut main = 0;
    for (offset, _) in core.match_indices("recolor_cursor_glyphs_in(") {
        let call = &core[offset..core.len().min(offset + 80)];
        let after = &core[offset..core.len().min(offset + 260)];
        assert!(
            call.contains("(&mut*glyph_instances,row_spans,"),
            "unexpected main recolor: {call}"
        );
        assert!(
            after.contains(");crate::frame_stats::note_recolor_glyphs_visited(||visited);"),
            "uncounted main recolor: {after}"
        );
        main += 1;
    }
    // The search and palette fields recolor through `paint_field_marks`, always on an overlay slice,
    // and core draws no other overlay recolor directly.
    assert_eq!(
        core.matches("recolor_cursor_glyphs(").count(),
        0,
        "a direct overlay recolor in core"
    );
    let mut overlay = 0;
    for (offset, _) in core.match_indices("crate::cursor::paint_field_marks(") {
        let call = &core[offset..core.len().min(offset + 120)];
        let before = &core[offset.saturating_sub(90)..offset];
        assert!(
            call.contains("&mutoverlay_glyph_instances["),
            "a field mark recolors the main list: {call}"
        );
        assert!(!before.contains("note_recolor_glyphs_visited"), "an overlay recolor was counted");
        overlay += 1;
    }
    assert_eq!((main, overlay), (3, 2));
    assert_eq!(core.matches("note_recolor_glyphs_visited(||glyph_instances.len())").count(), 0);
}

/// `text` with CRLF line ends turned into LF, the form every scan reads.
pub(super) fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `text` as a Windows checkout holds it, with CRLF line ends.
pub(super) fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// Whether the presenter branch in `present` and the frame count in `core` both read
/// `presents_software(self.software_render_degrade)`.
pub(super) fn presenter_predicate_is_shared(present: &str, core: &str) -> bool {
    let (present, core) = (to_lf(present), to_lf(core));
    present.contains("if crate::frame_stats::presents_software(self.software_render_degrade)")
        && core.contains(
            "let software_presenter =\n            crate::frame_stats::presents_software(self.software_render_degrade);\n        crate::frame_stats::note_frame(software_presenter);",
        )
}

/// FontStack shaping calls in `sources` outside frame_stats.rs: how many go through
/// `shape_request`, and each one that does not, as `file:line method`.
pub(super) fn shaping_calls(sources: &[(String, String)]) -> (usize, Vec<String>) {
    let mut wrapped = 0;
    let mut bare = Vec::new();
    for (file, text) in sources {
        if file.ends_with("frame_stats.rs") {
            continue;
        }
        // The 80-byte look-back and line numbers read the LF form, whatever the checkout holds.
        let text = &to_lf(text);
        for method in [".shape_text_for_frame(", ".measure_text_width_for_frame("] {
            for (offset, _) in text.match_indices(method) {
                let mut start = offset.saturating_sub(80);
                while !text.is_char_boundary(start) {
                    start -= 1;
                }
                if text[start..offset].contains("shape_request(||") {
                    wrapped += 1;
                } else {
                    let line = text[..offset].matches('\n').count() + 1;
                    bare.push(format!("{file}:{line} {method}"));
                }
            }
        }
    }
    (wrapped, bare)
}

/// Each direct `.request_redraw()` call in `sources`, as `file:line`, outside comments and strings.
pub(super) fn renderer_redraw_requests(sources: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (file, text) in sources {
        let code = code_only(&to_lf(text));
        for (offset, _) in code.match_indices(".request_redraw()") {
            found.push(format!("{file}:{}", code[..offset].matches('\n').count() + 1));
        }
    }
    found
}

#[test]
pub(super) fn source_scans_read_a_crlf_checkout_as_they_read_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends. Each scan, fed a CRLF copy of its real
    // input, must reach the answer it reaches on the LF copy; every scan that differs is listed.
    let mut differs = Vec::new();
    let (present, core) = (include_str!("present.rs"), crate::core::CORE_SOURCE);
    assert!(presenter_predicate_is_shared(&to_lf(present), &to_lf(core)));
    if !presenter_predicate_is_shared(&to_crlf(present), &to_crlf(core)) {
        differs.push("presenter_predicate_is_shared");
    }
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let as_lf: Vec<_> = sources.iter().map(|(file, text)| (file.clone(), to_lf(text))).collect();
    let as_crlf: Vec<_> =
        sources.iter().map(|(file, text)| (file.clone(), to_crlf(text))).collect();
    if shaping_calls(&as_crlf) != shaping_calls(&as_lf) {
        differs.push("shaping_calls");
    }
    if unscoped_entry_points(&as_crlf) != unscoped_entry_points(&as_lf) {
        differs.push("unscoped_entry_points");
    }
    if renderer_redraw_requests(&as_crlf) != renderer_redraw_requests(&as_lf) {
        differs.push("renderer_redraw_requests");
    }
    assert!(differs.is_empty(), "{differs:#?}");
}

/// Each call to a shaping entry point that may wait for fallback discovery, as `file:line name`.
/// Sources are read as LF with comments, strings, raw strings and character literals blanked.
pub(super) fn blocking_shape_calls(sources: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (file, text) in sources {
        let code = code_only(&to_lf(text));
        for name in ["blocking_shape", "shape_text", "shape_text_with_style", "measure_text_width"]
        {
            for (offset, _) in code.match_indices(name) {
                let before = offset == 0 || !is_ident(code.as_bytes()[offset - 1]);
                let after = &code[offset + name.len()..];
                if before && after.trim_start().starts_with('(') {
                    let line = code[..offset].matches('\n').count() + 1;
                    found.push(format!("{file}:{line} {name}"));
                }
            }
        }
    }
    found
}

/// A rasterizer that returns its scripted results in order, then nothing.
pub(super) struct Scripted {
    results: std::collections::VecDeque<Option<RasterTile>>,
}

impl Scripted {
    pub(super) fn new(results: Vec<Option<RasterTile>>) -> Self {
        Self { results: results.into() }
    }
}

impl Rasterizer for Scripted {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        self.results.pop_front().flatten()
    }
}

/// A rasterizer that shapes glyph zero through a counted request, as a nested-timer probe.
pub(super) struct ShapingRasterizer;

impl Rasterizer for ShapingRasterizer {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        shape_request(|| ());
        Some(coverage_tile(1, 1))
    }
}

/// A monochrome tile of `width` by `height` pixels; zero in either is an empty tile.
pub(super) fn coverage_tile(width: u32, height: u32) -> RasterTile {
    RasterTile {
        width,
        height,
        offset_x: 0,
        offset_y: 0,
        advance: width as f32,
        coverage: vec![255; (width * height) as usize],
        is_color: false,
        is_subpixel: false,
    }
}

pub(super) fn glyph(character: char) -> GlyphKey {
    GlyphKey::new(character, false, false)
}

/// Every glyph-atlas insertion in `sources` as `path:line`, with whether its arguments wrap the
/// rasterizer in `CountingRasterizer`. A one-argument `get_or_insert` is `Option`'s, not an atlas
/// insertion (which takes a key and a rasterizer), so it is skipped.
pub(super) fn atlas_insertions(sources: &[(String, String)]) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for (path, text) in sources {
        let code = code_only(&to_lf(text));
        for (offset, _) in code.match_indices(".get_or_insert(") {
            let open = offset + ".get_or_insert".len();
            let mut depth = 0_usize;
            let mut close = code.len();
            for (index, character) in code[open..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + index;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let arguments = &code[open + 1..close];
            let mut nesting = 0_usize;
            let top_level_commas = arguments
                .chars()
                .filter(|character| {
                    match character {
                        '(' | '[' | '{' => nesting += 1,
                        ')' | ']' | '}' => nesting = nesting.saturating_sub(1),
                        _ => {}
                    }
                    *character == ',' && nesting == 0
                })
                .count();
            if top_level_commas == 0 {
                // A one-argument call is `Option::get_or_insert`, not an atlas insertion.
                continue;
            }
            let line = code[..offset].matches('\n').count() + 1;
            let name = Path::new(path)
                .file_name()
                .map_or(path.clone(), |name| name.to_string_lossy().into_owned());
            found.push((
                format!("{name}:{line}"),
                code[open..close].contains("CountingRasterizer::new("),
            ));
        }
    }
    found
}
