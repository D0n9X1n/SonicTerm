//! Pins the renderer's frame statistics: the counting gate, per-renderer collection scopes,
//! shaping counts, damage, the presenter path and the call sites that must open a scope.

use std::collections::BTreeSet;
use std::path::Path;

use sonicterm_render_model::geometry::PixelRect;

use super::*;

#[test]
fn notes_outside_a_counting_renderer_record_nothing() {
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
fn a_counting_renderer_collects_into_its_own_sink_and_restores_the_enclosing_gate() {
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
fn interleaved_renderers_on_one_thread_keep_their_own_statistics() {
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
fn a_closed_renderers_notes_never_reach_the_next_renderer() {
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
fn each_shaping_request_counts_once_failures_included() {
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
fn a_native_redraw_request_counts_on_its_renderer() {
    // Renderer-owned redraw requests bypass the App, so the renderer counts them itself.
    let sink = FrameStatsSink::default();
    sink.note_native_request();
    sink.note_native_request();
    assert_eq!(sink.snapshot().native_request_redraw, 2);
}

#[test]
fn damage_is_computed_only_inside_a_counting_scope() {
    // With the gate off the frame's damage share is never computed.
    note_damage(|| panic!("damage computed with no counting scope"));
    {
        let _not_counting = CollectGuard::enter(None);
        note_damage(|| panic!("damage computed for a non-counting renderer"));
    }
}

#[test]
fn damage_is_the_damaged_share_of_the_surface_in_permille() {
    // A share of an empty surface is 0, and damage never counts above the whole surface.
    let full = PixelRect { x: 0, y: 0, w: 800, h: 600 };
    let quarter = PixelRect { x: 0, y: 0, w: 400, h: 300 };
    assert_eq!(damage_permille(&full, 800, 600), 1_000);
    assert_eq!(damage_permille(&quarter, 800, 600), 250);
    assert_eq!(damage_permille(&full, 0, 600), 0);
    assert_eq!(damage_permille(&full, 400, 300), 1_000);
}

#[test]
fn a_frame_counts_as_software_only_where_the_software_presenter_runs() {
    // Degraded software rendering switches presenters only on Windows; elsewhere a degraded
    // frame still goes through wgpu and counts as a GPU frame.
    assert!(!presents_software_on(false, true));
    assert!(!presents_software_on(false, false));
    assert!(presents_software_on(true, true));
    assert!(!presents_software_on(true, false));
    assert_eq!(presents_software(true), cfg!(target_os = "windows"));
    assert!(!presents_software(false));
    // The presenter and the frame count read the same predicate.
    assert!(presenter_predicate_is_shared(include_str!("present.rs"), include_str!("core.rs")));
}

/// Every non-test Rust source under `dir`, recursively, as `(path, text)`.
fn crate_sources(dir: &Path, found: &mut Vec<(String, String)>) {
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
fn every_font_stack_shaping_call_goes_through_shape_request() {
    // A call outside the wrapper goes uncounted; the wrapper counts each one exactly once.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let (wrapped, bare) = shaping_calls(&sources);
    assert!(bare.is_empty(), "uncounted FontStack shaping calls: {bare:#?}");
    assert_eq!(wrapped, 8, "the shaping sites changed; review the count");
}

/// `text` with comments, strings, raw strings and character literals blanked to spaces, so
/// braces and call names inside them never count. Lifetimes are kept.
fn code_only(text: &str) -> String {
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
fn block_at(code: &str, from: usize) -> Option<std::ops::Range<usize>> {
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
struct FunctionBody {
    name: String,
    renderer_entry_point: bool,
    body: String,
}

/// Every function with a body in `sources`.
fn function_bodies(sources: &[(String, String)]) -> Vec<FunctionBody> {
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

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `body` calls a function or method named `name`.
fn calls(body: &str, name: &str) -> bool {
    body.match_indices(name).any(|(offset, _)| {
        let before = offset == 0 || !is_ident(body.as_bytes()[offset - 1]);
        let after = body[offset + name.len()..].trim_start();
        before && (after.starts_with('(') || after.starts_with("::<"))
    })
}

/// The renderer's public entry points that can reach a counted shaping call without opening
/// a collection scope. Reachability follows calls by name, over-approximating: a function that
/// opens a scope covers everything it calls, so the walk stops there.
fn unscoped_entry_points(sources: &[(String, String)]) -> Vec<String> {
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
fn every_renderer_entry_point_that_can_shape_opens_a_collection_scope() {
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
    let core = include_str!("core.rs").replace("\r\n", "\n");
    let wrapper = core.split_once("    pub fn render_with_outcome(").expect("wrapper").1;
    let wrapper = wrapper.split_once("\n    }\n").expect("wrapper body").0;
    assert!(
        !wrapper.contains("CollectGuard::enter(") && !wrapper.contains("RenderScope::enter("),
        "the wrapper opens its own scope"
    );
    assert!(wrapper.contains("self.render_releasing("), "the wrapper bypasses render_releasing");
}

#[test]
fn the_entry_point_audit_reports_a_scope_missing_two_calls_away() {
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
fn every_renderer_redraw_request_goes_through_the_counting_helper() {
    // A renderer-owned request outside request_window_redraw would go uncounted.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let found = renderer_redraw_requests(&sources);
    assert_eq!(found.len(), 1, "{found:#?}");
    let core = code_only(&to_lf(include_str!("core.rs")));
    let helper = core.find("fn request_window_redraw(").expect("counting helper");
    let body = &core[block_at(&core, helper).expect("helper body")];
    assert!(body.contains(".request_redraw()") && body.contains("note_native_request()"), "{body}");
}

#[test]
fn each_work_counter_moves_as_defined_inside_a_counting_scope() {
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
fn with_the_gate_off_no_work_counter_moves_or_reads_a_clock() {
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
fn assembly_buckets_use_the_microsecond_bounds_with_a_value_at_a_bound_in_its_bucket() {
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
fn core_code() -> String {
    to_lf(include_str!("core.rs")).split_whitespace().collect()
}

#[test]
fn assembly_runs_from_the_frame_key_lap_to_the_overlays_lap_after_the_noop_return() {
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
fn only_the_quad_cache_drops_dirty_rows_inside_each_panes_loop() {
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
        .find("forpvinpane_views.iter().filter(|pane|pane.planned.full_clip.is_some()){")
        .expect("per-pane loop");
    assert!(pane_loop < core.find(quad_call).expect("quad call"), "inside a pane loop");
}

#[test]
fn recolor_visits_count_the_main_glyph_list_and_never_an_overlay() {
    // The three recolors of glyph_instances go through the row-pruned scan and record the
    // glyphs it examined; the two field-mark recolors keep their overlay slices and are not counted.
    let core = core_code();
    let mut main = 0;
    for (offset, _) in core.match_indices("recolor_cursor_glyphs_in(") {
        let call = &core[offset..core.len().min(offset + 80)];
        let after = &core[offset..core.len().min(offset + 260)];
        assert!(
            call.contains("(&mutglyph_instances,&row_spans,"),
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
fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `text` as a Windows checkout holds it, with CRLF line ends.
fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// Whether the presenter branch in `present` and the frame count in `core` both read
/// `presents_software(self.software_render_degrade)`.
fn presenter_predicate_is_shared(present: &str, core: &str) -> bool {
    let (present, core) = (to_lf(present), to_lf(core));
    present.contains("if crate::frame_stats::presents_software(self.software_render_degrade)")
        && core.contains(
            "let software_presenter =\n            crate::frame_stats::presents_software(self.software_render_degrade);\n        crate::frame_stats::note_frame(software_presenter);",
        )
}

/// FontStack shaping calls in `sources` outside frame_stats.rs: how many go through
/// `shape_request`, and each one that does not, as `file:line method`.
fn shaping_calls(sources: &[(String, String)]) -> (usize, Vec<String>) {
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
fn renderer_redraw_requests(sources: &[(String, String)]) -> Vec<String> {
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
fn source_scans_read_a_crlf_checkout_as_they_read_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends. Each scan, fed a CRLF copy of its real
    // input, must reach the answer it reaches on the LF copy; every scan that differs is listed.
    let mut differs = Vec::new();
    let (present, core) = (include_str!("present.rs"), include_str!("core.rs"));
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
fn blocking_shape_calls(sources: &[(String, String)]) -> Vec<String> {
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

#[test]
fn no_frame_code_calls_a_shaping_entry_point_that_may_wait() {
    // The renderer shapes and measures only through the frame entry points, which never wait for
    // fallback discovery; the blocking ones stay for explicit callers and tests.
    // The working-set helper is the one explicit caller: it measures outside any frame and must
    // wait for fallback faces. It must exist, so a rename cannot silently widen the exemption.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let is_measurement = |file: &str| Path::new(file).ends_with("glyph_working_set.rs");
    assert!(sources.iter().any(|(file, _)| is_measurement(file)), "the helper's file exists");
    sources.retain(|(file, _)| !is_measurement(file));
    assert_eq!(blocking_shape_calls(&sources), Vec::<String>::new());
}

#[test]
fn the_blocking_call_scan_ignores_comments_strings_and_line_endings() {
    // Mentions in line and block comments, raw strings and escaped char literals never count, the
    // frame entry points never count, and a CRLF checkout finds exactly what an LF one finds.
    let fixture = r##"
// stack.shape_text("x")
/* font.blocking_shape(a, b) */
const RAW: &str = r#"stack.measure_text_width("y")"#;
const ESCAPED: char = '\'';
fn frame(stack: &FontStack) {
    let quote = "shape_text_with_style(";
    stack.shape_text_for_frame("a", false, false);
    stack.measure_text_width_for_frame("b");
    stack.shape_text_with_style("c", true, false);
}
"##;
    let lf = vec![("fixture.rs".to_owned(), fixture.to_owned())];
    let crlf = vec![("fixture.rs".to_owned(), fixture.replace('\n', "\r\n"))];
    assert_eq!(blocking_shape_calls(&lf), vec!["fixture.rs:10 shape_text_with_style".to_owned()]);
    assert_eq!(blocking_shape_calls(&crlf), blocking_shape_calls(&lf));
}

/// Growth counts and growth-to-present times are recorded only inside a counting scope; a time at a
/// millisecond bound lands in that bucket, a time past the last bound in the overflow, and the sum
/// is exact in microseconds. An abandoned episode reaches the sink only through a finalization
/// note, which lands outside any scope.
#[test]
fn growth_counters_and_growth_to_present_buckets() {
    let sink = FrameStatsSink::default();
    note_glyph_atlas_growths(5);
    record_growth_to_present_us(1_000);
    assert_eq!(sink.snapshot(), FrameStats::ZERO, "no scope, nothing recorded");
    {
        let _counting = CollectGuard::enter(Some(&sink));
        note_glyph_atlas_growths(2);
        for elapsed_us in [4_000, 4_001, 100_000, 100_001] {
            record_growth_to_present_us(elapsed_us);
        }
    }
    sink.note_teardown_growths(1, 1);
    let stats = sink.snapshot();
    assert_eq!((stats.glyph_atlas_growths, stats.atlas_growth_abandoned), (3, 1));
    let mut expected = [0; GROWTH_TO_PRESENT_BUCKETS];
    expected[0] = 1; // 4 ms is at the first bound
    expected[1] = 1; // just past 4 ms
    expected[8] = 1; // 100 ms is at the last bound
    expected[9] = 1; // overflow
    assert_eq!(stats.atlas_growth_to_present_buckets, expected);
    assert_eq!(stats.atlas_growth_to_present_sum_us, 4_000 + 4_001 + 100_000 + 100_001);
}

/// Finalizing growth episodes is idempotent: the first call adds the uncounted growths and one
/// abandoned episode to the sink outside any scope, and a second call, as `Drop` makes after the
/// App's own finalization, adds nothing.
#[test]
fn finalizing_growth_episodes_twice_counts_them_once() {
    let sink = FrameStatsSink::default();
    let mut episodes = GrowthEpisodes::default();
    {
        let _counting = CollectGuard::enter(Some(&sink));
        episodes.count(1, Instant::now());
    }
    // One growth counted at a frame check, then one more outside any frame before teardown.
    episodes.finalize(2, Some(&sink));
    let first = sink.snapshot();
    assert_eq!((first.glyph_atlas_growths, first.atlas_growth_abandoned), (2, 1));
    episodes.finalize(2, Some(&sink));
    assert_eq!(sink.snapshot(), first, "a second finalization adds nothing");
}

/// A rasterizer that returns its scripted results in order, then nothing.
struct Scripted {
    results: std::collections::VecDeque<Option<RasterTile>>,
}

impl Scripted {
    fn new(results: Vec<Option<RasterTile>>) -> Self {
        Self { results: results.into() }
    }
}

impl Rasterizer for Scripted {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        self.results.pop_front().flatten()
    }
}

/// A rasterizer that shapes glyph zero through a counted request, as a nested-timer probe.
struct ShapingRasterizer;

impl Rasterizer for ShapingRasterizer {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        shape_request(|| ());
        Some(coverage_tile(1, 1))
    }
}

/// A monochrome tile of `width` by `height` pixels; zero in either is an empty tile.
fn coverage_tile(width: u32, height: u32) -> RasterTile {
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

fn glyph(character: char) -> GlyphKey {
    GlyphKey::new(character, false, false)
}

#[test]
fn a_timed_request_adds_exactly_its_clock_pair_and_the_gate_off_reads_no_clock() {
    // The test clock steps 7 ns per reading, so one request reads it twice and adds exactly 7 ns.
    // With no counting scope each closure still runs once and returns its value, and no clock,
    // counter, attempt or preparation timer reads the clock.
    test_clock::install(100, 7);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        assert_eq!(shape_request(|| 42), 42);
    }
    assert_eq!(test_clock::reads(), 2);
    assert_eq!((sink.snapshot().shape_ns, sink.snapshot().shape_requests), (7, 1));

    let mut runs = 0;
    assert_eq!(
        shape_request(|| {
            runs += 1;
            5
        }),
        5
    );
    let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
    assert!(CountingRasterizer::new(&mut scripted).rasterize(glyph('a')).is_some());
    let _attempt = AttemptScope::enter(true);
    assert_eq!(prepare_clock(), None);
    assert_eq!(runs, 1);
    assert_eq!(test_clock::reads(), 2, "the gate-off calls read no clock");
    assert_eq!(sink.snapshot().shape_requests, 1, "and write no counter");
    test_clock::remove();
}

#[test]
fn nested_shaping_and_rasterizing_count_once_under_the_outer_timer() {
    // Glyph-zero shaping inside a rasterizer call is raster time, and a rasterizer reached from a
    // shaping request is shape time: only the outermost timer reads the clock.
    test_clock::install(0, 10);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        CountingRasterizer::new(&mut ShapingRasterizer).rasterize(glyph('a'));
        shape_request(|| {
            let mut scripted = Scripted::new(vec![Some(coverage_tile(1, 1))]);
            CountingRasterizer::new(&mut scripted).rasterize(glyph('b'))
        });
    }
    let stats = sink.snapshot();
    assert_eq!((stats.raster_ns, stats.shape_ns), (10, 10));
    assert_eq!((stats.raster_calls, stats.shape_requests), (2, 2));
    assert_eq!(test_clock::reads(), 4, "two outer timers, two readings each");
    test_clock::remove();
}

#[test]
fn a_rasterizer_call_counts_once_and_a_tile_only_when_it_has_pixels() {
    // No tile and an empty tile are calls but not tiles; an atlas hit calls nothing.
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let mut scripted =
            Scripted::new(vec![None, Some(coverage_tile(0, 0)), Some(coverage_tile(2, 2))]);
        let mut counting = CountingRasterizer::new(&mut scripted);
        for character in ['a', 'b', 'c'] {
            counting.rasterize(glyph(character));
        }
        let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(64, 64);
        let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
        for _ in 0..2 {
            atlas.get_or_insert(glyph('d'), &mut CountingRasterizer::new(&mut scripted));
        }
    }
    let stats = sink.snapshot();
    assert_eq!((stats.raster_calls, stats.raster_tiles), (4, 2));
}

#[test]
fn an_attempt_folds_its_own_work_once_and_an_apply_attempt_into_both_classes() {
    // Clock step 10. Shaping before any attempt adds to the phase only; the apply attempt (enter,
    // shape pair, raster pair, close) and the retry after it (enter, shape pair, close) each fold
    // once, and only the apply attempt joins the apply class.
    test_clock::install(0, 10);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        shape_request(|| ());
        {
            let _attempt = AttemptScope::enter(true);
            shape_request(|| ());
            let mut scripted = Scripted::new(vec![Some(coverage_tile(2, 2))]);
            CountingRasterizer::new(&mut scripted).rasterize(glyph('a'));
            note_attempt_presented();
            note_attempt_presented();
        }
        {
            let _retry = AttemptScope::enter(false);
            shape_request(|| ());
        }
    }
    let stats = sink.snapshot();
    assert_eq!((stats.shape_ns, stats.raster_ns, stats.shape_requests), (30, 10, 3));
    let apply = AttemptStats {
        attempts: 1,
        presented: 1,
        attempt_ns: 50,
        shape_ns: 10,
        raster_ns: 10,
        shape_requests: 1,
        raster_calls: 1,
        raster_tiles: 1,
    };
    assert_eq!(stats.apply_attempts, apply);
    let retry = AttemptStats {
        attempts: 1,
        attempt_ns: 30,
        shape_ns: 10,
        shape_requests: 1,
        ..AttemptStats::ZERO
    };
    let mut every = apply;
    every.add(&retry);
    assert_eq!(stats.attempts, every);
    test_clock::remove();
}

#[test]
fn a_helper_of_the_same_renderer_joins_its_attempt_and_any_other_renderer_is_isolated() {
    // notification_layout opens its own scope on the drawing renderer's sink, so its shaping joins
    // that renderer's attempt once. Another renderer, or a non-counting one, starts outside every
    // attempt and with no open timer, and the outer attempt resumes when it closes.
    test_clock::install(0, 1);
    let sink = FrameStatsSink::default();
    let other = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let _attempt = AttemptScope::enter(false);
        {
            let _helper = CollectGuard::enter(Some(&sink));
            shape_request(|| ());
        }
        {
            let _quiet = CollectGuard::enter(None);
            shape_request(|| ());
        }
        {
            let _outer_timer = WorkTimer::start(TimedWork::Raster);
            let _other = CollectGuard::enter(Some(&other));
            assert_eq!(ATTEMPT.with(Cell::get), None, "another renderer is outside any attempt");
            assert_eq!(TIMING_DEPTH.with(Cell::get), 0, "and its timers start unnested");
            shape_request(|| ());
        }
        assert!(ATTEMPT.with(Cell::get).is_some(), "the outer attempt resumes");
    }
    let stats = sink.snapshot();
    assert_eq!((stats.shape_requests, stats.attempts.shape_requests), (1, 1));
    assert_eq!((other.snapshot().shape_requests, other.snapshot().shape_ns), (1, 1));
    assert_eq!(other.snapshot().attempts, AttemptStats::ZERO);
    test_clock::remove();
}

#[test]
fn a_panic_inside_a_request_or_another_renderer_restores_the_timers_and_the_attempt() {
    // A failing request is still counted and timed once, and its timer is closed on unwind; a
    // renderer that panics inside another's attempt restores that attempt and its timer depth.
    test_clock::install(0, 5);
    let sink = FrameStatsSink::default();
    let other = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        let _attempt = AttemptScope::enter(false);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shape_request(|| -> u8 { panic!("shaping failed") })
        }));
        assert!(caught.is_err());
        assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _other = CollectGuard::enter(Some(&other));
            let _timer = WorkTimer::start(TimedWork::Raster);
            panic!("another renderer failed");
        }));
        assert!(caught.is_err());
        assert!(ATTEMPT.with(Cell::get).is_some());
        assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
        shape_request(|| ());
    }
    let stats = sink.snapshot();
    assert_eq!((stats.attempts.shape_requests, stats.shape_ns), (2, 10));
    test_clock::remove();
}

#[test]
fn font_preparation_time_is_exact_split_by_generation_and_outside_every_attempt() {
    // A preparation reads the clock twice; a generation apply adds to both preparation fields.
    test_clock::install(0, 3);
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_font_prepare(prepare_clock(), true);
        note_font_prepare(prepare_clock(), false);
    }
    let stats = sink.snapshot();
    assert_eq!((stats.font_prepare_ns, stats.font_generation_prepare_ns), (6, 3));
    assert_eq!(stats.attempts, AttemptStats::ZERO);
    note_font_prepare(prepare_clock(), true);
    assert_eq!(test_clock::reads(), 4, "the gate-off preparation reads no clock");
    test_clock::remove();
}

#[test]
fn an_attempt_nested_in_another_renderers_attempt_folds_into_its_own_renderer_even_on_unwind() {
    // A counting renderer that draws inside another's attempt keeps its own attempt and apply
    // class, and the outer attempt resumes with its own work; an inner attempt that panics still
    // folds once, unpresented, and restores the outer attempt and timer depth.
    let outer = FrameStatsSink::default();
    let inner = FrameStatsSink::default();
    {
        let mut outer_owed = false;
        let _outer = RenderScope::enter(Some(&outer), &mut outer_owed);
        shape_request(|| ());
        // An outer raster timer stays open across both inner attempts, so the depth they must
        // restore is 1, not 0: restoring 0 would let the outer renderer time nested work twice.
        let _outer_timer = WorkTimer::start(TimedWork::Raster);
        assert_eq!(TIMING_DEPTH.with(Cell::get), 1);
        {
            let mut inner_owed = true;
            let _inner = RenderScope::enter(Some(&inner), &mut inner_owed);
            shape_request(|| ());
            note_attempt_presented();
        }
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut inner_owed = true;
            let _inner = RenderScope::enter(Some(&inner), &mut inner_owed);
            shape_request(|| ());
            panic!("the inner renderer failed mid-attempt");
        }));
        assert!(caught.is_err());
        assert!(ATTEMPT.with(Cell::get).is_some(), "the outer attempt resumes");
        assert_eq!(TIMING_DEPTH.with(Cell::get), 1, "the outer timer is still open");
        // Nested inside the open outer timer, this request is counted but not timed again.
        shape_request(|| ());
    }
    assert_eq!(TIMING_DEPTH.with(Cell::get), 0);
    let inner = inner.snapshot();
    assert_eq!((inner.apply_attempts.attempts, inner.apply_attempts.presented), (2, 1));
    assert_eq!((inner.attempts.attempts, inner.attempts.shape_requests), (2, 2));
    let outer = outer.snapshot();
    assert_eq!((outer.attempts.attempts, outer.attempts.shape_requests), (1, 2));
    assert_eq!(outer.apply_attempts, AttemptStats::ZERO);
}

/// Every glyph-atlas insertion in `sources` as `path:line`, with whether its arguments wrap the
/// rasterizer in `CountingRasterizer`. A one-argument `get_or_insert` is `Option`'s, not an atlas
/// insertion (which takes a key and a rasterizer), so it is skipped.
fn atlas_insertions(sources: &[(String, String)]) -> Vec<(String, bool)> {
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

#[test]
fn every_glyph_atlas_insertion_counts_its_rasterizer() {
    // An insertion that passes the bare rasterizer goes uncounted. The seven sites are the four
    // terminal paths in core.rs, the shared chrome path (tabs, palette, search, preedit), the
    // working-set helper's ASCII pass and the test glyph seams' solid tile in cursor.rs.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let insertions = atlas_insertions(&sources);
    let bare: Vec<_> = insertions.iter().filter(|(_, wrapped)| !wrapped).collect();
    assert!(bare.is_empty(), "uncounted glyph-atlas insertions: {bare:#?}");
    assert_eq!(insertions.len(), 7, "the insertion sites changed; review them: {insertions:#?}");
    assert!(insertions.iter().any(|(site, _)| site.starts_with("chrome_text.rs:")));
    assert!(insertions.iter().any(|(site, _)| site.starts_with("cursor.rs:")));
}

#[test]
fn the_insertion_scan_skips_option_get_or_insert() {
    // `Option::get_or_insert` takes one argument and is never an atlas insertion; a two-argument
    // call with a nested comma inside its key is still found.
    let fixture = "fn track(pending: &mut Option<u64>, atlas: &mut Atlas, wt: &mut Raster) {\n    \
                   pending.get_or_insert(f(1, 2));\n    atlas.get_or_insert(key(1, 2), wt);\n}\n";
    let found = atlas_insertions(&[("fixture.rs".to_owned(), fixture.to_owned())]);
    assert_eq!(found, vec![("fixture.rs:3".to_owned(), false)]);
}

#[test]
fn the_insertion_scan_finds_a_bare_rasterizer_across_lines_and_line_endings() {
    // A wrapped call split over lines counts as wrapped; a bare one does not, on LF and CRLF.
    let fixture = "fn draw(atlas: &mut Atlas, wt: &mut Raster) {\n    atlas.get_or_insert(key, wt);\n    \
                   atlas.get_or_insert(\n        key,\n        &mut CountingRasterizer::new(wt),\n    );\n}\n";
    for text in [fixture.to_owned(), to_crlf(fixture)] {
        let found = atlas_insertions(&[("fixture.rs".to_owned(), text)]);
        assert_eq!(
            found,
            vec![("fixture.rs:2".to_owned(), false), ("fixture.rs:3".to_owned(), true)]
        );
    }
}

#[test]
fn the_attempt_and_the_owed_apply_are_wired_through_the_production_entry_points() {
    // render_releasing opens the attempt inside its collection scope and takes the owed apply, so
    // exactly one attempt carries it; both presenters mark the attempt where a frame passes the
    // present boundary; begin_frame_fonts times the preparation and records the change.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let bodies = function_bodies(&sources);
    let body = |name: &str| {
        bodies
            .iter()
            .find(|function| function.name == name)
            .map(|function| function.body.split_whitespace().collect::<String>())
            .unwrap_or_else(|| panic!("{name} not found"))
    };
    // The order of the scopes and the preparation's arguments live in the seams, which the
    // attribution test drives; here only their use by the real entry points is pinned.
    let releasing = body("render_releasing");
    assert!(releasing
        .starts_with("{let_scope=crate::frame_stats::RenderScope::enter(self.frame_sink.as_ref(),&mutself.unattributed_apply,);"));
    assert!(
        !releasing.contains("AttemptScope::enter") && !releasing.contains("CollectGuard::enter")
    );
    let present = to_lf(include_str!("present.rs")).split_whitespace().collect::<String>();
    assert_eq!(
        present.matches("note_attempt_presented();Ok(PresentOutcome::Presented)").count(),
        2,
        "each presenter marks the attempt where it returns Presented"
    );
    let prepare = body("begin_frame_fonts");
    assert!(prepare.contains(
        "frame_fonts::prepare_and_owe(&mutself.applied_fonts,&mutself.unattributed_apply,"
    ));
    assert!(!prepare.contains("prepare_frame_fonts("), "preparation goes through the seam only");
    assert!(body("notification_layout").contains("CollectGuard::enter(self.frame_sink.as_ref())"));
}

/// A frame's damage waste is the permille of its single union rectangle minus the permille of the
/// exact area its parts cover: a tab band and a cursor row 100 px apart on a 240x160 surface waste
/// the gap between them; parts that exactly fill the union waste nothing; no surface wastes 0.
#[test]
fn damage_waste_is_the_union_share_minus_the_covered_share() {
    let cursor_row = PixelRect { x: 0, y: 22, w: 100, h: 20 };
    let tab_band = PixelRect { x: 0, y: 140, w: 240, h: 20 };
    let union = cursor_row.union(tab_band);
    // Union 240x138 = 33120 px is 862 permille; the parts cover 6800 px, 177 permille.
    assert_eq!(damage_waste_permille(&union, &[cursor_row, tab_band], 240, 160), 862 - 177);
    assert_eq!(damage_waste_permille(&tab_band, &[tab_band], 240, 160), 0);
    let halves =
        [PixelRect { x: 0, y: 0, w: 120, h: 160 }, PixelRect { x: 120, y: 0, w: 120, h: 160 }];
    let whole = PixelRect { x: 0, y: 0, w: 240, h: 160 };
    assert_eq!(damage_waste_permille(&whole, &halves, 240, 160), 0);
    assert_eq!(damage_waste_permille(&union, &[cursor_row], 0, 160), 0);
}

/// The waste is summed per damaged frame beside the damage share, and is never computed with the
/// counting gate off.
#[test]
fn damage_waste_is_summed_only_inside_a_counting_scope() {
    note_damage_waste(|| panic!("waste computed with no counting scope"));
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_damage(|| 862);
        note_damage_waste(|| 685);
        note_damage(|| 100);
        note_damage_waste(|| 0);
    }
    let stats = sink.snapshot();
    assert_eq!((stats.damage_waste_permille_sum, stats.damaged_frames), (685, 2));
    let mut total = FrameStats::ZERO;
    total.add(&stats);
    total.add(&stats);
    assert_eq!(total.damage_waste_permille_sum, 1370);
}

/// The partial-assembly counters record only inside a counting scope: a presented frame counts as
/// partial only when its mode was `Partial`, each fallback counts once, and the hashed cells are
/// summed lazily, so with the gate off the cell count is never computed. `add` folds all three.
#[test]
fn partial_counters_record_only_inside_a_counting_scope() {
    note_partial_frame(true);
    note_partial_fallback();
    note_row_cells_hashed(|| panic!("cells counted with no counting scope"));
    let sink = FrameStatsSink::default();
    {
        let _collect = CollectGuard::enter(Some(&sink));
        note_partial_frame(true);
        note_partial_frame(false);
        note_partial_fallback();
        note_row_cells_hashed(|| 80);
        note_row_cells_hashed(|| 40);
    }
    let stats = sink.snapshot();
    assert_eq!(
        (stats.partial_frames, stats.partial_fallbacks, stats.row_cells_hashed),
        (1, 1, 120)
    );
    let mut total = FrameStats::ZERO;
    total.add(&stats);
    total.add(&stats);
    assert_eq!(
        (total.partial_frames, total.partial_fallbacks, total.row_cells_hashed),
        (2, 2, 240)
    );
}
