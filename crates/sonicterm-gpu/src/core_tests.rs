use super::*;
pub(super) use sonicterm_render_model::boundary::ui::tabs::TITLE_FIT_TOLERANCE_PX;
pub(super) use sonicterm_types::{ClassCoverage, PaneSeamTerm};

pub(super) fn revision_plan(id: u64, revision: u64) -> FramePlan {
    FramePlan::build(
        FrameFacts {
            window: WindowIdentity { width: 80, height: 40, ..Default::default() },
            cell_w: 10.0,
            cell_h: 20.0,
            padding: [0.0; 4],
            vertical_ink_pad: 0.0,
            scrollbar_mode: ScrollbarMode::Never,
            degraded: false,
            tab_bar_top: None,
            status_band: None,
            scale: 1.0,
            previous_recolor: crate::cursor::RecolorRecord::default(),
        },
        [PaneMetadata {
            id,
            revision,
            dirty_generation: 0,
            rect: PixelRect { x: 0, y: 0, w: 80, h: 40 },
            cols: 8,
            rows: 2,
            scrollback_len: 0,
            viewport_top_abs: None,
            is_active: true,
            is_alt: false,
            scrollbar_alpha: 0.0,
            dirty_rows: vec![0, 1],
            row_ink: Vec::new(),
        }],
        None,
    )
}

/// A plan's receipts, applied to the same panes, as a presented frame's would be.
pub(super) fn acknowledge_plan(
    plan: &FramePlan,
    panes: &mut [sonicterm_render_model::PaneRender<'_>],
) -> usize {
    let receipts = presented_receipts(plan, panes);
    acknowledge_receipts(&receipts, panes)
}

/// Hold the shared font fixture even after a failed sibling test poisoned it, so one failure
/// cannot fail every later test that shapes text.
pub(super) fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The width a title draws at in the tab font, as the tab draw loop lays it out.
pub(super) fn drawn_tab_title_px(
    stack: &sonicterm_engine::FontStack,
    title: &str,
    size_px: f32,
) -> f32 {
    let mut raster = stack.clone();
    let mut atlas = GlyphAtlas::new(2048, 2048);
    chrome_text::layout_with_raster_variant(
        stack,
        &mut raster,
        &mut atlas,
        title,
        ChromeColor::WHITE,
        ChromeAttrs::default(),
        size_px,
        size_px,
        (0.0, 20.0),
        (4096.0, 256.0),
        None,
        GlyphRasterVariant::TabTitle,
    )
    .width_px
}

/// Stand-in for the tab font in hash tests: ten pixels per character of the drawn text.
pub(super) fn ten_px_per_char(content: &TabContent<'_>) -> Option<f32> {
    Some(content.display_text().chars().count() as f32 * 10.0)
}

/// The longest `status-checklist-footer` across the shipped locales, by character count.
pub(super) fn longest_localized_footer(key: &str) -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/i18n");
    let prefix = format!("{key} = ");
    ["en", "ja", "zh-CN"]
        .iter()
        .filter_map(|locale| std::fs::read_to_string(root.join(locale).join("messages.ftl")).ok())
        .filter_map(|text| {
            text.lines().find_map(|line| line.strip_prefix(prefix.as_str())).map(str::to_owned)
        })
        .max_by_key(|footer| footer.chars().count())
        .expect("every locale has the footer")
}

// ---------------------------------------------------------------------------
// Renderer retention reporting
//
// `GpuRenderer` needs a real adapter, so it cannot be built here. The
// aggregation and class mapping are pure and are tested directly; what a live
// renderer puts into them is covered by the atlas and software-frame suites
// that already exist.
// ---------------------------------------------------------------------------

pub(super) fn amount(bytes: usize, items: usize) -> ResourceAmount {
    ResourceAmount { bytes, items }
}

/// `finish_successful_frame` in `source`, up to the next method at the same indentation, so every
/// later line of it stays in range.
pub(super) fn finish_successful_frame_source(source: &str) -> String {
    // A CRLF checkout (Windows) is read as LF, so the `\n` search below finds the same boundary.
    let source = source.replace("\r\n", "\n");
    method_body(&source, "    pub(super) fn finish_successful_frame(").to_owned()
}

pub(super) fn selection_for_rows(start: u64, end: u64) -> Selection {
    Selection {
        start: (start, 1),
        end: (end, 2),
        anchored: true,
        pane_id: Some(1),
        content_seq: 0,
        on_alt_screen: false,
        scrollback_evicted: 0,
        content_fingerprint: None,
    }
}

/// The transition rule `set_hover_cursor` returns: the hovered tab differs
/// between the previous and the next pointer position. The source scan below
/// pins that the method's body is exactly this comparison.
pub(super) fn hover_tab_changed(
    tabs: &TabBar,
    geometry: TabBarHoverGeometry,
    previous: Option<(f32, f32)>,
    next: Option<(f32, f32)>,
) -> bool {
    hovered_tab_at(tabs, geometry, previous) != hovered_tab_at(tabs, geometry, next)
}

/// Two-tab bar used by the hover tests, with its drawn tab rects.
pub(super) fn hover_test_bar(
    geometry: TabBarHoverGeometry,
) -> (TabBar, Vec<sonicterm_render_model::boundary::ui::tabbar_view::Rect>) {
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    let layout =
        TabBarLayout::compute_with_height(&tabs, geometry.width_px, geometry.bar_height_px)
            .with_top_offset(geometry.top_offset_px);
    let rects = layout.tabwidgets().iter().map(|widget| widget.bg_rect).collect();
    (tabs, rects)
}

/// A top bar and a bottom bar, so the hit test is checked against both offsets.
pub(super) fn hover_test_geometries() -> [TabBarHoverGeometry; 2] {
    let top = TabBarHoverGeometry {
        width_px: 400.0,
        bar_height_px: 40.0,
        top_offset_px: 0.0,
        visible: true,
    };
    [top, TabBarHoverGeometry { top_offset_px: 560.0, ..top }]
}

/// A stamp at `allocation`, `identity` and `growths` on device generation 7.
pub(super) fn growth_stamp(allocation: u64, identity: u64, growths: u64) -> GlyphContentStamp {
    GlyphContentStamp {
        device_generation: 7,
        allocation_generation: allocation,
        content_identity: identity,
        growths,
    }
}

/// The source of `name`'s body in `source`, from its signature to the next top-level item.
pub(super) fn function_body(source: &str, signature: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start + signature.len()..]
        .find("\n    pub fn ")
        .map_or(source.len(), |offset| start + signature.len() + offset);
    source[start..end].to_owned()
}

/// The body of the method whose signature starts at `signature`, through its closing brace at
/// method indentation.
pub(super) fn method_body(source: &str, signature: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start..].find("\n    }\n").map_or(source.len(), |offset| start + offset);
    source[start..end].to_owned()
}

/// The body of the item whose signature is `signature` in `source`: from the signature to the
/// closing brace at the signature's own indentation, so a method ends at its own `}`.
pub(super) fn item_body(source: &str, signature: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let indent = &source[line_start..start];
    let indent = &indent[..indent.len() - indent.trim_start().len()];
    let close = format!("\n{indent}}}\n");
    let body = &source[start..];
    let end = body.find(&close).map_or(body.len(), |offset| offset + close.len());
    body[..end].to_owned()
}

#[path = "core_raster_fixture.rs"]
mod core_raster_fixture;
pub(super) use core_raster_fixture::*;

#[cfg(test)]
#[path = "core_tests_parity_fixture_tests.rs"]
mod core_parity_fixture;
pub(super) use core_parity_fixture::*;

#[path = "core_row_fixture.rs"]
mod core_row_fixture;
pub(crate) use core_row_fixture::warm_and_cold_row_glyphs;
pub(super) use core_row_fixture::{
    assemble_pass, begin_pass, counted, emit_pass_recording, fallback_facts, fallback_pane,
    instance_bytes, oracle_block, oracle_natural, oracle_shaped, policy_plan, present_pass,
    strip_records, text_grid, write_row, AtlasChange, EmittedRow, FallbackFixture, GlyphRig,
    OracleCell, Widening,
};

#[path = "core_oracle_fixture.rs"]
mod core_oracle_fixture;
pub(super) use core_oracle_fixture::*;

/// Inline media is a shipping rendering phase; only the row-shape monotonicity check is debug-only.
#[test]
fn inline_image_module_is_available_without_debug_assertions() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let declaration = "#[path = \"core_inline_images.rs\"]\nmod inline_images;";
    assert!(source.contains(declaration));
    assert!(!source.contains(&format!("#[cfg(debug_assertions)]\n{declaration}")));
}

/// All shared source selectors return the same bounded body from LF and Windows CRLF checkouts.
#[test]
fn source_body_selectors_normalize_checkout_line_endings() {
    let source = "impl Renderer {\n    pub fn first() {\n        draw();\n    }\n    pub fn second() {\n        present();\n    }\n}\n";
    let windows = source.replace('\n', "\r\n");
    for select in [function_body, method_body, item_body] {
        let expected = select(source, "pub fn first(");
        assert!(expected.contains("draw();"));
        assert!(!expected.contains("present();"));
        assert_eq!(select(&windows, "pub fn first("), expected);
    }
}
