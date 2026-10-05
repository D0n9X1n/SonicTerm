use super::*;
use sonicterm_render_model::boundary::ui::tabs::TITLE_FIT_TOLERANCE_PX;
use sonicterm_types::{ClassCoverage, PaneSeamTerm};

#[test]
fn broadcast_warning_keeps_red_highlighting_without_label_text() {
    // Production uses the overlay layer so terminal ink cannot cover safety edges; no banner glyphs remain.
    let source: String = include_str!("core.rs").chars().filter(|ch| !ch.is_whitespace()).collect();
    let start = source.find("pubfnrender(").unwrap();
    let end = source[start..].find("fnfinish_successful_frame(").unwrap() + start;
    let render = &source[start..end];
    assert!(!render.contains("BROADCAST"), "broadcast chrome must not emit warning text");
    // The overlay quads are the frame scratch's, reborrowed from the pass's lease.
    assert!(render.contains("emit_broadcast_borders(&mut*quads_overlay,"));
    assert!(render.contains("theme.colors.bright.red"));
    assert!(render.contains("broadcast_participants_hash"));
}

#[test]
fn broadcast_borders_outline_only_participants_with_thin_edges() {
    // Both participants receive four exact 2px edges; the unrelated pane and interior stay untouched.
    let panes = [
        (1, PaneRect::new(10.0, 20.0, 100.0, 80.0)),
        (2, PaneRect::new(110.0, 20.0, 100.0, 80.0)),
        (3, PaneRect::new(210.0, 20.0, 100.0, 80.0)),
    ];
    let warning = [1.0, 0.0, 0.0, 1.0];
    let mut quads = Vec::new();
    emit_broadcast_borders(&mut quads, &panes, &[1, 2], warning, 400.0, 200.0);
    assert_eq!(quads.len(), 8);
    for (index, x) in [10.0, 110.0].into_iter().enumerate() {
        let expected = [
            px_to_ndc(x, 20.0, 100.0, 2.0, 400.0, 200.0),
            px_to_ndc(x, 98.0, 100.0, 2.0, 400.0, 200.0),
            px_to_ndc(x, 22.0, 2.0, 76.0, 400.0, 200.0),
            px_to_ndc(x + 98.0, 22.0, 2.0, 76.0, 400.0, 200.0),
        ];
        for (quad, rect) in quads[index * 4..index * 4 + 4].iter().zip(expected) {
            assert_eq!(quad.rect, rect);
            assert_eq!(quad.color, warning);
        }
    }
    quads.clear();
    emit_broadcast_borders(&mut quads, &panes, &[], warning, 400.0, 200.0);
    assert!(quads.is_empty());
}

#[test]
fn broadcast_borders_fit_tiny_panes_and_skip_empty_rectangles() {
    // A 1px-wide pane clamps all edges to 0.5px without crossing its bounds or overlapping corners.
    let panes = [
        (1, PaneRect::new(10.0, 20.0, 1.0, 3.0)),
        (2, PaneRect::new(30.0, 20.0, 0.0, 30.0)),
        (3, PaneRect::new(40.0, 20.0, 30.0, 0.0)),
    ];
    let mut quads = Vec::new();
    emit_broadcast_borders(&mut quads, &panes, &[1, 2, 3], [1.0, 0.0, 0.0, 1.0], 100.0, 100.0);
    let expected = [
        px_to_ndc(10.0, 20.0, 1.0, 0.5, 100.0, 100.0),
        px_to_ndc(10.0, 22.5, 1.0, 0.5, 100.0, 100.0),
        px_to_ndc(10.0, 20.5, 0.5, 2.0, 100.0, 100.0),
        px_to_ndc(10.5, 20.5, 0.5, 2.0, 100.0, 100.0),
    ];
    assert_eq!(quads.len(), 4);
    assert_eq!(quads.iter().map(|quad| quad.rect).collect::<Vec<_>>(), expected);
}

fn revision_plan(id: u64, revision: u64) -> FramePlan {
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
fn acknowledge_plan(
    plan: &FramePlan,
    panes: &mut [sonicterm_render_model::PaneRender<'_>],
) -> usize {
    let receipts = presented_receipts(plan, panes);
    acknowledge_receipts(&receipts, panes)
}

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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let start = source.find("    pub fn render(").unwrap();
    let end = source[start..].find("    fn finish_successful_frame(").unwrap() + start;
    let render = &source[start..end];
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
    // The frame finishes once, and only after its presenter reports `Presented`.
    let handoff = render.find("self.present_frame(&layers, &mut gpu_timing)?;").unwrap();
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

/// A style run and the row it shapes into, for driving the record builder directly.
struct ShapeRunFixture<'run> {
    atlas: &'run mut GlyphAtlas,
    row: u16,
    style: RunStyle,
    cells: &'run [(u16, Cell)],
    theme: &'run Theme,
    fg_default: ChromeColor,
    cell_size: (f32, f32),
    origin: (f32, f32),
    surface: (f32, f32),
    baseline_y_in_cell: f32,
    snapped_cell_x: &'run [f32],
    font_stack: Option<&'run sonicterm_engine::FontStack>,
    wt_raster: Option<&'run mut sonicterm_engine::FontStack>,
    hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    hovered_url_accent: [f32; 4],
    software_presenter: bool,
}

/// Build one style run's records and project them at `fixture.row`, as a missed row is drawn,
/// appending glyphs, tofu and missing characters; returns the run's completeness.
fn shape_run_for_test(
    fixture: ShapeRunFixture<'_>,
    glyphs: &mut Vec<GlyphInstance>,
    tofu: &mut Vec<(f32, f32, f32, f32, ChromeColor)>,
    missing: &mut Vec<char>,
) -> bool {
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    // The builder takes cells borrowed from a grid; the fixture owns its cells.
    let borrowed: Vec<(u16, &Cell)> = fixture.cells.iter().map(|(col, cell)| (*col, cell)).collect();
    let complete = GpuRenderer::build_shape_run(
        fixture.atlas,
        &mut records,
        fixture.row,
        fixture.style,
        &borrowed,
        fixture.theme,
        fixture.fg_default,
        fixture.cell_size.0,
        fixture.cell_size.1,
        fixture.origin.1,
        fixture.snapped_cell_x,
        fixture.font_stack,
        fixture.wt_raster,
        fixture.hovered_url_cells,
        fixture.hovered_url_accent,
        fixture.software_presenter,
    );
    let at = RowPlacement {
        slot: fixture.row,
        origin: fixture.origin,
        cols: (fixture.snapped_cell_x.len().saturating_sub(1)) as u16,
        snapped_cell_x: fixture.snapped_cell_x,
        cell_size: fixture.cell_size,
        baseline_y_in_cell: fixture.baseline_y_in_cell,
        surface: fixture.surface,
    };
    let (mut underlines, mut spans) = (Vec::new(), Vec::new());
    project_cached_row(
        &records,
        &at,
        fixture.software_presenter,
        GlyphFrame {
            glyph_instances: glyphs,
            underlines: &mut underlines,
            missing_tofu: tofu,
            missing_chars_this_frame: missing,
            row_spans: &mut spans,
        },
    );
    complete
}

#[derive(Clone, Default)]
struct GlyphLogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for GlyphLogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for GlyphLogCapture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

// Real ASCII and shaped emission keeps white masks/color sentinels ordinary and cache geometry unchanged.
#[test]
fn ordinary_white_and_color_glyph_emission_does_not_warn() {
    use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
    use tracing_subscriber::{layer::SubscriberExt, Layer};
    struct Tile(bool);
    impl Rasterizer for Tile {
        fn rasterize(&mut self, _: sonicterm_types::GlyphKey) -> Option<RasterTile> {
            Some(RasterTile {
                width: 2,
                height: 3,
                offset_x: 1,
                offset_y: -2,
                advance: 2.0,
                coverage: if self.0 { [8, 16, 32, 64].repeat(6) } else { vec![255; 6] },
                is_color: self.0,
                is_subpixel: false,
            })
        }
    }
    let captured = GlyphLogCapture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(captured.clone())
            .with_filter(tracing_subscriber::EnvFilter::new("sonic=warn")),
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(target: "sonic::render::glyph", "glyph-positive-control");
        let mut stack = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
            &[("Rec Mono St.Helens", false)],
            vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")],
            14.0,
            72,
            1.0,
        )
        .unwrap();
        let shaper = stack.clone();
        for ch in ['A', '='] {
            for is_color in [false, true] {
                for foreground in [Color::Rgb(255, 255, 255), Color::Rgb(20, 40, 60)] {
                    let mut atlas = GlyphAtlas::new(32, 32);
                    let cell = Cell::plain(ch, foreground, Color::Default, CellFlags::empty());
                    let style = RunStyle::from_cell(&cell);
                    let key = if ch == 'A' {
                        sonicterm_types::GlyphKey::new(ch, false, false)
                    } else {
                        let glyph = shaper.shape_text(&ch.to_string()).unwrap().remove(0);
                        sonicterm_types::GlyphKey::shaped(
                            ch,
                            glyph.font_idx as u8,
                            glyph.glyph_pos,
                            false,
                            false,
                        )
                    };
                    let info = atlas.get_or_insert(key, &mut Tile(is_color)).unwrap();
                    let pixels = atlas.pixels().to_vec();
                    let mut glyphs = Vec::new();
                    let mut tofu = Vec::new();
                    let mut missing = Vec::new();
                    let _complete = shape_run_for_test(
                        ShapeRunFixture {
                            atlas: &mut atlas,
                            row: 0,
                            style,
                            cells: &[(0, cell)],
                            theme: &Theme::default(),
                            fg_default: ChromeColor::rgb(255, 255, 255),
                            cell_size: (10.0, 20.0),
                            origin: (0.0, 0.0),
                            surface: (100.0, 100.0),
                            baseline_y_in_cell: 15.0,
                            snapped_cell_x: &[0.0, 10.0],
                            font_stack: Some(&shaper),
                            wt_raster: Some(&mut stack),
                            hovered_url_cells: None,
                            hovered_url_accent: [0.0; 4],
                            software_presenter: false,
                        },
                        &mut glyphs,
                        &mut tofu,
                        &mut missing,
                    );
                    assert_eq!(glyphs.len(), 1);
                    assert!(tofu.is_empty() && missing.is_empty());
                    assert_eq!(glyphs[0].rect, px_to_ndc(1.0, 13.0, 2.0, 3.0, 100.0, 100.0));
                    assert_eq!(glyphs[0].uv, info.uv);
                    assert_eq!(glyphs[0].flags, glyph_flags(is_color, false));
                    let expected_color = if is_color {
                        [1.0; 4]
                    } else {
                        chrome_color_to_linear_rgba(color_to_chrome(
                            foreground,
                            &Theme::default(),
                            ChromeColor::rgb(255, 255, 255),
                        ))
                    };
                    assert_eq!(glyphs[0].color, expected_color);
                    assert_eq!(atlas.pixels(), pixels);
                    assert_eq!(atlas.hits(), 1);
                }
            }
        }
    });
    let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("glyph-positive-control"));
    assert!(!output.contains("emitting a glyph in pure white"), "{output}");
}

// Genuine atlas pressure still emits its production warning, while unchanged media stays quiet.
#[test]
fn atlas_pressure_warning_remains_observable() {
    use tracing_subscriber::{layer::SubscriberExt, Layer};
    let captured = GlyphLogCapture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(captured.clone())
            .with_filter(tracing_subscriber::EnvFilter::new("sonic=warn")),
    );
    tracing::subscriber::with_default(subscriber, || {
        let atlas = GlyphAtlas::new(1, 1);
        report_inline_image_pressure(true, 1, &atlas);
        report_inline_image_pressure(false, 1, &atlas);
        report_inline_image_pressure(true, 0, &atlas);
    });
    let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        output
            .matches("inline image atlas full; skipped older images without evicting text glyphs")
            .count(),
        1
    );
    assert!(output.contains("skipped=1"));
}

/// Software adapters select low reserve while hardware keeps the performance policy.
///
/// Exercising both classification values pins the policy seam before descriptor construction.
#[test]
fn device_memory_policy_selects_usage_for_software_and_performance_for_hardware() {
    assert_eq!(device_memory_policy_from(true), DeviceMemoryPolicy::MemoryUsage);
    assert_eq!(device_memory_policy_from(false), DeviceMemoryPolicy::Performance);
}

/// Device descriptors carry the policy's exact wgpu memory hint.
///
/// Inspecting both descriptors prevents device creation from silently reverting to the default.
#[test]
fn device_descriptor_uses_the_selected_memory_hint() {
    assert!(matches!(
        device_descriptor_for(true, wgpu::Features::empty()).memory_hints,
        wgpu::MemoryHints::MemoryUsage
    ));
    assert!(matches!(
        device_descriptor_for(false, wgpu::Features::empty()).memory_hints,
        wgpu::MemoryHints::Performance
    ));
}

/// Windows requests dual-source blending only when the adapter advertises it.
#[test]
fn optional_device_features_select_dual_source_only_on_supported_windows() {
    let dual = wgpu::Features::DUAL_SOURCE_BLENDING;

    assert_eq!(selected_optional_device_features(dual, true), dual);
    assert!(selected_optional_device_features(wgpu::Features::empty(), true).is_empty());
    assert!(selected_optional_device_features(dual, false).is_empty());
}

/// The descriptor requests exactly the selected optional feature set.
#[test]
fn device_descriptor_carries_selected_optional_features() {
    let dual = wgpu::Features::DUAL_SOURCE_BLENDING;

    assert_eq!(device_descriptor_for(false, dual).required_features, dual);
    assert_eq!(
        device_descriptor_for(true, wgpu::Features::empty()).required_features,
        wgpu::Features::empty()
    );
}

/// Effective LCD mode requires Windows, an opaque target, and a viable presenter path.
#[test]
fn effective_subpixel_aa_policy_falls_back_deterministically() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::{Bgr, Off, Rgb};

    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, false, true), Rgb);
    assert_eq!(effective_subpixel_aa_mode(Bgr, true, true, false, true), Bgr);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, true, false), Rgb);
    assert_eq!(effective_subpixel_aa_mode(Off, true, true, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, false, true, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, false, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, false, false), Off);
}

/// Allocator projection retains totals, counts, and the largest block without labels.
///
/// Distinct totals and uneven block sizes expose field swaps, bad counts, and a wrong maximum.
#[test]
fn allocator_snapshot_maps_report_totals_counts_and_largest_block() {
    let report = wgpu::AllocatorReport {
        allocations: vec![
            wgpu::wgt::AllocationReport {
                name: String::from("must-not-be-read"),
                offset: 0,
                size: 19,
            },
            wgpu::wgt::AllocationReport {
                name: String::from("also-must-not-be-read"),
                offset: 64,
                size: 23,
            },
        ],
        blocks: vec![
            wgpu::wgt::MemoryBlockReport { size: 128, allocations: 0..1 },
            wgpu::wgt::MemoryBlockReport { size: 512, allocations: 1..2 },
            wgpu::wgt::MemoryBlockReport { size: 256, allocations: 2..2 },
        ],
        total_allocated_bytes: 42,
        total_reserved_bytes: 896,
    };

    assert_eq!(
        allocator_snapshot_from(&report),
        AllocatorSnapshot {
            allocated_bytes: 42,
            reserved_bytes: 896,
            allocations: 2,
            blocks: 3,
            largest_block_bytes: 512,
        }
    );
}

/// An unavailable backend report remains absence rather than a zero-valued snapshot.
///
/// Passing `None` through the production adapter preserves capability state for callers.
#[test]
fn allocator_snapshot_preserves_an_unavailable_report_as_none() {
    assert_eq!(allocator_snapshot_from_report(None), None);
}

#[test]
fn renderer_resize_has_no_unchecked_wrapper() {
    const SOURCE: &str = include_str!("core.rs");
    assert!(!SOURCE.contains("pub fn resize(&mut self, width: u32, height: u32)"));
    let lines = SOURCE.lines().collect::<Vec<_>>();
    assert!(lines.windows(2).any(|pair| {
        pair[0].trim() == "#[must_use]" && pair[1].trim_start().starts_with("pub fn try_resize")
    }));
}

// --- Inline IME preedit opaque background -------------------------

#[test]
fn badge_width_never_uses_a_narrower_shaped_or_invalid_measurement() {
    assert_eq!(conservative_badge_text_width(120.0, Some(160.0)), 160.0);
    assert_eq!(conservative_badge_text_width(120.0, Some(90.0)), 120.0);
    assert_eq!(conservative_badge_text_width(120.0, Some(f32::NAN)), 120.0);
    assert_eq!(conservative_badge_text_width(120.0, None), 120.0);
}

#[test]
fn preedit_bg_rect_covers_the_glyph_run() {
    // The glyphs are emitted at emit_x = start_x + pad, across `pre_w`.
    // The background mask must start no later than start_x and extend past
    // the glyph run's right edge, so app placeholder/hint text under the
    // composing pinyin is fully masked.
    let start_x = 100.0;
    let top_y = 50.0;
    let pre_w = 64.0;
    let pad = 2.0;
    let line_h = 20.0;

    let (x, y, w, h) = preedit_bg_rect(start_x, top_y, pre_w, pad, line_h);

    // Left edge aligns with the cursor cell (no later than where glyphs start).
    assert_eq!(x, start_x);
    assert!(x <= start_x + pad, "mask starts at/under the glyph emit_x");
    // One line tall, anchored at the cell top.
    assert_eq!(y, top_y);
    assert_eq!(h, line_h);
    // Right edge reaches past the glyph run end (emit_x + pre_w).
    let glyph_right = (start_x + pad) + pre_w;
    assert!(x + w >= glyph_right, "mask must cover the full glyph run, got right={}", x + w);
}

#[test]
fn preedit_bg_rect_width_is_at_least_pre_w() {
    // Width must never be narrower than the run width used to lay glyphs,
    // otherwise the tail of the composing text shows through to whatever is
    // underneath. It also must not be absurdly wide (bleeding onto adjacent
    // cells): width == pre_w + pad exactly.
    let (_, _, w, _) = preedit_bg_rect(0.0, 0.0, 80.0, 2.0, 18.0);
    assert!(w >= 80.0, "mask at least as wide as the glyph run");
    assert_eq!(w, 82.0, "mask is exactly pre_w + pad, not wider");
}

#[test]
fn preedit_bg_rect_zero_pad_equals_pre_w() {
    let (_, _, w, _) = preedit_bg_rect(10.0, 10.0, 40.0, 0.0, 16.0);
    assert_eq!(w, 40.0);
}

// --- Dim / faint text (SGR 2), -------------------------------------

#[test]
fn dim_toward_endpoints_and_midpoint() {
    let fg = ChromeColor::rgba(200, 100, 40, 255);
    let bg = ChromeColor::rgb(0, 0, 0);
    // t = 0 → unchanged fg.
    assert_eq!(dim_toward(fg, bg, 0.0), fg);
    // t = 1 → exactly bg (but fg's alpha preserved).
    let full = dim_toward(fg, bg, 1.0);
    assert_eq!((full.r(), full.g(), full.b()), (0, 0, 0));
    assert_eq!(full.a(), 255, "alpha is preserved, not blended");
    // t = 0.5 → halfway each channel.
    let mid = dim_toward(fg, bg, 0.5);
    assert_eq!((mid.r(), mid.g(), mid.b()), (100, 50, 20));
}

#[test]
fn dim_toward_clamps_factor_and_preserves_alpha() {
    let fg = ChromeColor::rgba(255, 255, 255, 128);
    let bg = ChromeColor::rgb(0, 0, 0);
    // Out-of-range t is clamped (no panic, no overshoot).
    assert_eq!(dim_toward(fg, bg, -1.0), fg, "t<0 clamps to 0 → unchanged");
    let over = dim_toward(fg, bg, 2.0);
    assert_eq!((over.r(), over.g(), over.b()), (0, 0, 0), "t>1 clamps to 1 → bg");
    assert_eq!(over.a(), 128, "alpha untouched");
}

#[test]
fn cell_fg_dims_faint_text_toward_background() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    let fg = Color::Rgb(200, 200, 200);
    let bg = Color::Rgb(0, 0, 0);

    let normal = Cell::plain('x', fg, bg, CellFlags::empty());
    let faint = Cell::plain('x', fg, bg, CellFlags::DIM);

    let normal_c = cell_fg(&normal, &theme, default);
    let faint_c = cell_fg(&faint, &theme, default);

    // Regression: the faint cell must NOT equal the normal cell,
    // and must be strictly dimmer on every channel (closer to the black bg).
    assert_ne!(faint_c, normal_c, "dim text must differ from normal text");
    assert!(faint_c.r() < normal_c.r(), "dim R should be lower");
    assert!(faint_c.g() < normal_c.g(), "dim G should be lower");
    assert!(faint_c.b() < normal_c.b(), "dim B should be lower");
}

#[test]
fn cell_fg_leaves_normal_text_unchanged() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    let fg = Color::Rgb(123, 200, 50);
    let cell = Cell::plain('x', fg, Color::Default, CellFlags::empty());
    // No DIM → exactly the resolved fg, no blending.
    assert_eq!(cell_fg(&cell, &theme, default), ChromeColor::rgb(123, 200, 50));
}

#[test]
fn cell_fg_dim_with_inverse_dims_swapped_foreground() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    // INVERSE: the glyph is painted in the cell's bg color over the fg.
    let fg = Color::Rgb(0, 0, 0);
    let bg = Color::Rgb(200, 200, 200);

    let inverse = Cell::plain('x', fg, bg, CellFlags::INVERSE);
    let inverse_dim = Cell::plain('x', fg, bg, CellFlags::INVERSE | CellFlags::DIM);

    let inv_c = cell_fg(&inverse, &theme, default);
    let inv_dim_c = cell_fg(&inverse_dim, &theme, default);

    // Inverse foreground resolves to the cell bg (200,200,200); DIM then
    // pulls it toward the swapped background (the cell fg = black), so each
    // channel must drop.
    assert_eq!(inv_c, ChromeColor::rgb(200, 200, 200));
    assert_ne!(inv_dim_c, inv_c, "inverse+dim must still dim");
    assert!(inv_dim_c.r() < inv_c.r() && inv_dim_c.g() < inv_c.g() && inv_dim_c.b() < inv_c.b());
}

#[test]
fn detects_cpu_device_type_as_software() {
    // Even a "GPU-sounding" name is software if the device type is CPU.
    assert!(software_rendering_from("Some Virtual GPU", wgpu::DeviceType::Cpu));
}

#[test]
fn detects_known_software_rasterizers_by_name() {
    assert!(software_rendering_from(
        "Microsoft Basic Render Driver",
        wgpu::DeviceType::DiscreteGpu
    ));
    assert!(software_rendering_from("llvmpipe (LLVM 15.0.7, 256 bits)", wgpu::DeviceType::Other));
    assert!(software_rendering_from("Google SwiftShader", wgpu::DeviceType::Other));
}

#[test]
fn does_not_flag_real_gpus() {
    assert!(!software_rendering_from("NVIDIA GeForce RTX 4090", wgpu::DeviceType::DiscreteGpu));
    assert!(!software_rendering_from("Apple M3 Max", wgpu::DeviceType::IntegratedGpu));
    assert!(!software_rendering_from("Intel(R) Iris(R) Xe", wgpu::DeviceType::IntegratedGpu));
}

#[test]
fn unfocused_window_dims_the_active_panel_marker_rather_than_hiding_it() {
    // The accent answers "which tab is active", which is true of the window
    // whether or not it holds keyboard focus. Suppressing it on blur made an
    // unfocused window say nothing about its own state. It now dims instead,
    // so both facts stay readable at once.
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.set_active_custom_color("#fabd2f");
    tabs.activate(0);
    tabs.set_active_custom_color("#83a598");

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );

    let emit = |alpha: f32| {
        let mut quads = Vec::new();
        emit_tab_bar_quads(
            &mut quads,
            &layout,
            &TabBarQuadParams {
                accent: [1.0, 0.0, 0.0, 1.0],
                separator: [0.5, 0.5, 0.5, 1.0],
                border: [0.0, 0.0, 0.0, 1.0],
                hover_tab_idx: u32::MAX,
                surface: (400.0, 80.0),
                active_panel_marker_alpha: alpha,
            },
        );
        quads
    };

    let focused = emit(ACTIVE_PANEL_MARKER_ALPHA_FOCUSED);
    let unfocused = emit(ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED);

    // Asserting only that a quad exists would pass if the dimming were
    // dropped, and asserting only the count would pass if the color were
    // wrong. Both the presence and the difference have to hold.
    assert_eq!(
        unfocused.len(),
        focused.len(),
        "the unfocused bar must still be drawn, not omitted"
    );

    // Located by geometry, not by position in the list: the separator quad is
    // pushed after the accent, so `.last()` returns the separator and would
    // compare two identical greys that never change with focus — passing
    // whatever the accent did.
    let accent_rect = {
        let t = layout.tabs.iter().find(|t| layout.active == Some(t.idx)).expect("an active tab");
        let scale = (t.bg_rect.h
            / (sonicterm_render_model::boundary::ui::tabbar_view::TAB_BAR_HEIGHT
                - 2.0 * sonicterm_render_model::boundary::ui::tabbar_view::TAB_VERT_INSET))
            .max(0.1);
        let inset =
            sonicterm_render_model::boundary::ui::tabbar_view::ACTIVE_TOP_ACCENT_INSET * scale;
        px_to_ndc(
            t.bg_rect.x + inset,
            t.bg_rect.y + 1.0 * scale,
            (t.bg_rect.w - inset * 2.0).max(0.0),
            sonicterm_render_model::boundary::ui::tabbar_view::ACTIVE_TOP_ACCENT_H * scale,
            400.0,
            80.0,
        )
    };
    let find_accent = |quads: &[QuadInstance]| {
        *quads.iter().find(|q| q.rect == accent_rect).expect("an accent quad at the accent rect")
    };

    let focused_accent = find_accent(&focused);
    let unfocused_accent = find_accent(&unfocused);
    assert_ne!(
        focused_accent.color, unfocused_accent.color,
        "the unfocused bar must be visibly dimmer, not identical"
    );

    // Premultiplied blending: every channel scales together. Alpha alone
    // would leave the bar brighter than its alpha claims.
    assert_eq!(focused_accent.color, [0.226_965_87, 0.376_262_13, 0.313_988_72, 1.0]);
    assert_eq!(
        unfocused_accent.color,
        scale_premultiplied_alpha(focused_accent.color, ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED)
    );
}

/// Quad opacity changes must use helpers that rescale RGB with premultiplied alpha.
#[test]
fn authored_quad_sources_do_not_mutate_alpha_channels_directly() {
    for (name, source) in [("core", include_str!("core.rs")), ("quad", include_str!("quad.rs"))] {
        assert!(!source.contains("[3] ="), "{name} directly replaces quad alpha");
        assert!(!source.contains("[3] *="), "{name} directly scales only quad alpha");
    }
}

#[test]
fn a_fully_transparent_marker_alpha_emits_no_accent_quad() {
    // The alpha is not a visibility flag, but zero still has to mean absent
    // rather than an invisible quad occupying a draw slot.
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.activate(0);

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );
    let mut quads = Vec::new();
    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            accent: [1.0, 0.0, 0.0, 1.0],
            separator: [0.5, 0.5, 0.5, 1.0],
            border: [0.0, 0.0, 0.0, 1.0],
            hover_tab_idx: u32::MAX,
            surface: (400.0, 80.0),
            active_panel_marker_alpha: 0.0,
        },
    );

    assert_eq!(quads.len(), 3);
}

#[test]
fn custom_tab_color_emits_focused_panel_marker_once() {
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.set_active_custom_color("#fabd2f");
    tabs.activate(0);
    tabs.set_active_custom_color("#83a598");

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );
    let mut quads = Vec::new();
    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            accent: [1.0, 0.0, 0.0, 1.0],
            separator: [0.5, 0.5, 0.5, 1.0],
            border: [0.0, 0.0, 0.0, 1.0],
            hover_tab_idx: u32::MAX,
            surface: (400.0, 80.0),
            active_panel_marker_alpha: ACTIVE_PANEL_MARKER_ALPHA_FOCUSED,
        },
    );

    assert_eq!(quads.len(), 4);
}

#[test]
fn preedit_overlay_skips_whitespace_only_preedit() {
    // Regression: a whitespace-only preedit (macOS can momentarily deliver a
    // bare space as marked text during ordinary typing) carries no glyph
    // ink, but the inline overlay's underline is clamped to >= one cell —
    // so drawing it left a stray ~1-cell underscore at the cursor that
    // lingered until the next repaint. The overlay must be suppressed.
    assert!(!preedit_has_visible_ink(""), "empty preedit draws nothing");
    assert!(!preedit_has_visible_ink(" "), "a bare space must not draw an underline");
    assert!(!preedit_has_visible_ink("   "), "all-whitespace must not draw");
    assert!(!preedit_has_visible_ink("\t"), "a tab is whitespace");
}

#[test]
fn preedit_overlay_draws_for_real_composition() {
    // Genuine composition always carries non-whitespace ink, so the overlay
    // is never suppressed for real CJK / multi-key input.
    assert!(preedit_has_visible_ink("ni"), "latin composing run draws");
    assert!(preedit_has_visible_ink("\u{4f60}"), "CJK composing run draws");
    assert!(preedit_has_visible_ink("a b"), "ink with embedded space draws");
}

#[test]
fn preedit_caret_advance_zero_for_whitespace_only() {
    // the terminal-cursor caret advance MUST use the same visible-ink
    // gate as the glyph overlay. macOS delivers a whitespace-only marked
    // string during ordinary typing / bare Enter with a CJK source active;
    // advancing the cursor for it shoved the cursor-colored block into empty
    // prompt space with no glyph under it (the stray "yellow line/block").
    assert_eq!(preedit_caret_advance("", 0, 16.0), 0.0, "empty → no advance");
    assert_eq!(preedit_caret_advance(" ", 1, 16.0), 0.0, "bare space → no advance");
    assert_eq!(preedit_caret_advance("   ", 3, 16.0), 0.0, "all-whitespace → no advance");
    assert_eq!(preedit_caret_advance("\t", 1, 16.0), 0.0, "tab → no advance");
}

#[test]
fn preedit_caret_advance_nonzero_for_real_composition() {
    // Real composition still advances the caret to the insertion point.
    assert!(preedit_caret_advance("ni", 2, 16.0) > 0.0, "latin composing run advances the caret");
    assert!(
        preedit_caret_advance("\u{4f60}", 3, 16.0) > 0.0,
        "CJK composing run advances the caret"
    );
    // A caret byte that lands off a char boundary falls back to full width
    // rather than panicking.
    let full = preedit_caret_advance("\u{4f60}", 3, 16.0);
    assert_eq!(
        preedit_caret_advance("\u{4f60}", 1, 16.0),
        full,
        "non-boundary caret byte falls back to full width"
    );
}

// --- tab title colour hover (Issue: custom tab colour did not highlight) ---

/// Distinct sentinel colours so each branch is unambiguous in asserts.
fn active_fg() -> ChromeColor {
    ChromeColor::rgb(0xEB, 0xDB, 0xB2) // gruvbox fg0
}
fn inactive_fg() -> ChromeColor {
    ChromeColor::rgb(0x92, 0x83, 0x74) // gruvbox gray
}

#[test]
fn default_tab_color_brightens_on_hover() {
    // No custom colour: an inactive tab uses inactive_fg, but hovering it
    // (or activating it) swaps to active_fg — the historical default.
    let inactive = tab_title_color(None, false, false, false, active_fg(), inactive_fg());
    assert_eq!(inactive, inactive_fg());

    let hovered = tab_title_color(None, false, true, false, active_fg(), inactive_fg());
    assert_eq!(hovered, active_fg(), "default tab must brighten under the cursor");

    let active = tab_title_color(None, true, false, true, active_fg(), inactive_fg());
    assert_eq!(active, active_fg());
}

#[test]
fn custom_tab_color_brightens_on_hover() {
    // Regression: a user-set custom title colour must light up on hover just
    // like a default tab, instead of staying dimmed to 0.55 alpha.
    let custom = "#83a598";
    let full = hex_to_chrome_color(custom);

    // Inactive, unhovered, unfocused panel → dimmed.
    let dimmed = tab_title_color(Some(custom), false, false, false, active_fg(), inactive_fg());
    assert!(dimmed.a() < 255, "inactive custom tab should be dimmed");
    assert_eq!((dimmed.r(), dimmed.g(), dimmed.b()), (full.r(), full.g(), full.b()));

    // Hovered → full strength (the fix).
    let hovered = tab_title_color(Some(custom), false, true, false, active_fg(), inactive_fg());
    assert_eq!(hovered, full, "hovered custom tab must paint at full alpha");

    // Active tab → full strength regardless of hover.
    let active = tab_title_color(Some(custom), true, false, true, active_fg(), inactive_fg());
    assert_eq!(active, full);

    // Focused panel keeps a custom inactive tab at full strength (unchanged).
    let focused = tab_title_color(Some(custom), false, false, true, active_fg(), inactive_fg());
    assert_eq!(focused, full);
}

#[test]
fn preedit_cache_matches_only_on_identical_inputs_and_atlas_stamp() {
    // Preedit reuse requires identical input and qualified atlas identity to reject recycled UVs.
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: GlyphContentStamp {
            device_generation: 7,
            allocation_generation: 1,
            content_identity: 7,
            growths: 0,
        },
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    // Exact match.
    let epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 1,
        content_identity: 7,
        growths: 0,
    };
    assert!(c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch));
    // Any single field differing must miss.
    assert!(!c.matches("ni'ha", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // text grew
    assert!(!c.matches("ni'hao", 15.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // font size
    assert!(!c.matches("ni'hao", 14.0, 101.0, 50.0, 0xAABBCCFF, epoch)); // x (scroll)
    assert!(!c.matches("ni'hao", 14.0, 100.0, 51.0, 0xAABBCCFF, epoch)); // y
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0x11223344, epoch)); // color
    let evicted_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 1,
        content_identity: 8,
        growths: 0,
    };
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, evicted_epoch));
}

#[test]
fn preedit_cache_rejects_same_content_identity_after_atlas_replacement() {
    // Equal local content identities cannot validate UVs from another allocation.
    let old_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 3,
        content_identity: 0,
        growths: 0,
    };
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: old_epoch,
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    let replacement_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 4,
        content_identity: 0,
        growths: 0,
    };

    assert!(
        !c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, replacement_epoch),
        "equal content identities from different atlas allocations must not reuse cached UVs"
    );
}

#[test]
fn preedit_cache_rejects_reset_with_unchanged_evictions() {
    // The cache stamp independently rejects reset content even with fixed device/allocation qualifiers.
    let mut atlas = GlyphAtlas::new(2, 1);
    let capture = |atlas: &GlyphAtlas| GlyphContentStamp::capture(7, 3, atlas);
    let cache = PreeditGlyphCache {
        text: "preedit".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: capture(&atlas),
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    assert!(cache.matches("preedit", 14.0, 100.0, 50.0, 0xAABBCCFF, capture(&atlas)));
    let evictions = atlas.evictions();
    let identity = atlas.identity();
    atlas.reset_in_place();
    assert_eq!(atlas.evictions(), evictions);
    assert_ne!(atlas.identity(), identity);
    assert!(!cache.matches("preedit", 14.0, 100.0, 50.0, 0xAABBCCFF, capture(&atlas)));
}

#[test]
fn atlas_frame_detector_qualifies_equal_content_by_allocation_and_device() {
    // Equal atlas-local identities cannot authorize UVs belonging to another allocation or device.
    let atlas = GlyphAtlas::new(2, 1);
    let replacement = GlyphAtlas::new(2, 1);
    let device = DeviceErrorState::new();
    let other_device = DeviceErrorState::new();
    let before = GlyphContentStamp::capture(device.generation(), 3, &atlas);
    assert_eq!(atlas.identity(), replacement.identity());
    for after in [
        GlyphContentStamp::capture(device.generation(), 4, &replacement),
        GlyphContentStamp::capture(other_device.generation(), 3, &replacement),
    ] {
        assert_eq!(before.content_identity, after.content_identity);
        assert!(atlas_changed_during_frame(before, after));
        let cache = PreeditGlyphCache {
            text: "preedit".to_string(),
            font_size: 14.0,
            start_x: 0.0,
            top_y: 0.0,
            color_bits: 0xFFFFFFFF,
            atlas_stamp: before,
            glyphs: Vec::new(),
            missing_boxes: Vec::new(),
            missing_chrome_chars: Vec::new(),
        };
        assert!(!cache.matches("preedit", 14.0, 0.0, 0.0, 0xFFFFFFFF, after));
    }
}

#[test]
fn atlas_frame_detector_production_capture_and_retry_precede_presentation() {
    // Frame and preedit checks share qualified identity; diagnostic eviction counts cannot admit a frame.
    let source = include_str!("core.rs");
    let start = source.find("let atlas_stamp_at_frame_start = self.glyph_atlas_stamp();").unwrap();
    // `assemble_frame` hands the start stamp and the current one to the shared pass-end helper,
    // which checks them before any batch reaches the presenter.
    let handoff = source.find("atlas_stamp_now: self.glyph_atlas_stamp(),").unwrap();
    let call = source.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    assert!(start < handoff && handoff < call);
    let guard = source
        .find("if atlas_changed_during_frame(pass.atlas_stamp_at_start, pass.atlas_stamp_now)")
        .unwrap();
    let retry = source[guard..].find("return Err(Assembled::AtlasRetry {").unwrap() + guard;
    let present = source.find("self.present_frame(&layers, &mut gpu_timing)?;").unwrap();
    let acknowledge = source.find("self.finish_successful_frame(").unwrap();
    assert!(start < guard && guard < retry && retry < present && present < acknowledge);
    assert!(source.contains("let atlas_evictions_at_frame_start = self.glyph_atlas.evictions();"));
    let lifecycle = include_str!("atlas_lifecycle.rs");
    let capture = lifecycle.split_once("fn glyph_atlas_stamp(&self)").unwrap().1;
    let capture = capture.split_once("fn mark_glyph_atlas_replaced").unwrap().0;
    assert!(capture.contains("self.device_errors.generation()"));
    assert!(capture.contains("self.glyph_atlas_generation"));
    assert!(capture.contains("&self.glyph_atlas"));
    assert_eq!(source.matches("atlas_stamp: self.glyph_atlas_stamp()").count(), 1);
}

struct OnePixelAtlasGlyph;

impl sonicterm_text::glyph_atlas::Rasterizer for OnePixelAtlasGlyph {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255],
            is_color: false,
            is_subpixel: false,
        })
    }
}

#[cfg(target_os = "windows")]
struct SolidTallAtlasGlyph;

#[cfg(target_os = "windows")]
impl sonicterm_text::glyph_atlas::Rasterizer for SolidTallAtlasGlyph {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 1,
            height: 30,
            offset_x: 0,
            offset_y: 0,
            advance: 1.0,
            coverage: vec![255; 30],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// Padded dirty damage clears real GPU glyph ink outside a compressed cell row.
#[cfg(target_os = "windows")]
#[test]
fn warp_retained_redraw_clears_overhanging_glyph_ink() {
    const WIDTH: u32 = 2;
    const HEIGHT: u32 = 64;
    const BYTES_PER_ROW: u32 = 256;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .expect("Windows WARP fallback adapter");
    let (device, queue) = pollster::block_on(
        adapter.request_device(&device_descriptor_for(true, wgpu::Features::empty())),
    )
    .expect("WARP device");
    let mut pipeline =
        crate::wezterm_pipeline::WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm, 2);
    let mut atlas = GlyphAtlas::new(1, 30);
    let glyph = atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('T', false, false), &mut SolidTallAtlasGlyph)
        .expect("tall glyph inserts");
    let image_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.image_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Image,
    );
    let mut glyph_upload = crate::atlas_upload::AtlasUpload::new(
        &device,
        &atlas,
        pipeline.glyph_bind_group_layout(),
        crate::atlas_upload::AtlasBindingKind::Glyph,
    );
    glyph_upload.sync(&queue, &mut atlas);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("retained overhang test target"),
        size: wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("retained overhang readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let glyphs = [sonicterm_text::GlyphInstance {
        rect: crate::quad::px_to_ndc(0.0, 10.0, 1.0, 30.0, WIDTH as f32, HEIGHT as f32),
        uv: glyph.uv,
        color: [1.0; 4],
        flags: [0.0; 4],
    }];
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("retained overhang initial pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipeline.draw_frame(
            &device,
            &queue,
            &mut pass,
            image_upload.image_bind_group(),
            glyph_upload.glyph_bind_group(),
            WIDTH as f32,
            HEIGHT as f32,
            SubpixelAaMode::Off,
            None,
            &[],
            &[],
            &glyphs,
            &[],
            &[],
        );
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll initial WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped initial WARP readback");
    assert_ne!(&bytes[35 * BYTES_PER_ROW as usize..35 * BYTES_PER_ROW as usize + 3], &[0, 0, 0]);
    drop(bytes);
    readback.unmap();

    let damage = dirty_rows_damage_rect_with_ink_pad(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 10, w: WIDTH, h: 40 },
        0.0,
        10.0,
        1,
        2.0,
        12.0,
        18.0,
        WIDTH,
        HEIGHT,
    )
    .expect("dirty row produces padded damage");
    let clear = crate::quad::QuadInstance::sharp(
        crate::quad::px_to_ndc(
            damage.x as f32,
            damage.y as f32,
            damage.w as f32,
            damage.h as f32,
            WIDTH as f32,
            HEIGHT as f32,
        ),
        [0.0, 0.0, 0.0, 1.0],
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("retained overhang clear pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(damage.x as u32, damage.y as u32, damage.w, damage.h);
        pipeline.draw_frame(
            &device,
            &queue,
            &mut pass,
            image_upload.image_bind_group(),
            glyph_upload.glyph_bind_group(),
            WIDTH as f32,
            HEIGHT as f32,
            SubpixelAaMode::Off,
            Some(clear),
            &[],
            &[],
            &[],
            &[],
            &[],
        );
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(BYTES_PER_ROW),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d { width: WIDTH, height: HEIGHT, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll WARP readback");
    let bytes = slice.get_mapped_range().expect("mapped WARP readback");

    assert_eq!(&bytes[35 * BYTES_PER_ROW as usize..35 * BYTES_PER_ROW as usize + 3], &[0, 0, 0]);
}

struct RetainedPixelFixture {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: WeztermPipeline,
    image_upload: crate::atlas_upload::AtlasUpload,
    glyph_upload: crate::atlas_upload::AtlasUpload,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    dual_source: bool,
}

impl RetainedPixelFixture {
    fn new() -> Self {
        #[cfg(target_os = "windows")]
        let (backends, fallback) = (wgpu::Backends::DX12, true);
        #[cfg(not(target_os = "windows"))]
        let (backends, fallback) = (wgpu::Backends::all(), false);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: fallback,
            apply_limit_buckets: false,
        }))
        .expect("retained-pixel adapter");
        let features = selected_optional_device_features(adapter.features(), cfg!(windows));
        let dual_source = features.contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor_for(
            adapter.get_info().device_type == wgpu::DeviceType::Cpu,
            features,
        )))
        .expect("retained-pixel device");
        let pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 3);
        let atlas = GlyphAtlas::new(1, 1);
        let image_upload = crate::atlas_upload::AtlasUpload::new(
            &device,
            &atlas,
            pipeline.image_bind_group_layout(),
            crate::atlas_upload::AtlasBindingKind::Image,
        );
        let glyph_upload = crate::atlas_upload::AtlasUpload::new(
            &device,
            &atlas,
            pipeline.glyph_bind_group_layout(),
            crate::atlas_upload::AtlasBindingKind::Glyph,
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("retained reset pixel fixture"),
            size: wgpu::Extent3d { width: 4, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        Self { device, queue, pipeline, image_upload, glyph_upload, target, view, dual_source }
    }

    fn draw(
        &mut self,
        first: bool,
        damage: PixelRect,
        background: wgpu::Color,
        mode: SubpixelAaMode,
        quads: &[QuadInstance],
    ) {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        draw_retained_frame(
            &mut self.pipeline,
            &self.device,
            &self.queue,
            &mut encoder,
            &self.view,
            self.image_upload.image_bind_group(),
            self.glyph_upload.glyph_bind_group(),
            4.0,
            2.0,
            first,
            damage,
            background,
            mode,
            quads,
            &[],
            &[],
            &[],
            &[],
        );
        self.queue.submit([encoder.finish()]);
    }

    fn pixels(&self) -> Vec<[u8; 4]> {
        const STRIDE: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retained reset readback"),
            size: u64::from(STRIDE) * 2,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(STRIDE),
                    rows_per_image: Some(2),
                },
            },
            wgpu::Extent3d { width: 4, height: 2, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("retained readback poll");
        let mapped = slice.get_mapped_range().expect("retained mapped pixels");
        let mut pixels = Vec::new();
        for y in 0..2 {
            for x in 0..4 {
                let offset = y * STRIDE as usize + x * 4;
                pixels.push(mapped[offset..offset + 4].try_into().unwrap());
            }
        }
        drop(mapped);
        readback.unmap();
        pixels
    }
}

fn reset_background(opacity: f32) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(0.125 * opacity),
        g: f64::from(0.25 * opacity),
        b: f64::from(0.5 * opacity),
        a: f64::from(opacity),
    }
}

fn clear_bgra(background: wgpu::Color) -> [u8; 4] {
    [
        crate::color::linear_channel_to_srgb_u8(background.b as f32),
        crate::color::linear_channel_to_srgb_u8(background.g as f32),
        crate::color::linear_channel_to_srgb_u8(background.r as f32),
        (background.a * 255.0).round() as u8,
    ]
}

fn pixels_close(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual.into_iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1)
}

/// The first retained frame writes background alpha once at every supported opacity.
#[test]
fn retained_first_frame_does_not_blend_a_second_background_reset() {
    let mut fixture = RetainedPixelFixture::new();
    let mut failures = Vec::new();
    for opacity in [0.0, 0.5, 1.0] {
        let background = reset_background(opacity);
        fixture.draw(true, full_surface_rect(4, 2), background, SubpixelAaMode::Off, &[]);
        let actual = fixture.pixels()[0];
        let expected = clear_bgra(background);
        if !pixels_close(actual, expected) {
            failures.push((opacity, actual, expected));
        }
    }
    assert!(failures.is_empty(), "first-frame background mismatch: {failures:?}");
}

/// Partial resets erase old ink once, preserve peer pixels, and cannot rely on later redraws to decay.
#[test]
fn retained_partial_resets_replace_pixels_and_remain_idempotent() {
    let mut fixture = RetainedPixelFixture::new();
    let damage = PixelRect { x: 0, y: 0, w: 2, h: 2 };
    let ink = [QuadInstance::sharp(px_to_ndc(0.0, 0.0, 2.0, 2.0, 4.0, 2.0), [1.0, 0.0, 0.0, 1.0])];
    let mut modes = vec![SubpixelAaMode::Off];
    if fixture.dual_source {
        modes.extend([SubpixelAaMode::Rgb, SubpixelAaMode::Bgr]);
    }
    let mut failures = Vec::new();
    for mode in modes {
        for opacity in [0.0, 0.5, 1.0] {
            fixture.draw(true, full_surface_rect(4, 2), wgpu::Color::GREEN, mode, &ink);
            let baseline = fixture.pixels();
            assert_eq!(baseline[0], [0, 0, 255, 255], "old ink must be present");
            let background = reset_background(opacity);
            fixture.draw(false, damage, background, mode, &[]);
            let first = fixture.pixels();
            let idle = fixture.pixels();
            fixture.draw(false, damage, background, mode, &[]);
            let repeated = fixture.pixels();
            let expected = clear_bgra(background);
            if !pixels_close(first[0], expected) || !pixels_close(repeated[0], expected) {
                failures.push((mode, opacity, first[0], repeated[0], expected));
            }
            assert_eq!(first[3], baseline[3], "outside damage must remain untouched");
            assert_eq!(repeated[3], baseline[3]);
            assert_eq!(idle, first, "an idle frame submits no correcting draw");
        }
    }
    println!("replacement_reset=EXERCISED dual_source={}", fixture.dual_source);
    assert!(failures.is_empty(), "retained reset mismatch: {failures:?}");
}

/// A later full invalidation clears once while ordinary translucent content still source-overs.
#[test]
fn retained_full_clear_preserves_content_blending_and_replaces_prior_frame() {
    let mut fixture = RetainedPixelFixture::new();
    let full = full_surface_rect(4, 2);
    fixture.draw(true, full, wgpu::Color::RED, SubpixelAaMode::Off, &[]);
    let background = reset_background(0.5);
    let source = [0.0, 0.25, 0.0, 0.5];
    let content = [QuadInstance::sharp(px_to_ndc(0.0, 0.0, 1.0, 1.0, 4.0, 2.0), source)];

    fixture.draw(false, full, background, SubpixelAaMode::Off, &content);

    let pixels = fixture.pixels();
    assert!(pixels_close(pixels[3], clear_bgra(background)), "full clear must discard prior red");
    let expected = wgpu::Color {
        r: background.r * 0.5,
        g: f64::from(source[1]) + background.g * 0.5,
        b: background.b * 0.5,
        a: 0.5 + background.a * 0.5,
    };
    assert!(
        pixels_close(pixels[0], clear_bgra(expected)),
        "content must retain source-over blending"
    );
}

#[test]
fn atlas_reset_during_frame_requires_retry() {
    // Reset can recycle UVs without changing eviction counts, so the assembled frame must retry.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
        .expect("first glyph fills the atlas");
    let frame_epoch = atlas.evictions();
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
    let frame_identity = atlas.identity();
    assert!(!atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));

    atlas.reset_in_place();
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
        .expect("replacement glyph reuses the reset atlas");

    assert_eq!(atlas.evictions(), frame_epoch);
    assert_ne!(atlas.identity(), frame_identity);
    assert!(
        atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)),
        "a frame must retry when reset recycled its glyph coordinates"
    );
}

#[test]
fn atlas_frame_detector_rejects_repeated_eviction_count_after_reset() {
    // A reset followed by matching diagnostic counts must not resurrect an earlier frame's UVs.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    for ch in ['a', 'b'] {
        atlas.get_or_insert(sonicterm_types::GlyphKey::new(ch, false, false), &mut raster).unwrap();
        atlas.tick_frame();
    }
    let frame_evictions = atlas.evictions();
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
    assert_eq!(frame_evictions, 1);
    atlas.reset_in_place();
    for ch in ['c', 'd'] {
        atlas.get_or_insert(sonicterm_types::GlyphKey::new(ch, false, false), &mut raster).unwrap();
        atlas.tick_frame();
    }
    assert_eq!(atlas.evictions(), frame_evictions);
    assert_ne!(atlas.identity(), frame_stamp.content_identity);
    assert!(atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));
}

#[test]
fn atlas_frame_detector_accepts_stable_contents_and_nonrecycling_admission() {
    // Lookup, aging and unused-space admission preserve UVs both before and after a reset.
    let mut atlas = GlyphAtlas::new(2, 1);
    let mut raster = OnePixelAtlasGlyph;
    for reset in [false, true] {
        if reset {
            atlas.reset_in_place();
            assert_ne!(atlas.identity(), atlas.evictions());
        }
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
            .unwrap();
        let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);
        atlas.tick_frame();
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
            .unwrap();
        atlas
            .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
            .unwrap();
        assert!(!atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)));
    }
}

#[test]
fn atlas_eviction_during_frame_requires_retry() {
    // Recycling a real atlas slot must reject previously emitted UVs before presentation.
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
        .expect("first glyph fills the atlas");
    let frame_stamp = GlyphContentStamp::capture(7, 3, &atlas);

    atlas.tick_frame();
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
        .expect("second glyph evicts and reuses the only slot");

    assert!(
        atlas_changed_during_frame(frame_stamp, GlyphContentStamp::capture(7, 3, &atlas)),
        "a frame must not present instances whose UV rectangles may have been recycled"
    );
}

/// Row-cache UVs must miss after reset even when the eviction counter repeats.
#[test]
fn row_cache_uses_nonrepeating_atlas_identity() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut cache = sonicterm_text::row_glyph_cache::RowGlyphCache::new();
    cache.begin_frame(&[(7, 1, 8)]);
    let stale_identity = row_cache_atlas_identity(&atlas);
    let stale_evictions = atlas.evictions();
    assert!(cache.insert(
        7,
        13,
        stale_identity,
        sonicterm_text::row_glyph_cache::CachedRow::default()
    ));

    atlas.reset_in_place();

    assert_eq!(atlas.evictions(), stale_evictions, "the diagnostic counter repeats after reset");
    assert_ne!(row_cache_atlas_identity(&atlas), stale_identity);
    assert!(cache.get(7, 13, row_cache_atlas_identity(&atlas), |_| true).is_none());
}

#[test]
fn same_size_atlas_reset_reuses_gpu_texture() {
    assert!(!atlas_texture_rebuild_required((2048, 2048), (2048, 2048)));
    assert!(atlas_texture_rebuild_required((1024, 1024), (2048, 2048)));
}

#[test]
fn equal_scale_factor_does_not_rebuild_atlas() {
    assert!(!scale_factor_rebuild_required(1.0, 1.0));
    assert!(!scale_factor_rebuild_required(0.1, 0.0));
    assert!(scale_factor_rebuild_required(1.0, 1.25));
}

#[test]
fn software_block_glyph_rect_matches_integer_raster_size() {
    let rect = software_block_glyph_target_rect(21.0, 49.017345, 36.0, 84.03469);

    assert_eq!(rect, (21.0, 49.0, 15.0, 35.0));
}

#[test]
fn software_block_glyph_rows_share_integer_edges() {
    let first = software_block_glyph_target_rect(21.0, 49.017345, 36.0, 84.03469);
    let second = software_block_glyph_target_rect(21.0, 84.03469, 36.0, 119.05203);

    assert_eq!(first.1 + first.3, second.1);
    assert_eq!(first.3, 35.0);
    assert_eq!(second.3, 35.0);
}

#[test]
fn software_block_glyph_rows_distribute_fractional_height_without_seams() {
    let first = software_block_glyph_target_rect(0.0, 0.0, 15.0, 35.6);
    let second = software_block_glyph_target_rect(0.0, 35.6, 15.0, 71.2);

    assert_eq!(first.1 + first.3, second.1);
    assert_eq!((first.3, second.3), (36.0, 35.0));
}

#[test]
fn hardware_block_glyph_geometry_stays_fractional() {
    let cx = 21.0_f32;
    let cy = 49.017345_f32;
    let cell_right = 36.0_f32;
    let cell_h = 35.017345_f32;
    let rect = (cx, cy, cell_right - cx, cell_h);

    assert_eq!(rect, (21.0, 49.017345, 15.0, 35.017345));
}

#[test]
fn status_markers_fit_the_same_single_cell_geometry() {
    // Contract: every Claude Code circle marker uses the same width-bound fit policy.
    let natural = (4.0, 7.0, 24.0, 12.0);
    let cell = (10.0, 20.0, 12.0, 20.0);

    for marker in ['\u{23fa}', '\u{25ef}', '\u{25cf}'] {
        assert_eq!(
            fit_single_cell_status_marker(marker, 1, false, false, natural, cell),
            (10.0, 27.0, 12.0, 6.0)
        );
    }
}

#[test]
fn status_marker_fit_preserves_aspect_ratio_when_height_binds() {
    // Contract: tall marker tiles remain centered and proportional inside one cell.
    let fitted = fit_single_cell_status_marker(
        '\u{23fa}',
        1,
        false,
        false,
        (2.0, 3.0, 8.0, 32.0),
        (10.0, 20.0, 12.0, 16.0),
    );

    assert_eq!(fitted, (14.0, 20.0, 4.0, 16.0));
    assert_eq!(fitted.2 / fitted.3, 8.0 / 32.0);
}

/// Hollow and solid fallback tiles normalize to one outer cell-constrained size.
///
/// Unequal square source tiles model the observed fallback-font mismatch: the hollow circle
/// exceeds the cell while the solid circle is naturally smaller than it.
#[test]
fn status_marker_fit_enlarges_small_tiles_to_match_oversized_tiles() {
    let cell = (10.0, 20.0, 6.0, 8.0);
    let hollow =
        fit_single_cell_status_marker('\u{25ef}', 1, false, false, (8.0, 18.0, 8.0, 8.0), cell);
    let solid =
        fit_single_cell_status_marker('\u{25cf}', 1, false, false, (11.0, 22.0, 4.0, 4.0), cell);

    assert_eq!(hollow, (10.0, 21.0, 6.0, 6.0));
    assert_eq!(solid, hollow);
}

#[test]
fn status_marker_fit_leaves_ineligible_geometry_unchanged() {
    // Contract: the targeted policy cannot alter ordinary text, wide cells, or multi-cell clusters.
    let natural = (-8.0, 3.0, 16.0, 20.0);
    let cell = (0.0, 0.0, 10.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('x', 1, false, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 1, true, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 2, false, false, natural, cell), natural);
    assert_eq!(fit_single_cell_status_marker('\u{23fa}', 1, false, true, natural, cell), natural);
}

#[test]
fn status_marker_fit_leaves_multi_cell_ligature_geometry_unchanged() {
    // Contract: both halves of a two-cell `=>` ligature retain their natural overhang.
    let cell = (0.0, 0.0, 10.0, 20.0);
    let equals_half = (-8.0, 1.0, 16.0, 20.0);
    let arrow_half = (2.0, 1.0, 16.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('=', 2, false, false, equals_half, cell), equals_half);
    assert_eq!(fit_single_cell_status_marker('>', 2, false, false, arrow_half, cell), arrow_half);
}

#[test]
fn status_marker_fit_leaves_degenerate_geometry_unchanged() {
    // Contract: a zero-area glyph or cell cannot produce a meaningful fit ratio.
    let glyph = (1.0, 2.0, 0.0, 12.0);
    let cell = (10.0, 20.0, 12.0, 20.0);

    assert_eq!(fit_single_cell_status_marker('\u{25cf}', 1, false, false, glyph, cell), glyph);
    assert_eq!(
        fit_single_cell_status_marker(
            '\u{25cf}',
            1,
            false,
            false,
            (1.0, 2.0, 8.0, 12.0),
            (10.0, 20.0, 0.0, 20.0)
        ),
        (1.0, 2.0, 8.0, 12.0)
    );
}

/// Fallback and shaped glyphs both carry the marker-fit decision made at shape time from the
/// lead cell, and projection applies the fit to those two kinds only, after the shaping offset
/// and before the device-pixel snap and NDC conversion.
#[test]
fn status_marker_fit_is_wired_before_both_terminal_glyph_emissions() {
    let source: String = include_str!("core.rs").split_whitespace().collect();
    let build = source.find("fnbuild_shape_run(").expect("record builder");
    let builder = &source[build..];
    let decide = builder.find("letmarker_fit=status_marker_fit_eligible(").expect("decision");
    let fallback = builder.find("ifg.glyph_id==0{").expect("fallback branch");
    assert!(decide < fallback, "one decision serves both the fallback and the shaped branch");
    assert_eq!(
        builder
            .matches("set_shaped_bits(&mutrecord,[shape_x_offset,g.y_offset],marker_fit,")
            .count(),
        2
    );
    let project = source.find("pub(crate)fnproject_row_glyph(").expect("projection");
    let body = &source[project..project + source[project..].find("\n}").unwrap_or(4000).min(4000)];
    let arm = body.find("RowGlyphKind::Fallback|RowGlyphKind::Shaped=>{").expect("fit arm");
    let offset = body.find("positioned_shaped_glyph_rect(").expect("shaping offset");
    let fit = body.find("fit_status_marker_rect(positioned,").expect("fit");
    let snap = arm + body[arm..].find("snap_to_device_pixels(fitted,").expect("snap");
    let ndc = body.find("rect:px_to_ndc(").expect("ndc");
    assert!(arm < offset && offset < fit && fit < snap && snap < ndc);
}

/// Glyph flags preserve color selection in x and raw subpixel coverage in y.
#[test]
fn glyph_flags_keep_color_and_subpixel_axes_independent() {
    assert_eq!(glyph_flags(false, false), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(true, false), [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(false, true), [0.0, 1.0, 0.0, 0.0]);
}

/// CPU color composition decodes the original atlas without rewriting its storage.
#[test]
fn windows_software_presenter_keeps_cpu_color_atlas_storage_unchanged() {
    const SOURCE: &str = include_str!("software_frame.rs");

    assert!(SOURCE.contains("let atlas_pixels = atlas.pixels_bgra();"));
    assert!(SOURCE.contains("premultiplied_srgb_bgra_to_linear_rgba"));
    assert!(SOURCE.contains("blend_premul_linear_over_srgb_bgra("));
    assert!(!SOURCE.contains("copy_rect_into_scratch"));
}

/// Atlas roles keep linear-filtered images separate from dual-view nearest glyph sampling.
#[test]
fn atlas_sync_and_bind_groups_are_wired_by_role() {
    // `core.rs` builds the atlas uploads; the wgpu presenter in `present.rs` syncs and binds them.
    let source =
        [include_str!("core.rs"), include_str!("present.rs"), include_str!("atlas_lifecycle.rs")]
            .concat()
            .replace("\r\n", "\n");

    assert!(source.contains("self.image_upload.sync(&self.queue, &mut self.image_atlas)"));
    assert!(source.contains("self.glyph_upload.sync(&self.queue, &mut self.glyph_atlas)"));
    assert!(source.contains("self.image_upload.image_bind_group()"));
    assert!(source.contains("self.glyph_upload.glyph_bind_group()"));
    assert!(source.contains("AtlasBindingKind::Image"));
    assert!(source.contains("AtlasBindingKind::Glyph"));
}

#[test]
fn inline_image_atlas_starts_placeholder_and_promotes_once() {
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
    assert!(!image_atlas_promotion_required(&placeholder, false));
    assert!(image_atlas_promotion_required(&placeholder, true));

    let promoted = GlyphAtlas::default_size();
    assert!(!image_atlas_promotion_required(&promoted, true));
}

#[test]
fn windows_software_presenter_uses_placeholder_gpu_atlases() {
    let atlas = GlyphAtlas::default_size();
    assert_eq!(
        desired_gpu_atlas_dimensions(true, &atlas),
        (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
    );
    assert_eq!(
        desired_gpu_atlas_dimensions(false, &atlas),
        (sonicterm_text::glyph_atlas::ATLAS_DIM, sonicterm_text::glyph_atlas::ATLAS_DIM)
    );
}

#[test]
fn inline_image_atlas_skips_older_images_without_eviction() {
    let older = sonicterm_render_model::InlineImage {
        id: 1,
        row: 0,
        col: 0,
        width: 1,
        height: 1,
        bgra: std::sync::Arc::from(vec![0, 0, 255, 255]),
    };
    let newer = sonicterm_render_model::InlineImage {
        id: 2,
        row: 0,
        col: 0,
        width: 1,
        height: 1,
        bgra: std::sync::Arc::from(vec![0, 255, 0, 255]),
    };
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut instances = Vec::new();
    let placements = [
        InlineImagePlacement {
            image: &older,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: PaneRect::new(0.0, 0.0, 10.0, 10.0),
            painter_order: 0,
        },
        InlineImagePlacement {
            image: &newer,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: PaneRect::new(0.0, 0.0, 10.0, 10.0),
            painter_order: 1,
        },
    ];

    let skipped =
        emit_inline_image_instances(&mut atlas, &mut instances, &placements, 1.0, 1.0, 10.0, 10.0);

    let newer_key = sonicterm_types::GlyphKey {
        ch: '\u{fffc}',
        font_slot: 0xFE,
        weight_bold: false,
        italic: false,
        glyph_id: fold_u64_to_u32(2),
        raster_variant: GlyphRasterVariant::Normal,
    };
    let older_key = sonicterm_types::GlyphKey { glyph_id: fold_u64_to_u32(1), ..newer_key };
    assert_eq!(skipped, 1);
    assert_eq!(atlas.evictions(), 0, "image pressure must never recycle atlas rectangles");
    assert!(atlas.get(newer_key).is_some(), "newest image should win bounded capacity");
    assert!(atlas.get(older_key).is_none(), "older image should be skipped once full");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].sample_uv, instances[0].uv, "uncut images retain their complete tile");
}

fn striped_inline_image(width: u32, height: u32) -> sonicterm_render_model::InlineImage {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[((x * 13) % 256) as u8, ((y * 71) % 256) as u8, 180, 255]);
        }
    }
    sonicterm_render_model::InlineImage {
        id: 71,
        row: 0,
        col: 0,
        width,
        height,
        bgra: std::sync::Arc::from(pixels),
    }
}

/// Image pixels beyond a pane's right edge must not overwrite its peer on either presenter.
#[test]
fn inline_image_clips_512_pixels_to_400_pixel_pane() {
    let image = striped_inline_image(512, 2);
    let mut atlas = GlyphAtlas::new(512, 2);
    let mut instances = Vec::new();
    let placement = InlineImagePlacement {
        image: &image,
        origin_x: 0.0,
        origin_y: 0.0,
        content_clip: PaneRect::new(0.0, 0.0, 400.0, 2.0),
        painter_order: 0,
    };
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 520.0, 2.0),
        0
    );
    assert_eq!(instances.len(), 1);
    let sentinel = [0.0, 1.0, 0.0, 1.0];
    let gpu = crate::atlas_upload::render_image_instances_readback(
        &mut atlas, &instances, 520, 2, sentinel,
    );
    #[cfg(target_os = "windows")]
    let cpu = {
        let mut frame = crate::software_frame::SoftwareFrame::new(520, 2, sentinel).unwrap();
        frame.draw_layers(&atlas, &atlas, &[], &instances, &[], &[], &[]);
        (0..2)
            .flat_map(|y| (0..520).map(move |x| (x, y)))
            .flat_map(|(x, y)| frame.pixel_bgra_at(x, y).unwrap())
            .collect::<Vec<u8>>()
    };
    for x in 0..400 {
        let offset = x * 4;
        assert_eq!(&gpu[offset..offset + 4], &image.bgra[offset..offset + 4]);
    }
    for x in 400..520 {
        let offset = x * 4;
        assert_eq!(&gpu[offset..offset + 4], &[0, 255, 0, 255], "GPU peer pixel {x}");
        #[cfg(target_os = "windows")]
        assert_eq!(&cpu[offset..offset + 4], &[0, 255, 0, 255], "CPU peer pixel {x}");
    }
}

/// Fully pane-clipped or undecoded images must not promote residency, pack tiles, or emit geometry.
#[test]
fn inline_image_visibility_matches_empty_emission_and_residency() {
    let image = striped_inline_image(8, 2);
    let mut atlas = GlyphAtlas::new(16, 4);
    let before = atlas.retained_amount();
    let placement = InlineImagePlacement {
        image: &image,
        origin_x: 20.0,
        origin_y: 0.0,
        content_clip: PaneRect::new(0.0, 0.0, 10.0, 4.0),
        painter_order: 0,
    };
    let mut instances = Vec::new();
    let visible = placement.visible_rect(1.0, 1.0, 40.0, 4.0).is_some();
    assert!(!visible, "on-surface pixels outside their pane are not renderable media");
    assert!(!image_atlas_promotion_required(&GlyphAtlas::new(1, 1), visible));
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 40.0, 4.0),
        0
    );
    assert!(instances.is_empty());
    assert!(atlas.is_empty());
    assert_eq!(atlas.retained_amount(), before);
    let empty =
        sonicterm_render_model::InlineImage { bgra: std::sync::Arc::from([]), ..image.clone() };
    let undecoded = InlineImagePlacement { image: &empty, origin_x: 0.0, ..placement };
    assert!(undecoded.visible_rect(1.0, 1.0, 40.0, 4.0).is_none());
}

/// Each pane and surface edge clips destination geometry without changing the source-image scale.
#[test]
fn inline_image_visible_rect_intersects_all_edges() {
    let image = striped_inline_image(10, 8);
    let cases = [
        (PaneRect::new(3.25, 2.0, 8.0, 8.0), PaneRect::new(3.25, 2.0, 6.75, 6.0)),
        (PaneRect::new(0.0, 0.0, 7.5, 6.25), PaneRect::new(0.0, 0.0, 7.5, 6.25)),
        (PaneRect::new(-3.0, -2.0, 20.0, 20.0), PaneRect::new(0.0, 0.0, 10.0, 8.0)),
        (PaneRect::new(1.25, 0.75, 6.5, 4.5), PaneRect::new(1.25, 0.75, 6.5, 4.5)),
    ];
    for (content_clip, expected) in cases {
        let placement = InlineImagePlacement {
            image: &image,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip,
            painter_order: 0,
        };
        assert_eq!(placement.visible_rect(1.0, 1.0, 20.0, 20.0), Some(expected));
    }
    let surface = InlineImagePlacement {
        image: &image,
        origin_x: -2.25,
        origin_y: -1.5,
        content_clip: PaneRect::new(-4.0, -3.0, 30.0, 30.0),
        painter_order: 0,
    };
    assert_eq!(surface.visible_rect(1.0, 1.0, 6.0, 5.0), Some(PaneRect::new(0.0, 0.0, 6.0, 5.0)));
}

fn packed_image_fixture(
    image: &sonicterm_render_model::InlineImage,
    origin: [f32; 2],
    content_clip: PaneRect,
) -> (GlyphAtlas, Vec<ImageInstance>) {
    let mut atlas = GlyphAtlas::new(image.width + 2, image.height + 2);
    let mut pack_neighbor = |ch, width, height| {
        atlas
            .get_or_insert_lazy_without_eviction(
                sonicterm_types::GlyphKey::new(ch, false, false),
                width,
                height,
                || sonicterm_text::glyph_atlas::RasterTile {
                    width,
                    height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage: [0, 0, 255, 255].repeat((width * height) as usize),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .unwrap();
    };
    pack_neighbor('T', image.width + 2, 1);
    pack_neighbor('L', 1, image.height);
    let placement = InlineImagePlacement {
        image,
        origin_x: origin[0],
        origin_y: origin[1],
        content_clip,
        painter_order: 0,
    };
    let mut instances = Vec::new();
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &[placement], 1.0, 1.0, 16.0, 12.0),
        0
    );
    for (ch, width, height) in [('R', 1, image.height), ('B', image.width + 2, 1)] {
        atlas
            .get_or_insert_lazy_without_eviction(
                sonicterm_types::GlyphKey::new(ch, false, false),
                width,
                height,
                || sonicterm_text::glyph_atlas::RasterTile {
                    width,
                    height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: width as f32,
                    coverage: [0, 0, 255, 255].repeat((width * height) as usize),
                    is_color: true,
                    is_subpixel: false,
                },
            )
            .unwrap();
    }
    (atlas, instances)
}

/// Fractional pane cuts keep the unclipped image's interpolation and cannot redefine packed-tile edges.
#[test]
fn inline_image_fractional_clips_preserve_original_tile_sampling() {
    let image = striped_inline_image(8, 6);
    let surface = PaneRect::new(0.0, 0.0, 16.0, 12.0);
    let cases = [
        ([2.25, 1.75], PaneRect::new(4.375, 0.0, 11.625, 12.0)),
        ([2.25, 1.75], PaneRect::new(0.0, 0.0, 7.625, 12.0)),
        ([2.25, 1.75], PaneRect::new(0.0, 3.375, 16.0, 8.625)),
        ([2.25, 1.75], PaneRect::new(0.0, 0.0, 16.0, 5.625)),
        ([2.25, 1.75], PaneRect::new(4.375, 3.375, 3.25, 2.25)),
        ([-2.25, -1.75], PaneRect::new(0.375, 0.375, 4.25, 3.25)),
    ];
    let sentinel = [0.0, 1.0, 0.0, 1.0];
    for (origin, clip) in cases {
        let (mut full_atlas, full_instances) = packed_image_fixture(&image, origin, surface);
        let baseline = crate::atlas_upload::render_image_instances_readback(
            &mut full_atlas,
            &full_instances,
            16,
            12,
            sentinel,
        );
        let (mut atlas, instances) = packed_image_fixture(&image, origin, clip);
        assert_eq!(instances.len(), 1);
        let instance = instances[0];
        assert_ne!(instance.uv, instance.sample_uv);
        assert!(instance.sample_uv[0] > 0.0 && instance.sample_uv[1] > 0.0);
        assert!(instance.sample_uv[2] < 1.0 && instance.sample_uv[3] < 1.0);
        assert_eq!(instance.sample_uv, full_instances[0].sample_uv);
        let gpu = crate::atlas_upload::render_image_instances_readback(
            &mut atlas, &instances, 16, 12, sentinel,
        );
        #[cfg(target_os = "windows")]
        let cpu = {
            let mut frame = crate::software_frame::SoftwareFrame::new(16, 12, sentinel).unwrap();
            frame.draw_layers(&atlas, &atlas, &[], &instances, &[], &[], &[]);
            (0..12)
                .flat_map(|y| (0..16).map(move |x| (x, y)))
                .flat_map(|(x, y)| frame.pixel_bgra_at(x, y).unwrap())
                .collect::<Vec<u8>>()
        };
        let [left, top, width, height] = instance.rect_px;
        let mut visible_samples = 0;
        for y in 0..12 {
            for x in 0..16 {
                let offset = (y * 16 + x) * 4;
                let point = [x as f32 + 0.5, y as f32 + 0.5];
                let visible = point[0] >= left
                    && point[0] < left + width
                    && point[1] >= top
                    && point[1] < top + height;
                let expected: [u8; 4] = if visible {
                    visible_samples += 1;
                    baseline[offset..offset + 4].try_into().unwrap()
                } else {
                    [0, 255, 0, 255]
                };
                let actual = gpu[offset..offset + 4].try_into().unwrap();
                assert!(
                    pixels_close(actual, expected),
                    "clip={clip:?} pixel=({x},{y}) GPU={actual:?} expected={expected:?}"
                );
                #[cfg(target_os = "windows")]
                {
                    let actual = cpu[offset..offset + 4].try_into().unwrap();
                    assert!(
                        pixels_close(actual, expected),
                        "clip={clip:?} pixel=({x},{y}) CPU={actual:?} expected={expected:?}"
                    );
                }
            }
        }
        assert!(visible_samples > 0);
    }
}

/// Newest-first atlas packing must not change overlapping images' original painter order.
#[test]
fn inline_image_clipping_preserves_painter_order() {
    let older = striped_inline_image(4, 2);
    let newer = sonicterm_render_model::InlineImage { id: older.id + 1, ..older.clone() };
    let clip = PaneRect::new(1.25, 0.0, 2.5, 2.0);
    let placements = [
        InlineImagePlacement {
            image: &older,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: clip,
            painter_order: 0,
        },
        InlineImagePlacement {
            image: &newer,
            origin_x: 0.0,
            origin_y: 0.0,
            content_clip: clip,
            painter_order: 1,
        },
    ];
    let mut atlas = GlyphAtlas::new(8, 2);
    let mut instances = Vec::new();
    assert_eq!(
        emit_inline_image_instances(&mut atlas, &mut instances, &placements, 1.0, 1.0, 8.0, 2.0),
        0
    );
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].rect_px, instances[1].rect_px);
    assert!(
        instances[0].sample_uv[0] > instances[1].sample_uv[0],
        "newer tile packs first but paints last"
    );
}

#[test]
fn cursor_color_uses_theme_cursor_accent() {
    let theme = Theme::default();
    assert_eq!(
        cursor_color_from_theme(&theme),
        hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0)
    );
    assert_eq!(theme.colors.cursor, theme.colors.tab.active_fg);
}

#[test]
fn cursor_text_color_uses_theme_cursor_text() {
    let theme = Theme::default();
    assert_eq!(
        cursor_text_color_from_theme(&theme),
        hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
    );
    assert_ne!(
        cursor_text_color_from_theme(&theme),
        cursor_color_from_theme(&theme),
        "cursor text must not reuse the colored cursor accent"
    );
}

#[test]
fn cursor_color_preserves_theme_channels() {
    // Cursor shape and blink policy do not modify the configured theme channels.
    for color in [[1.0, 0.5, 0.0, 1.0], [0.0, 0.25, 0.5, 0.5]] {
        assert_eq!(active_cursor_color(color), color);
    }
}

#[test]
fn indexed_color_supports_full_xterm_256_palette() {
    let theme = Theme::default();
    assert_eq!(indexed(16, &theme), Some(ChromeColor::rgb(0, 0, 0)));
    assert_eq!(indexed(231, &theme), Some(ChromeColor::rgb(255, 255, 255)));
    assert_eq!(indexed(232, &theme), Some(ChromeColor::rgb(8, 8, 8)));
    assert_eq!(indexed(255, &theme), Some(ChromeColor::rgb(238, 238, 238)));
}

#[test]
fn dirty_rows_damage_rect_unions_and_clips_rows() {
    let damage = dirty_rows_damage_rect(
        [1usize, 3usize],
        sonicterm_render_model::geometry::PixelRect { x: 8, y: 10, w: 100, h: 50 },
        8.0,
        10.0,
        10,
        6.0,
        12.0,
        80,
        80,
    );

    // Rows 1 and 3 union vertically into [22, 58); horizontally the strip
    // spans the pane's clipped bounds [8, 80) rather than the 60px the cell
    // grid occupies. The extra 20px is padding band, which carries glyph ink
    // from negative left side bearings and so is repainted with the row.
    assert_eq!(
        damage,
        Some(sonicterm_render_model::geometry::PixelRect { x: 8, y: 22, w: 72, h: 36 })
    );
}

#[test]
fn dirty_rows_damage_rect_returns_none_for_no_dirty_rows() {
    let damage = dirty_rows_damage_rect(
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 100, h: 50 },
        0.0,
        0.0,
        10,
        6.0,
        12.0,
        100,
        50,
    );

    assert_eq!(damage, None);
}

#[test]
fn dirty_rows_damage_rect_closes_fractional_cell_seam() {
    // at fractional DPI the cell height is non-integer. A single dirty
    // row's damage strip must extend down to the CEILED top of the next row,
    // otherwise the boundary pixel a full-cell inverse block (zsh's
    // reverse-video PROMPT_EOL_MARK `%`) painted into is never repainted when
    // the cell is later cleared — leaving a 1px underline-like remnant.
    //
    // cell_h = 20.4, origin_y = 0:
    //   row 2 top  = floor(2*20.4)=floor(40.8)=40
    //   row 3 top  = ceil(3*20.4)=ceil(61.2)=62
    //   strip      = [40, 62) → height 22
    // The pre-fix height was ceil(20.4)=21 → [40,61), missing pixel 61, which
    // is the start of where row 3 (= the next cell row) begins on screen.
    let damage = dirty_rows_damage_rect(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(damage.y, 40, "row top floored");
    assert_eq!(
        damage.y + damage.h as i32,
        62,
        "strip must reach the ceiled top of the next row (no rounding seam)"
    );

    // Two adjacent dirty rows must tile seamlessly: row N's bottom edge equals
    // row N+1's top edge with no gap and no missing boundary pixel.
    let r2 = dirty_rows_damage_rect(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .unwrap();
    let r3 = dirty_rows_damage_rect(
        [3usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .unwrap();
    assert!(
        r2.y + r2.h as i32 >= r3.y,
        "row 2 strip must reach row 3 top: {} vs {}",
        r2.y + r2.h as i32,
        r3.y
    );
}

/// Retained damage reserves one native line above and below every changed row.
#[test]
fn terminal_ink_pad_uses_native_font_height() {
    let metrics = sonicterm_engine::CellMetricsPx {
        cell_w: 12.0,
        cell_h: 24.0,
        underline_h: 1.0,
        descender: -5.0,
    };

    assert_eq!(terminal_vertical_ink_pad(31.2, Some(metrics)), 24.0);
    assert_eq!(terminal_vertical_ink_pad(24.0, Some(metrics)), 24.0);
    assert_eq!(terminal_vertical_ink_pad(12.0, Some(metrics)), 24.0);
}

/// Missing font metrics conservatively use the configured row height.
#[test]
fn terminal_ink_pad_falls_back_to_row_height() {
    assert_eq!(terminal_vertical_ink_pad(12.2, None), 13.0);
}

/// Font ink outside a compressed row expands retained GPU damage in both directions.
#[test]
fn dirty_rows_damage_rect_covers_vertical_glyph_overhang() {
    let damage = dirty_rows_damage_rect_with_ink_pad(
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        12.0,
        12.0,
        600,
        800,
    )
    .expect("one dirty row yields padded damage");

    assert_eq!(damage.y, 12, "12 px of preceding-row overhang is repainted");
    assert_eq!(damage.y + damage.h as i32, 48, "following-row overhang is repainted too");
}

/// Ink padding remains clipped to the pane rather than repainting peer panes.
#[test]
fn dirty_rows_damage_rect_clips_vertical_overhang_to_pane() {
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 30, w: 600, h: 24 };
    let damage = dirty_rows_damage_rect_with_ink_pad(
        [0usize],
        pane,
        0.0,
        30.0,
        80,
        10.0,
        12.0,
        20.0,
        600,
        800,
    )
    .expect("padded row intersects its pane");

    assert_eq!(damage, pane);
}

/// Alternate screens already repaint the pane and ignore row-level ink padding.
#[test]
fn pane_damage_rect_alt_ignores_redundant_ink_padding() {
    let pane = sonicterm_render_model::geometry::PixelRect { x: 10, y: 20, w: 200, h: 300 };
    let damage = pane_damage_rect_with_ink_pad(
        true,
        [3usize],
        pane,
        10.0,
        20.0,
        80,
        10.0,
        12.0,
        40.0,
        800,
        600,
    );

    assert_eq!(damage, Some(pane));
}

#[test]
fn dirty_rows_damage_rect_closes_fractional_origin_seam_horizontally() {
    // The horizontal twin of the vertical seam above. A pane whose left edge
    // is not on a whole pixel — a split boundary, or padding at fractional
    // DPI — has its damage rect floored leftward. If the width is derived
    // from the cell count alone it does not get that fraction back, so the
    // strip ends short of the true right edge and the last column is never
    // repainted. A glyph that painted there survives after its cell is
    // cleared, appearing as a stray mark on an otherwise empty row.
    //
    // origin_x = 24.6, cell_w = 10.4, cols = 80:
    //   left        = floor(24.6)               = 24
    //   true right  = 24.6 + 80*10.4 = 856.6
    //   ceiled      = 857
    //   width       = 857 - 24                  = 833
    // Deriving width as ceil(80*10.4) = 832 gives right = 856, one pixel
    // short of the 856.6 the pane actually covers.
    //
    // The pane rect here starts AT the content origin and ends at its right
    // edge, so the strip is not also being widened to a padding band — this
    // test is about the rounding alone.
    let damage = dirty_rows_damage_rect(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 24, y: 0, w: 833, h: 800 },
        24.6,
        0.0,
        80,
        10.4,
        20.0,
        1200,
        800,
    )
    .expect("one dirty row yields damage");

    assert_eq!(damage.x, 24, "left edge floored");
    assert_eq!(
        damage.x + damage.w as i32,
        857,
        "strip must reach the ceiled true right edge, not floor(origin) + ceil(cols * cell_w)"
    );

    // A whole-pixel origin must be unaffected, or the fix has moved the
    // common case to buy the fractional one.
    let aligned = dirty_rows_damage_rect(
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 24, y: 0, w: 832, h: 800 },
        24.0,
        0.0,
        80,
        10.4,
        20.0,
        1200,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(aligned.x, 24, "aligned left edge unchanged");
    assert_eq!(
        aligned.x + aligned.w as i32,
        856,
        "aligned origin still covers exactly ceil(24 + 80 * 10.4)"
    );
}

// --- pane_damage_rect: alt-screen whole-pane vs normal narrow damage ------

#[test]
fn pane_damage_rect_alt_clean_pane_has_no_damage() {
    // An alt-screen pane with zero dirty rows contributes no damage
    // (nothing changed -> no repaint), the same as a clean normal pane.
    let d = pane_damage_rect(
        true,
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 },
        0.0,
        0.0,
        80,
        10.0,
        12.0,
        800,
        600,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_alt_dirty_pane_repaints_whole_clipped_rect() {
    // A single dirty row on the alt screen must expand to the ENTIRE pane
    // rectangle (not a narrow row strip): the app may have scrolled content
    // out from under rows it did not re-emit this frame.
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 };
    let d = pane_damage_rect(true, [5usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600);
    assert_eq!(d, Some(pane), "one dirty alt row must repaint the full pane");
}

#[test]
fn pane_damage_rect_covers_the_padding_band_around_the_content() {
    // The cell grid starts at the CONTENT origin, inset from the pane by the
    // configured padding, but glyph ink is not confined to it: a negative
    // left side bearing at column 0 paints left of its cell, into the
    // padding band. A damage rect that stops at the content edge never
    // repaints those columns, so such a pixel survives every later frame —
    // including a full alt-screen pane repaint — until something forces a
    // whole-surface redraw.
    //
    // Both paths must therefore span the pane's own bounds, with the cell
    // geometry still measured from the content origin.
    //
    // pane at x=0 w=848, content origin x=24 (padding_left 12 logical at 2x),
    // 80 cols of 10.0 => content spans [24, 824), pane spans [0, 848).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 848, h: 640 };

    let normal = pane_damage_rect(false, [0usize], pane, 24.0, 16.0, 80, 10.0, 20.0, 1200, 800)
        .expect("a dirty normal pane damages its row");
    assert_eq!(normal.x, 0, "the row strip must reach the pane's left edge, not the content edge");
    assert_eq!(
        normal.x + normal.w as i32,
        848,
        "and its right edge, so ink in either padding band is repainted"
    );

    let alt = pane_damage_rect(true, [0usize], pane, 24.0, 16.0, 80, 10.0, 20.0, 1200, 800)
        .expect("a dirty alt pane damages its rect");
    assert_eq!(alt, pane, "an alt repaint covers the whole pane including padding");

    // Widening must not lose the row's vertical precision: a normal-screen
    // repaint still covers one row, not the whole pane. Otherwise this would
    // trade a stale-pixel bug for repainting everything on every keystroke.
    assert!(
        normal.h < pane.h,
        "the normal path must stay row-limited vertically: got h={} against a {}-tall pane",
        normal.h,
        pane.h
    );
}

#[test]
fn pane_damage_rect_alt_dirty_pane_is_clipped_to_surface() {
    // A dirty alt pane larger than the surface repaints only its on-screen
    // intersection — a complete pane repaint, never a full-window repaint
    // beyond the surface bounds.
    //   pane   = {100,100,800,800}, right/bottom = 900/900
    //   bounds = {0,0,400,300}
    //   clip   = {100,100, 400-100=300, 300-100=200}
    let d = pane_damage_rect(
        true,
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 100, y: 100, w: 800, h: 800 },
        100.0,
        100.0,
        80,
        10.0,
        12.0,
        400,
        300,
    );
    assert_eq!(
        d,
        Some(sonicterm_render_model::geometry::PixelRect { x: 100, y: 100, w: 300, h: 200 })
    );
}

#[test]
fn pane_damage_rect_alt_offscreen_pane_has_no_damage() {
    // A dirty alt pane wholly off the surface intersects nothing -> None.
    let d = pane_damage_rect(
        true,
        [0usize],
        sonicterm_render_model::geometry::PixelRect { x: 500, y: 500, w: 100, h: 100 },
        500.0,
        500.0,
        80,
        10.0,
        12.0,
        400,
        400,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_alt_sparse_rows_still_repaint_whole_pane() {
    // Scattered dirty rows [2, 37]: the alt pane repaints in full, while the
    // same input on a normal pane stays narrow (union of two thin strips).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 200, h: 480 };
    let alt = pane_damage_rect(true, [2usize, 37usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600);
    assert_eq!(alt, Some(pane), "alt sparse rows -> whole pane");

    let normal =
        pane_damage_rect(false, [2usize, 37usize], pane, 0.0, 0.0, 80, 10.0, 12.0, 800, 600)
            .expect("dirty normal pane has damage");
    assert!(
        normal.h < pane.h,
        "normal sparse damage must stay narrow, got {normal:?} vs pane {pane:?}"
    );
}

#[test]
fn pane_damage_rect_normal_matches_narrow_helper() {
    // A normal-screen pane delegates to the narrow dirty-row helper and
    // produces exactly its rect for the same inputs (behavior preserved).
    let pane = sonicterm_render_model::geometry::PixelRect { x: 8, y: 10, w: 100, h: 50 };
    let via_wrapper =
        pane_damage_rect(false, [1usize, 3usize], pane, 8.0, 10.0, 10, 6.0, 12.0, 80, 80);
    let direct = dirty_rows_damage_rect([1usize, 3usize], pane, 8.0, 10.0, 10, 6.0, 12.0, 80, 80);
    assert_eq!(via_wrapper, direct);
    // Width spans the pane's clipped bounds — [8, 80) after the 80px surface
    // clips the 100px pane — rather than the 60px the cell grid occupies.
    // The 20px difference is padding band, which holds glyph ink from
    // negative left side bearings and so has to be repainted with the row.
    assert_eq!(
        via_wrapper,
        Some(sonicterm_render_model::geometry::PixelRect { x: 8, y: 22, w: 72, h: 36 })
    );
}

#[test]
fn pane_damage_rect_normal_empty_input_is_none() {
    // No dirty rows on a normal pane -> no damage.
    let d = pane_damage_rect(
        false,
        [],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 100, h: 50 },
        0.0,
        0.0,
        10,
        6.0,
        12.0,
        100,
        50,
    );
    assert_eq!(d, None);
}

#[test]
fn pane_damage_rect_normal_closes_fractional_cell_seam() {
    // Fractional DPI: a normal pane still routes through the seam-closing
    // narrow logic. Row 2 at cell_h 20.4 spans [40, 62).
    let d = pane_damage_rect(
        false,
        [2usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("one dirty row yields damage");
    assert_eq!(d.y, 40, "row top floored");
    assert_eq!(d.y + d.h as i32, 62, "strip reaches the ceiled next-row top");
}

#[test]
fn pane_damage_rect_normal_sparse_rows_clip_offscreen_row() {
    // Scattered rows where the far row is off the surface: only the
    // on-surface row contributes, clipped into the pane.
    //   row 0   -> [floor(0), ceil(20.4)) = [0, 21)
    //   row 100 -> top floor(2040) is past the 800px surface -> clipped away
    let d = pane_damage_rect(
        false,
        [0usize, 100usize],
        sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 800 },
        0.0,
        0.0,
        80,
        10.0,
        20.4,
        600,
        800,
    )
    .expect("row 0 is on-surface");
    assert_eq!(d, sonicterm_render_model::geometry::PixelRect { x: 0, y: 0, w: 600, h: 21 });
}

/// Effective scrollbar opacity follows the same visibility floor as quad emission.
#[test]
fn effective_scrollbar_buckets_match_visible_output() {
    use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;
    use sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR;

    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Never, 10, 24, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 0, 24, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 0, 1.0), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, ALPHA_EMIT_FLOOR), 0);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, 0.5), 32_768);
    assert_eq!(effective_scrollbar_bucket(ScrollbarMode::Auto, 10, 24, 2.0), u16::MAX);
}

/// Pane order cannot perturb the deterministic scrollbar frame identity.
#[test]
fn scrollbar_identity_sorts_panes_by_id() {
    use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;

    let forward =
        pane_scrollbar_identity(ScrollbarMode::Auto, [(1, 4, 24, 0.25), (2, 8, 24, 0.75)]);
    let reversed =
        pane_scrollbar_identity(ScrollbarMode::Auto, [(2, 8, 24, 0.75), (1, 4, 24, 0.25)]);

    assert_eq!(forward, reversed);
}

/// A visible opacity change must make two otherwise identical frame keys unequal.
#[test]
fn scrollbar_bucket_change_invalidates_the_frame_key() {
    let baseline = revision_plan(7, 1).key;
    let mut visible = baseline.clone();
    visible.panes[0].scrollbar_bucket = u16::MAX;

    assert_ne!(baseline, visible);
}

/// Changing effective LCD policy invalidates the frame without requiring atlas identity changes.
#[test]
fn subpixel_aa_change_invalidates_the_frame_key() {
    let grayscale = revision_plan(7, 1).key;
    let mut lcd = grayscale.clone();
    lcd.window.subpixel_aa = SubpixelAaMode::Rgb;

    assert_ne!(grayscale, lcd);
}

/// Live LCD policy changes invalidate presentation without rebuilding font or atlas state.
#[test]
fn subpixel_aa_setter_does_not_rebuild_fonts_or_atlases() {
    const SOURCE: &str = include_str!("core.rs");
    let start = SOURCE.find("pub fn set_subpixel_aa_mode").expect("LCD setter");
    let body = &SOURCE[start..SOURCE[start..].find("\n    /// Requested LCD").unwrap() + start];

    assert!(body.contains("self.last_frame_key = None"));
    assert!(body.contains("self.request_window_redraw()"));
    for forbidden in ["set_font(", "reset_glyph_atlas", "rebuild_glyph_upload"] {
        assert!(!body.contains(forbidden), "LCD setter unexpectedly calls {forbidden}");
    }
}

/// Changing process privilege must invalidate otherwise-identical retained tab chrome.
#[test]
fn process_privilege_change_invalidates_the_frame_key() {
    let ordinary = revision_plan(7, 1).key;
    let mut privileged = ordinary.clone();
    privileged.window.process_privileged = true;

    assert_ne!(ordinary, privileged);
}

/// Process elevation marks every tab, while foreground elevation marks only its owning tab.
#[test]
fn privilege_badge_combines_process_and_per_tab_foreground_state() {
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 regular"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#2 gsudo"));
    tabs.set_foreground_privileged(1, true);
    let now = Instant::now();
    let marked = |process_privileged: bool| -> Vec<bool> {
        tabs.tabs()
            .iter()
            .map(|tab| TabContent::of(tab, now, false, process_privileged).privileged)
            .collect()
    };

    assert_eq!(marked(false), [false, true]);
    assert_eq!(marked(true), [true, true]);
}

/// A foreground elevation-only change participates in the tab hash and invalidates the frame.
#[test]
fn foreground_privilege_change_invalidates_tab_chrome() {
    let mut ordinary = TabBar::new();
    ordinary.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 shell"));
    let mut privileged = TabBar::new();
    privileged.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 shell"));
    privileged.set_active_foreground_privileged(true);

    assert_ne!(tab_bar_hash(&ordinary, Instant::now()), tab_bar_hash(&privileged, Instant::now()));
}

/// Privileged title layout reserves a bounded lock slot while ordinary titles keep the whole rect.
#[test]
fn privilege_badge_reserves_title_width_without_leaving_the_title_rect() {
    let rect = TabTitleRect { x: 20.0, y: 4.0, w: 180.0, h: 36.0 };
    let reserve = privilege_marker_reserve_px(true, 1.0);
    let ordinary_placement = tab_title_block_placement(rect, 92.0, false, 1.0);
    let placement = tab_title_block_placement(rect, 92.0, true, 1.0);
    let badge = placement.badge_rect.expect("privileged title has a badge");

    assert_eq!(privilege_marker_reserve_px(false, 1.0), 0.0);
    assert_eq!(reserve, PRIVILEGE_BADGE_SIZE_PX + PRIVILEGE_BADGE_GAP_PX);
    assert_eq!(privilege_marker_reserve_px(true, 2.0), reserve * 2.0);
    assert_eq!(ordinary_placement.badge_rect, None);
    assert_eq!(ordinary_placement.text_x, 64.0);
    assert_eq!(ordinary_placement.text_clip, rect);
    assert!(badge.x >= rect.x && badge.y >= rect.y);
    assert!(badge.x + badge.w <= rect.x + rect.w);
    assert!(badge.y + badge.h <= rect.y + rect.h);
    assert!(placement.text_x >= badge.x + badge.w);
    assert!(placement.text_clip.x >= badge.x + badge.w);
    assert!(placement.text_clip.x + placement.text_clip.w <= rect.x + rect.w);
    // A title as wide as the rect less the reserve fits beside the marker unclipped.
    let snug = tab_title_block_placement(rect, rect.w - reserve, true, 1.0);
    assert!(snug.text_clip.w >= rect.w - reserve - 0.001);
}

/// Hold the shared font fixture even after a failed sibling test poisoned it, so one failure
/// cannot fail every later test that shapes text.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The width a title draws at in the tab font, as the tab draw loop lays it out.
fn drawn_tab_title_px(stack: &sonicterm_engine::FontStack, title: &str, size_px: f32) -> f32 {
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
fn ten_px_per_char(content: &TabContent<'_>) -> Option<f32> {
    Some(content.display_text().chars().count() as f32 * 10.0)
}

/// A tab's stored width is the marker reserve plus the drawn width of its badge and title, so
/// CJK and emoji titles are measured by their drawn advance rather than by scalar count.
#[test]
fn tab_content_width_is_the_marker_reserve_plus_the_drawn_badge_and_title() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut tabs = TabBar::new();
    for title in ["#1 zsh", "#2 任务完成", "#3 \u{1f469}\u{200d}\u{1f4bb} ship it"] {
        tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new(title));
    }
    let now = Instant::now();
    for tab in tabs.tabs() {
        for privileged in [false, true] {
            let content = TabContent::of(tab, now, false, privileged);
            let drawn = drawn_tab_title_px(&stack, &content.display_text(), 15.0);
            let measured = tab_content_width_px(Some(&stack), &content, 15.0, 1.0)
                .expect("the tracked font shapes every title");
            let reserve = privilege_marker_reserve_px(privileged, 1.0);
            assert!(
                (measured - reserve - drawn).abs() < 0.01,
                "{:?}: measured {measured}, drawn {drawn}, reserve {reserve}",
                content.title
            );
        }
    }

    let until = now + std::time::Duration::from_secs(5);
    tabs.set_command_status(
        0,
        sonicterm_render_model::boundary::ui::tabs::CommandStatus::Done { exit: Some(1), until },
    );
    let failed = TabContent::of(&tabs.tabs()[0], now, false, false);
    assert_eq!(failed.display_text(), "✗ #1 zsh");
    let measured = tab_content_width_px(Some(&stack), &failed, 15.0, 1.0).expect("shaped");
    assert!((measured - drawn_tab_title_px(&stack, "✗ #1 zsh", 15.0)).abs() < 0.01);
    let expired = TabContent::of(&tabs.tabs()[0], until, false, false);
    assert_eq!(expired.display_text(), "#1 zsh");
    let no_font = TabContent { privileged: true, ..expired };
    assert_eq!(
        tab_content_width_px(None, &no_font, 15.0, 1.0),
        Some(privilege_marker_reserve_px(true, 1.0))
    );
}

/// A title that fits its stored width is drawn whole; a longer one is cut at a grapheme
/// boundary with `…`, and the drawn text stays inside its tab for ASCII, CJK and emoji titles.
#[test]
fn tab_titles_fit_their_stored_width_whole_or_cut_at_a_grapheme_boundary() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let coder = "\u{1f469}\u{200d}\u{1f4bb}";
    let titles = [
        "#1 zsh".to_string(),
        "#2 ~/work/fun-code/sonicterm/crates/sonicterm-app/src".to_string(),
        format!("#3 {}", "任务完成".repeat(4)),
        format!("#4 {}", coder.repeat(24)),
    ];
    let mut tabs = TabBar::new();
    for title in &titles {
        tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new(title.as_str()));
    }
    let now = Instant::now();
    tabs.refresh_content_widths(now, false, 1, false, |content| {
        tab_content_width_px(Some(&stack), content, 15.0, 1.0)
    });
    let layout = TabBarLayout::compute_with_height(&tabs, 1600.0, 40.0);

    assert_eq!(layout.tabs.len(), titles.len());
    let mut cut = Vec::new();
    for widget in &layout.tabs {
        let tab = &tabs.tabs()[widget.idx];
        let content = TabContent::of(tab, now, layout.active == Some(widget.idx), false);
        let display = content.display_text();
        let fitted = crate::chrome_cache::fit_title_run(&stack, &display, 15.0, widget.title_rect.w);
        assert!(fitted.complete, "the tracked font shapes every title");
        let drawn = drawn_tab_title_px(&stack, &fitted.text, 15.0);
        assert!(
            drawn <= widget.title_rect.w + TITLE_FIT_TOLERANCE_PX,
            "{:?} draws {drawn} px in a {} px title rect",
            fitted.text,
            widget.title_rect.w
        );
        let is_cut = fitted.text != display;
        if is_cut {
            let kept = fitted.text.strip_suffix('…').expect("a cut title ends with an ellipsis");
            assert!(display.starts_with(kept), "{kept:?} is not a prefix of {display:?}");
            assert!(!kept.ends_with('\u{200d}'), "{kept:?} splits a joined emoji");
            assert!(!display[kept.len()..].starts_with('\u{200d}'), "{kept:?} splits an emoji");
        } else {
            assert_eq!(fitted.text, display);
        }
        cut.push(is_cut);
    }
    assert!(!cut[0], "a short title that fits is drawn whole");
    assert!(cut[1], "a path wider than the maximum is cut");
}

/// A stored width is part of the tab hash, so a released held width repaints the retained
/// strip even when no title changed between the two frames.
#[test]
fn stored_tab_widths_invalidate_the_retained_tab_strip() {
    let now = Instant::now();
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("zsh"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("vim"));
    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);
    let id = tabs.tabs()[0].id;
    tabs.set_title(id, "cargo build --release");
    tabs.refresh_content_widths(now, false, 1, true, ten_px_per_char);
    let held = tab_bar_hash(&tabs, now);

    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);

    assert_ne!(tab_bar_hash(&tabs, now), held, "a released width must repaint the strip");
}

/// Either width limit moves idle tabs, so a `tab_min_width` or `tab_max_width` change
/// repaints the retained strip even when no title or stored width changed.
#[test]
fn tab_width_limits_invalidate_the_retained_tab_strip() {
    let now = Instant::now();
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("zsh"));
    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);
    let defaults = tab_bar_hash_with_limits(&tabs, now, 240.0, 320.0);

    assert_eq!(tab_bar_hash_with_limits(&tabs, now, 240.0, 320.0), defaults);
    assert_ne!(tab_bar_hash_with_limits(&tabs, now, 200.0, 320.0), defaults, "tab_min_width");
    assert_ne!(tab_bar_hash_with_limits(&tabs, now, 240.0, 400.0), defaults, "tab_max_width");
}

/// Any change to the family, size, weight, DPI scale or tab font measures every tab again,
/// even on a held bar.
#[test]
fn every_font_and_scale_input_changes_the_tab_font_key() {
    let base = tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, true);
    assert_eq!(base, tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, true));
    for other in [
        tab_font_key("Menlo", 14.0, 1.0, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 15.0, 1.0, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.25, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 1.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, false),
    ] {
        assert_ne!(other, base);
    }
}

/// Every privileged tab emits one bounded vector lock made from the same quad stream as other chrome.
#[test]
fn privilege_badge_emits_one_vector_lock_per_tab() {
    let mut quads = Vec::new();
    let first = TabTitleRect { x: 10.0, y: 2.0, w: 18.0, h: 18.0 };
    let second = TabTitleRect { x: 40.0, y: 2.0, w: 18.0, h: 18.0 };
    let danger = [0.8, 0.05, 0.02, 1.0];

    emit_privilege_badge_quads(&mut quads, first, danger, 1.0, (200.0, 40.0));
    emit_privilege_badge_quads(&mut quads, second, danger, 0.5, (200.0, 40.0));

    assert_eq!(quads.len(), PRIVILEGE_BADGE_QUAD_COUNT * 2);
    assert_eq!(quads[PRIVILEGE_BADGE_QUAD_COUNT].color[3], 0.5);
    assert_eq!(quads[PRIVILEGE_BADGE_QUAD_COUNT + 1].color[3], 0.5);
    for part in privilege_lock_rects(first) {
        assert!(part.x >= first.x && part.y >= first.y);
        assert!(part.x + part.w <= first.x + first.w);
        assert!(part.y + part.h <= first.y + first.h);
    }
}

/// Black-or-white lock geometry keeps at least WCAG AA contrast against varied danger colors.
#[test]
fn privilege_lock_color_contrasts_with_dark_light_and_high_contrast_badges() {
    for danger in [
        [0.8, 0.05, 0.02, 1.0],
        [0.08, 0.01, 0.01, 1.0],
        [1.0, 0.72, 0.72, 1.0],
        [1.0, 0.0, 0.0, 1.0],
    ] {
        let lock = privilege_lock_color(danger);
        assert!(linear_contrast_ratio(danger, lock) >= 4.5);
    }
}

/// Hover, activity, custom title color, and focus do not enter privilege-badge resolution.
#[test]
fn privilege_badge_uses_only_process_state_theme_danger_and_drag_alpha() {
    // The call stays outside tab-title color resolution so tab presentation cannot recolor the warning.
    const CORE: &str = include_str!("core.rs");
    let start = CORE.find("if let Some(badge) = placement.badge_rect").expect("badge branch");
    let call = &CORE[start..start + 500];

    assert!(call.contains("ui_palette.danger"));
    assert!(call.contains("badge_alpha"));
    for excluded in ["active_panel_focused", "hovered", "custom_color"] {
        assert!(!call.contains(excluded), "badge unexpectedly depends on {excluded}");
    }
}

/// The Windows software compositor consumes the same vector badge quads as the GPU path.
#[cfg(target_os = "windows")]
#[test]
fn privilege_badge_quads_rasterize_on_the_windows_software_path() {
    use sonicterm_text::glyph_atlas::GlyphAtlas;

    let badge = TabTitleRect { x: 10.0, y: 10.0, w: 18.0, h: 18.0 };
    let mut quads = Vec::new();
    emit_privilege_badge_quads(&mut quads, badge, [1.0, 0.0, 0.0, 1.0], 1.0, (40.0, 40.0));
    let glyph_atlas = GlyphAtlas::new(1, 1);
    let image_atlas = GlyphAtlas::new(1, 1);
    let mut frame = crate::software_frame::SoftwareFrame::new(40, 40, [0.0, 0.0, 0.0, 1.0])
        .expect("valid software frame");

    frame.draw_layers(&glyph_atlas, &image_atlas, &quads, &[], &[], &[], &[]);

    assert_eq!(frame.pixel_bgra_at(12, 20), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel_bgra_at(18, 20), Some([0, 0, 0, 255]));
    assert_eq!(frame.pixel_bgra_at(0, 0), Some([0, 0, 0, 255]));
}

/// Degraded rendering treats scrollbar changes as a full-frame signal.
#[test]
fn scrollbar_change_forces_degraded_full_render() {
    assert_eq!(
        decide_render_mode(true, RenderSignals { scrollbar_change: true, ..Default::default() },),
        RenderMode::Full
    );
}

#[test]
fn full_repaint_forced_on_invalidation() {
    let damage = Some(sonicterm_render_model::geometry::PixelRect { x: 1, y: 2, w: 3, h: 4 });
    let mut cases = [
        RenderSignals { first_frame: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { resize: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { dpi_or_scale_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { font_or_atlas_rebuild: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { theme_or_config_reload: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { surface_reconfigure: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { occlusion_restore: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { viewport_scroll: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { selection_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { tab_switch: true, dirty_damage: damage, ..Default::default() },
        RenderSignals { pane_topology_change: true, dirty_damage: damage, ..Default::default() },
        RenderSignals {
            overlay_active_or_toggled: true,
            dirty_damage: damage,
            ..Default::default()
        },
    ];
    for signals in cases.iter_mut() {
        assert_eq!(decide_render_mode(true, *signals), RenderMode::Full);
    }
    assert_eq!(
        decide_render_mode(true, RenderSignals { dirty_damage: damage, ..Default::default() }),
        RenderMode::Full
    );
}

#[test]
fn non_degrade_always_full() {
    assert_eq!(decide_render_mode(false, RenderSignals::default()), RenderMode::Full);
}

#[test]
fn overlay_active_forces_full() {
    assert_eq!(
        decide_render_mode(
            true,
            RenderSignals {
                overlay_active_or_toggled: true,
                dirty_damage: Some(sonicterm_render_model::geometry::PixelRect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
                ..Default::default()
            },
        ),
        RenderMode::Full
    );
}

/// Focus feedback advances through visible buckets and expires exactly at its bound.
#[test]
fn pane_focus_flash_sample_advances_and_expires() {
    let first = pane_focus_flash_sample(Duration::ZERO).expect("flash starts visible");
    let next = pane_focus_flash_sample(Duration::from_millis(16)).expect("second bucket visible");
    let last =
        pane_focus_flash_sample(Duration::from_millis(359)).expect("last millisecond visible");

    assert_eq!(first.0, 1);
    assert_eq!(next.0, 2);
    assert!(first.1 > next.1 && next.1 > last.1, "flash alpha must fade monotonically");
    assert_eq!(pane_focus_flash_sample(Duration::from_millis(360)), None);
}

#[test]
fn damage_empty_is_noop() {
    assert_eq!(decide_render_mode(true, RenderSignals::default()), RenderMode::Noop);
}

#[test]
fn surface_size_budget_accepts_8k_and_minimized_windows() {
    let eight_k =
        validated_surface_size(7680, 4320, MAX_SURFACE_DIMENSION).expect("8K must fit the budget");
    assert_eq!((eight_k.width, eight_k.height), (7680, 4320));
    assert_eq!(eight_k.bytes, 7680 * 4320 * 4);
    let dci_eight_k = validated_surface_size(8192, 4320, MAX_SURFACE_DIMENSION)
        .expect("DCI 8K must fit the budget");
    assert_eq!((dci_eight_k.width, dci_eight_k.height), (8192, 4320));

    let minimized =
        validated_surface_size(0, 0, MAX_SURFACE_DIMENSION).expect("zero clamps to one pixel");
    assert_eq!((minimized.width, minimized.height, minimized.bytes), (1, 1, 4));
}

#[test]
fn surface_size_budget_rejects_multi_gibabyte_frames() {
    assert!(validated_surface_size(8192, 8192, MAX_SURFACE_DIMENSION).is_none());
    assert!(validated_surface_size(u32::MAX, u32::MAX, MAX_SURFACE_DIMENSION).is_none());
    assert!(validated_surface_size(MAX_SURFACE_DIMENSION + 1, 1, MAX_SURFACE_DIMENSION).is_none());
}

/// Curly underline geometry must preserve its resolved color while joining alternating segments.
#[test]
fn curly_underline_segments_preserve_resolved_color() {
    let mut cell = Cell::plain('x', Color::Rgb(0, 255, 255), Color::Default, CellFlags::UNDERLINE);
    cell.set_underline_style(UnderlineStyle::Curly);
    let (style, resolved) = underline_key(&cell).expect("visible underline has a resolved color");
    assert_eq!(resolved, cell.fg, "missing SGR 58 must fall back to foreground");
    let color = [0.0, 1.0, 1.0, 1.0];
    let surface = (100.0, 80.0);
    let mut quads = Vec::new();

    push_underline_quads(
        &mut quads, style, 10.0, 20.0, 24.0, 12.0, 2.0, surface.0, surface.1, color,
    );

    assert!(quads.len() >= 2);
    let mut previous_end: Option<[f32; 2]> = None;
    let mut previous_slope: Option<f32> = None;
    for quad in quads {
        assert_eq!(quad.color, color);
        assert!(quad.line_thickness_px > 0.0);
        let (x, y, w, h) =
            crate::wezterm_pipeline::ndc_rect_to_pixels(quad.rect, surface.0, surface.1)
                .expect("curly segment has drawable geometry");
        let center = [x + w * 0.5, y + h * 0.5];
        let start = [center[0] + quad.line_a[0], center[1] + quad.line_a[1]];
        let end = [center[0] + quad.line_b[0], center[1] + quad.line_b[1]];
        if let Some(previous_end) = previous_end {
            assert!((start[0] - previous_end[0]).abs() < 0.001);
            assert!((start[1] - previous_end[1]).abs() < 0.001);
        }
        let slope = end[1] - start[1];
        if let Some(previous_slope) = previous_slope {
            assert!(slope * previous_slope < 0.0, "adjacent curl segments must alternate slope");
        }
        previous_end = Some(end);
        previous_slope = Some(slope);
    }
}

/// Styled whitespace continues the decoration; unstyled whitespace remains a deliberate break.
#[test]
fn underline_key_preserves_styled_spaces() {
    let mut blank = Cell::plain(' ', Color::Indexed(1), Color::Default, CellFlags::UNDERLINE);
    blank.set_underline_style(UnderlineStyle::Dashed);
    assert_eq!(underline_key(&blank), Some((UnderlineStyle::Dashed, Color::Indexed(1))));
    blank.flags.remove(CellFlags::UNDERLINE);
    assert_eq!(underline_key(&blank), None);

    let underlined = Cell::plain('x', Color::Indexed(1), Color::Default, CellFlags::UNDERLINE);
    assert_eq!(underline_key(&underlined), Some((UnderlineStyle::Single, Color::Indexed(1))));
}

#[test]
fn inverse_swaps_foreground_and_background_for_rendering() {
    let theme = Theme::default();
    let cell = Cell::plain('x', Color::Indexed(1), Color::Indexed(2), CellFlags::INVERSE);
    assert_eq!(cell_fg(&cell, &theme, ChromeColor::WHITE), indexed(2, &theme).unwrap());
    assert_eq!(
        cell_bg_rgba(&cell, &theme),
        Some(chrome_color_to_linear_rgba(indexed(1, &theme).unwrap()))
    );
}

/// A literal bar glyph typed into the palette query is query text: the renderer
/// positions the caret from the field display offset and never strips or splits on it.
#[test]
fn palette_query_keeps_literal_bar_glyph() {
    const CORE_SRC: &str = include_str!("core.rs");
    let bar = '\u{258f}';
    assert!(!CORE_SRC.contains(&format!(".replace('{bar}'")), "renderer strips the bar glyph");
    assert!(!CORE_SRC.contains(&format!(".split('{bar}')")), "renderer splits on the bar glyph");
    assert!(CORE_SRC.contains("FieldText::palette("), "palette caret comes from the field display");
}

// Two-line command text shares equal outer margins instead of pushing details against the highlight.
#[test]
fn palette_detail_block_has_balanced_padding() {
    for scale in [1.0, 1.5, 2.0] {
        let height = (sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT
            + sonicterm_render_model::boundary::ui::overlays::PALETTE_DETAIL_HEIGHT)
            * scale;
        let label = 13.0 * scale;
        let detail = 12.0 * scale;
        let gap = 4.0 * scale;
        let (label_baseline, detail_top, detail_baseline) =
            palette_detail_positions(height, label, detail, gap);
        let top = label_baseline - label * 0.8;
        let bottom = height - (detail_baseline + detail * 0.2);
        assert!((top - bottom).abs() < 0.001);
        assert!((detail_top - (top + label) - gap).abs() < 0.001);
        assert!((bottom - 7.5 * 0.8 * scale).abs() < 0.001);
    }
}

// Oversized fonts retain a visible subtitle clip inside the fixed-height row.
#[test]
fn palette_detail_large_fonts_keep_a_visible_clip() {
    for scale in [1.0, 1.5, 2.0] {
        for size in [22.0, 34.0] {
            let height = 38.0 * scale;
            let detail_size = (size - 1.0) * scale;
            let (_, detail_top, baseline) =
                palette_detail_positions(height, size * scale, detail_size, 4.0 * scale);
            assert!(detail_top >= 0.0);
            assert!(height - detail_top >= detail_size - 0.001);
            assert!(baseline < height);
        }
    }
}

#[test]
fn palette_footer_is_one_logical_pixel_smaller_and_native_at_windows_scales() {
    // Contract: footer text stays one logical pixel smaller and rasterizes at native scale.
    assert_eq!(palette_footer_font_size(13.0), 12.0);
    assert_eq!(palette_footer_font_size(1.0), 1.0);

    for scale in [1.0_f32, 1.25, 1.5, 1.75] {
        let requested_font_size = palette_footer_font_size(13.0) * scale;
        let native_em = palette_footer_font_size(13.0) * scale;

        assert_eq!(requested_font_size, 12.0 * scale);
        assert_eq!(
            requested_font_size.to_bits(),
            native_em.to_bits(),
            "footer {requested_font_size}px rescales a {native_em}px atlas tile at {scale}x"
        );
    }
}

#[test]
fn palette_footer_uses_native_regular_natural_spacing_and_scaled_geometry() {
    // Contract: footer rendering uses its native regular stack, natural advances, and inset clip.
    const CORE_SRC: &str = include_str!("core.rs");
    let start = CORE_SRC
        .find("if let Some(footer_stack) = self.palette_footer_font_stack.as_ref()")
        .expect("footer must use its native stack");
    let end = CORE_SRC[start..]
        .find("// Inline IME preedit at the TERMINAL CURSOR")
        .map(|offset| start + offset)
        .expect("footer block must end before inline IME rendering");
    let footer = &CORE_SRC[start..end];

    assert!(footer.contains("let mut footer_rasterizer = footer_stack.clone();"));
    assert!(footer.contains("let footer_native_em = footer_font_size;"));
    assert!(footer.contains("chrome_text::layout_with_raster_variant("));
    assert!(footer.contains("GlyphRasterVariant::PaletteFooter"));
    assert!(footer.contains("ChromeAttrs::default(),"), "footer text remains regular");
    assert!(!footer.contains("tracking"), "footer must keep the font's natural advances");
    assert!(!footer.contains("bold: true"), "footer must not request a bold face");
    assert!(footer.contains("self.chrome_px(PALETTE_FOOTER_INSET_X)"));
    assert!(footer.contains("Some(ChromeClip {"));
}

#[test]
fn body_title_and_footer_stacks_share_configuration_and_native_size_identity() {
    // Contract: chrome roles share font configuration but retain distinct native size identities.
    const CORE_SRC: &str = include_str!("core.rs");
    let helper_start = CORE_SRC.find("fn renderer_font_stacks(").expect("stack builder");
    let helper_end = CORE_SRC[helper_start..]
        .find("fn software_block_glyph_target_rect(")
        .map(|offset| helper_start + offset)
        .expect("stack builder end");
    let helper = &CORE_SRC[helper_start..helper_end];
    assert_eq!(helper.matches("try_new_full_with_weight_and_font_dirs(").count(), 1);
    assert!(helper.contains("font_dirs"));
    assert_eq!(helper.matches("with_font_size(").count(), 2);

    // Platform locators can accept a family without resolving it on a CI host;
    // the tracked asset fixture makes the native-size metric assertion real.
    let _font_lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().expect("font fixture lock");
    let body = crate::lib_tests::tracked_font_stack(13.0);
    let title = body.with_font_size(14.0);
    let footer = body.with_font_size(12.0);
    assert!(body.shares_configuration_with(&title));
    assert!(body.shares_configuration_with(&footer));
    assert!(title.shares_configuration_with(&footer));

    let body_metrics = body.cell_metrics_raster_px().expect("body metrics");
    let title_metrics = title.cell_metrics_raster_px().expect("title metrics");
    let footer_metrics = footer.cell_metrics_raster_px().expect("footer metrics");
    assert!(title_metrics.cell_h > body_metrics.cell_h);
    assert!(footer_metrics.cell_h < body_metrics.cell_h);

    let set_font_start = CORE_SRC.find("    pub fn set_font(").expect("set_font exists");
    let set_font_end = CORE_SRC[set_font_start..]
        .find("\n    pub fn set_scale_factor(")
        .map(|offset| set_font_start + offset)
        .expect("set_font has a bounded body");
    let set_font = &CORE_SRC[set_font_start..set_font_end];
    assert!(
        set_font.contains("renderer_font_stacks(family, size, dpi, weight_scale, &self.font_dirs)")
    );
    // The body stack is installed through the seam that also attaches the fallback waker.
    let install =
        set_font.find("frame_fonts::install_body_stack(").expect("missing stack replacement: body");
    assert!(
        set_font[install..].split_once(");").expect("a bounded call").0.contains("new_stacks.body"),
        "the body seam installs the new body stack"
    );
    for assignment in [
        "self.tab_title_font.set_font(family, size, weight_scale, new_stacks.tab_title);",
        "self.palette_footer_font_stack = new_stacks.palette_footer;",
    ] {
        assert!(set_font.contains(assignment), "missing stack replacement: {assignment}");
    }
    // The scale rebuild rescales the tab-title font through the same device-free state.
    assert!(CORE_SRC.contains("self.tab_title_font.set_scale_factor(sf, fs_dpi);"));
}

#[test]
fn title_size_helper_is_used_only_for_title_stack_and_title_rendering() {
    // Protect title sizing from body/footer leaks while keeping vector privilege chrome font-independent.
    const CORE_SRC: &str = include_str!("core.rs");
    assert_eq!(CORE_SRC.matches("tab_title_font_size(").count(), 1);
    // The tab-title font sizes the stack it measures and draws with.
    assert_eq!(include_str!("tab_title_font.rs").matches("tab_title_font_size(").count(), 1);
    assert!(CORE_SRC.contains("self.tab_title_font.stack(), tab_rasterizer.as_mut()"));
    assert!(CORE_SRC.contains("} else if show_privilege_badge {"));
    assert!(CORE_SRC.contains("GlyphRasterVariant::TabTitle"));
}

#[test]
fn longest_palette_footer_fits_supported_panel_width_with_natural_spacing() {
    // Contract: the longest localized footer fits every supported body size without compression.
    use sonicterm_render_model::boundary::ui::command_palette::{
        CommandPalette, CommandPaletteMode,
    };
    use sonicterm_render_model::boundary::ui::overlays::{PaletteLayout, PALETTE_WIDTH};

    let _font_lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().expect("font fixture lock");
    let mut longest = String::new();
    let mut supported_width = 0.0_f32;
    for mode in [
        CommandPaletteMode::Commands,
        CommandPaletteMode::RenameTab,
        CommandPaletteMode::RenameWindow,
        CommandPaletteMode::TabColor,
    ] {
        let mut palette = CommandPalette::new();
        match mode {
            CommandPaletteMode::Commands => palette.open(),
            CommandPaletteMode::RenameTab => palette.start_rename_tab("tab"),
            CommandPaletteMode::RenameWindow => palette.start_rename_window("window"),
            CommandPaletteMode::TabColor => palette.start_tab_color_picker("tab", Vec::new()),
        }
        let layout = PaletteLayout::compute(&mut palette, 4000.0, 2400.0, 0.0, 1.0)
            .expect("open palette has layout");
        assert_eq!(layout.footer.w, PALETTE_WIDTH - 2.0);
        if layout.footer_label.chars().count() > longest.chars().count() {
            longest = layout.footer_label;
            supported_width = layout.footer.w;
        }
    }

    let available = supported_width - PALETTE_FOOTER_INSET_X * 2.0;
    for body_font_size in [13.0_f64, 14.5, 18.0] {
        let footer_font_size = f64::from(palette_footer_font_size(body_font_size as f32));
        let stack = crate::lib_tests::tracked_font_stack(footer_font_size);
        let shaped = stack.measure_text_width(&longest).expect("tracked footer font shapes");
        assert!(
            shaped <= available + f32::EPSILON,
            "footer at body {body_font_size}px must fit: {shaped}px text in {available}px: {longest:?}"
        );
    }
}

#[test]
fn plain_url_hover_does_not_need_accent_palette() {
    use sonicterm_render_model::inputs::HoveredUrlCells;

    assert!(!hovered_url_needs_accent(None));
    assert!(!hovered_url_needs_accent(HoveredUrlCells::single(7, 0, 1, 5, false)));
    assert!(hovered_url_needs_accent(HoveredUrlCells::single(7, 0, 1, 5, true)));
}

/// Active wrapped fragments perturb only their owning pane and intersecting row cache keys.
#[test]
fn wrapped_hover_cache_identity_is_pane_and_row_local() {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};

    let hovered = HoveredUrlCells::new(
        7,
        [
            HoveredUrlSpan { row: 2, start_col: 3, end_col: 10 },
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 4 },
        ],
        true,
    )
    .unwrap();
    let first = hovered_url_for_pane_row(Some(hovered), 7, 2).unwrap();
    let second = hovered_url_for_pane_row(Some(hovered), 7, 3).unwrap();

    // Each row folds only its own fragment's columns into its content key.
    assert_eq!(hovered_url_row_key_span(Some(first), 2), Some((3, 10)));
    assert_eq!(hovered_url_row_key_span(Some(second), 3), Some((0, 4)));
    assert_eq!(hovered_url_row_key_span(Some(first), 3), None, "no fragment on that row");
    assert!(hovered_url_for_pane_row(Some(hovered), 8, 2).is_none());
    assert!(hovered_url_for_pane_row(Some(hovered), 7, 1).is_none());
}

/// Wrapped hover fragments project to exact snapped rectangles and reject invisible coverage.
#[test]
fn wrapped_hover_fragments_project_exact_underline_rectangles() {
    use sonicterm_render_model::inputs::HoveredUrlSpan;

    let edges = build_snapped_cell_x(10.0, 7.5, 8);
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 1, start_col: 2, end_col: 8 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        Some((edges[2], 36.0, edges[8] - edges[2], 16.0))
    );
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 2 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        None
    );
    assert_eq!(
        hovered_url_span_rect(
            HoveredUrlSpan { row: 1, start_col: 8, end_col: 9 },
            8,
            3,
            10.0,
            20.0,
            7.5,
            16.0,
            &edges,
        ),
        None
    );
}

/// Hint-only wrapped fragments reuse ordinary glyph rows because they change only overlay geometry.
#[test]
fn wrapped_hover_hint_keeps_plain_row_cache_identity() {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};

    let hovered = HoveredUrlCells::new(
        7,
        [
            HoveredUrlSpan { row: 2, start_col: 3, end_col: 10 },
            HoveredUrlSpan { row: 3, start_col: 0, end_col: 4 },
        ],
        false,
    )
    .unwrap();
    let row = hovered_url_for_pane_row(Some(hovered), 7, 3).unwrap();

    assert_eq!(hovered_url_row_key_span(Some(row), 3), None, "a hint folds nothing");
}

/// HarfBuzz placement offsets move the origin without resizing the tile.
#[test]
fn shaped_glyph_position_applies_signed_offsets() {
    let natural = (12.0, 24.0, 8.0, 10.0);
    assert_eq!(positioned_shaped_glyph_rect(natural, 2.5, -7.25), (14.5, 16.75, 8.0, 10.0));
    assert_eq!(positioned_shaped_glyph_rect(natural, -0.5, 3.0), (11.5, 27.0, 8.0, 10.0));
}

/// Shaped glyphs accumulate advances within one cluster and reset at the next cell.
#[test]
fn shaped_cluster_position_uses_running_harfbuzz_pen() {
    use sonicterm_text::shape::ShapedGlyph;

    let mut col = None;
    let mut pen = 0.0;
    let first = ShapedGlyph {
        lead_col: 3,
        cluster_cells: 1,
        font_slot: 0,
        glyph_id: 1,
        x_advance: 6.0,
        x_offset: 1.5,
        y_offset: 0.0,
        ch: 'م',
    };
    let mark = ShapedGlyph { glyph_id: 2, x_advance: 0.0, x_offset: -2.0, ..first };
    let next = ShapedGlyph { lead_col: 4, glyph_id: 3, x_advance: 5.0, x_offset: 0.5, ..first };

    assert_eq!(shaped_cluster_x_offset(&mut col, &mut pen, &first), 1.5);
    assert_eq!(shaped_cluster_x_offset(&mut col, &mut pen, &mark), 4.0);
    assert_eq!(shaped_cluster_x_offset(&mut col, &mut pen, &next), 0.5);
}

#[test]
fn shaped_glyph_column_check_allows_multiple_glyphs_in_one_cell_cluster() {
    use sonicterm_text::shape::ShapedGlyph;

    let glyphs = [
        ShapedGlyph {
            lead_col: 0,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 1,
            x_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            ch: '✔',
        },
        ShapedGlyph {
            lead_col: 0,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 2,
            x_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            ch: '✔',
        },
        ShapedGlyph {
            lead_col: 1,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 3,
            x_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            ch: 'x',
        },
    ];

    assert!(shaped_glyph_columns_are_monotonic(&glyphs));
}

#[test]
fn shaped_glyph_column_check_rejects_backtracking_columns() {
    use sonicterm_text::shape::ShapedGlyph;

    let glyphs = [
        ShapedGlyph {
            lead_col: 1,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 1,
            x_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            ch: 'x',
        },
        ShapedGlyph {
            lead_col: 0,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 2,
            x_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            ch: 'y',
        },
    ];

    assert!(!shaped_glyph_columns_are_monotonic(&glyphs));
}

// --- Atlas reset / row-cache invalidation pairing ------------------

/// Every atlas reset must flush the row glyph cache and defeat the frame-skip
/// guard, in the same function.
///
/// The row cache's monotonic atlas identity rejects stale UVs even if a future
/// reset path misses this wholesale clear. The explicit invalidation remains
/// load-bearing for prompt reclamation and bounds stale entries that can never
/// match again.
///
/// `last_frame_key` is the second half: it skips presentation when a frame is
/// byte-identical to the last one. A reset changes what that same frame
/// *renders to*, so a surviving key can skip the redraw and leave pre-reset
/// pixels on screen.
///
/// Nothing structural enforces either pairing. What holds today is that all
/// four `reset_glyph_atlas_in_place` call sites happen to do both in the same
/// function. A fifth reset path missing either call reintroduces a
/// stale-pixel class with no compile error and no failing test.
///
/// Order is deliberately not asserted. Flushing before the reset is at least
/// as safe as flushing after — one arrangement hoists the invalidation above
/// the reset so it covers both transition directions — so the requirement is
/// that both calls appear in the same function, not that they appear in a
/// particular sequence.
///
/// `GpuRenderer` needs a live wgpu device and a window, so the pairing cannot
/// be driven at runtime in a unit test. Reading the source is the available
/// check, and it is the one that would actually catch the regression: the
/// mistake being guarded is an omitted call, which is visible in the text.
#[test]
fn every_glyph_atlas_reset_invalidates_the_row_cache() {
    // Include lifecycle and recovery reset sites so moving a helper cannot hide missing invalidation.
    let core_source =
        [include_str!("core.rs"), include_str!("atlas_lifecycle.rs"), include_str!("rebind.rs")]
            .join("\n");
    const RESET: &str = "self.reset_glyph_atlas_in_place(";
    const INVALIDATE: &str = "self.row_glyph_cache.invalidate_all()";
    const CLEAR_FRAME_KEY: &str = "self.last_frame_key = None";

    let lines: Vec<&str> = core_source.lines().collect();
    let reset_sites: Vec<usize> =
        lines.iter().enumerate().filter(|(_, l)| l.contains(RESET)).map(|(i, _)| i).collect();

    // Guard against the check silently passing because the call was renamed
    // and no site matches any more.
    assert!(
        reset_sites.len() >= 4,
        "expected at least the four known atlas reset sites, found {}; \
         if `reset_glyph_atlas_in_place` was renamed, update this test",
        reset_sites.len()
    );

    for &site in &reset_sites {
        // Search the whole enclosing function, not just forward from the
        // reset. Flushing the cache *before* replacing the atlas is at least
        // as safe as flushing it after, and one call site does exactly that —
        // hoisting the invalidation above the reset so it runs on both
        // transition directions. A forward-only scan would read that safer
        // arrangement as a violation, so the invariant is "somewhere in the
        // same function", not "after".
        let start = lines[..site]
            .iter()
            .rposition(|line| {
                line.starts_with("    fn ")
                    || line.starts_with("    pub fn ")
                    || line.starts_with("    pub(crate) fn ")
                    || line.starts_with("    pub(super) fn ")
            })
            .map_or(0, |index| index + 1);
        let end = lines
            .iter()
            .enumerate()
            .skip(site + 1)
            .find(|(_, line)| {
                line.starts_with("    fn ")
                    || line.starts_with("    pub fn ")
                    || line.starts_with("    pub(crate) fn ")
                    || line.starts_with("    pub(super) fn ")
                    || line.starts_with("impl ")
                    || line.starts_with("}")
            })
            .map_or(lines.len(), |(index, _)| index);

        let body = &lines[start..end];
        let invalidated = body.iter().any(|line| line.contains(INVALIDATE));
        let frame_key_cleared = body.iter().any(|line| line.contains(CLEAR_FRAME_KEY));
        assert!(
            invalidated,
            "renderer source line {} resets the glyph atlas without calling \
             `row_glyph_cache.invalidate_all()` in the same function.\n\
             The row cache identity rejects stale UVs, but reset must also \
             reclaim entries that can never match the new atlas.\n\
             Offending line: {}",
            site + 1,
            lines[site].trim()
        );
        assert!(
            frame_key_cleared,
            "renderer source line {} resets the glyph atlas without clearing \
             `last_frame_key` in the same function.\n\
             The frame key skips presentation when a frame is unchanged. A reset \
             changes what the same frame renders to, so a stale key can skip the \
             redraw and leave pre-reset pixels on screen.\n\
             Offending line: {}",
            site + 1,
            lines[site].trim()
        );
    }
}

// --- Image atlas promotion / demotion ------------------------------

/// A window with no inline media must not clear its image atlas on every
/// frame it draws.
///
/// The frame-assembly guard asks whether inline media *changed*, and that
/// question is answered `true` whenever the previous frame key is absent —
/// which is every frame following any of the many state changes that clear
/// it. The media hash itself is deterministic, so on a window that has never
/// shown an image the hash arm can never fire and the absent-key arm accounts
/// for every reset. The result is one reset per rendered frame on a window
/// with nothing to reset.
///
/// Resetting an atlas that holds nothing is not free: it rebuilds the packer
/// and bumps the atlas identity, which invalidates every dependent cache
/// keyed to it. Gating on whether the atlas actually holds anything is
/// therefore correct regardless of why the frame key was cleared.
#[test]
fn an_empty_placeholder_image_atlas_is_not_reset_every_frame() {
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);

    // The reported defect: a window with no media, drawing frames, whose
    // atlas is still the untouched 1x1 placeholder. Nothing to clear.
    assert!(
        !image_atlas_reset_warranted(&placeholder),
        "an empty placeholder atlas must not be reset; there is nothing in it to clear"
    );

    // A promoted atlas carries packer and eviction state even when its entry
    // map is momentarily empty, so it must still reset. Guarding on emptiness
    // alone would strand that state and let the packer refuse new inserts.
    let promoted = GlyphAtlas::default_size();
    assert!(
        image_atlas_reset_warranted(&promoted),
        "a promoted atlas must still be reset even while its entry map is empty"
    );
}

/// The frame-assembly call site actually consults the reset guard.
///
/// The predicate test above pins the decision, but nothing structural forces
/// frame assembly to ask. Dropping the call from the condition restores one
/// reset per rendered frame on a window with no media, and it does so with no
/// compile error and no other failing test — the predicate would simply go
/// unused at that site while every assertion about it still held.
///
/// `GpuRenderer` needs a live wgpu device and a window, so the composed guard
/// cannot be driven at runtime in a unit test. Reading the source is the
/// available check, and it is the one that catches this regression: the
/// mistake being guarded is an omitted call, which is visible in the text.
#[test]
fn image_atlas_reset_is_gated_on_the_atlas_holding_something() {
    const CORE_SRC: &str = include_str!("core.rs");
    const RESET_CALL: &str = "self.reset_image_atlas()";
    const GUARD: &str = "image_atlas_reset_warranted(";

    let lines: Vec<&str> = CORE_SRC.lines().collect();
    let reset_sites: Vec<usize> =
        lines.iter().enumerate().filter(|(_, l)| l.contains(RESET_CALL)).map(|(i, _)| i).collect();

    // Guard against the check silently passing because the call was renamed
    // and no site matches any more.
    assert!(
        reset_sites.len() >= 2,
        "expected at least the two known image-atlas reset sites, found {}; \
         if `reset_image_atlas` was renamed, update this test",
        reset_sites.len()
    );

    // The frame-assembly site is the per-frame one. Find it by the condition
    // that precedes it: the surface-transition site resets unconditionally
    // once, which is correct there and must not be required to carry a guard.
    let frame_site = reset_sites
        .iter()
        .copied()
        .find(|&site| {
            lines[site.saturating_sub(6)..site].iter().any(|l| l.contains("inline_media_changed"))
        })
        .expect(
            "no image-atlas reset site is preceded by an `inline_media_changed` condition; \
             if frame assembly was restructured, update this test",
        );

    let guard_window = &lines[frame_site.saturating_sub(6)..frame_site];
    assert!(
        guard_window.iter().any(|l| l.contains(GUARD)),
        "the per-frame image-atlas reset at line {} is not gated on \
         `image_atlas_reset_warranted`; without it a window with no inline media \
         resets an empty atlas on every frame it draws",
        frame_site + 1
    );
}

/// A promoted image atlas is released once the window stops showing media,
/// but not on the first idle frame.
///
/// `reset_in_place` clears the map and repacker and never touches the pixel
/// buffer, so promotion is otherwise permanent: a window that displays one
/// inline image holds 16 MiB of CPU pixels — plus a matching GPU texture off
/// the software path — until it closes. Across windows that is the largest
/// retained term in the process.
///
/// The delay is the load-bearing part. Demoting on the first frame without
/// visible media would free and reallocate the atlas every time an image
/// scrolled out of view and back, re-decoding every visible image each time.
#[test]
fn an_idle_image_atlas_is_released_but_not_on_the_first_idle_frame() {
    let promoted = GlyphAtlas::default_size();
    let placeholder = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);

    // Media visible: never demote, however long the window has been idle
    // before — the counter resets at the call site.
    assert!(
        !image_atlas_demotion_ready(&promoted, true, IMAGE_ATLAS_IDLE_FRAMES * 10),
        "an atlas must never be released while media is on screen"
    );

    // Idle, but not yet long enough.
    assert!(
        !image_atlas_demotion_ready(&promoted, false, 0),
        "the first idle frame must not release the atlas"
    );
    assert!(
        !image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES - 1),
        "one frame short of the threshold must not release the atlas"
    );

    // Idle long enough.
    assert!(
        image_atlas_demotion_ready(&promoted, false, IMAGE_ATLAS_IDLE_FRAMES),
        "a sustained absence of media must release the atlas"
    );

    // Already at placeholder size: nothing to release, so no repeated work.
    assert!(
        !image_atlas_demotion_ready(&placeholder, false, IMAGE_ATLAS_IDLE_FRAMES * 10),
        "a placeholder atlas must not be re-released every frame"
    );

    // Promotion and demotion must not both fire for the same state, or a
    // window with no media would allocate and free every frame.
    for frames in [0, IMAGE_ATLAS_IDLE_FRAMES, IMAGE_ATLAS_IDLE_FRAMES * 2] {
        for has_media in [false, true] {
            let promote = image_atlas_promotion_required(&placeholder, has_media);
            let demote = image_atlas_demotion_ready(&placeholder, has_media, frames);
            assert!(
                !(promote && demote),
                "placeholder atlas: promote and demote both fired (media={has_media}, frames={frames})"
            );
            let promote_full = image_atlas_promotion_required(&promoted, has_media);
            let demote_full = image_atlas_demotion_ready(&promoted, has_media, frames);
            assert!(
                !(promote_full && demote_full),
                "promoted atlas: promote and demote both fired (media={has_media}, frames={frames})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Renderer retention reporting
//
// `GpuRenderer` needs a real adapter, so it cannot be built here. The
// aggregation and class mapping are pure and are tested directly; what a live
// renderer puts into them is covered by the atlas and software-frame suites
// that already exist.
// ---------------------------------------------------------------------------

fn amount(bytes: usize, items: usize) -> ResourceAmount {
    ResourceAmount { bytes, items }
}

/// Every part must reach a class, and no part may be charged twice.
///
/// The failure this guards is a part added to `RendererRetention` without a
/// matching row in `seam_classes` — it would be counted by `total()` and
/// classified as nothing, so the struct would report a byte it could not name.
///
/// A structural check on the struct, not on a charge path. Nothing charges
/// these classes; see `RendererRetention::seam_classes`.
#[test]
fn every_reported_part_is_classified_exactly_once() {
    let retention = RendererRetention {
        glyph_atlas: amount(16 * 1024 * 1024, 512),
        image_atlas: amount(8 * 1024 * 1024, 12),
        row_glyph_cache: amount(2 * 1024 * 1024, 120),
        row_quad_cache: amount(1024 * 1024, 80),
        software_frame: amount(4 * 1024 * 1024, 1),
        vertex_scratch: amount(3 * 1024 * 1024, 1),
        row_ink: amount(64 * 1024, 40),
    };

    let classes = retention.seam_classes();
    let classified: usize = classes.iter().map(|(_, part)| part.bytes).sum();

    assert_eq!(
        classified,
        retention.total().bytes,
        "the classified parts must account for every byte the struct reports"
    );

    let distinct: std::collections::HashSet<_> = classes.iter().map(|(class, _)| *class).collect();
    assert_eq!(
        distinct.len(),
        classes.len(),
        "no class may appear twice, or bytes are counted twice"
    );
}

/// The per-row ink records are one reported part, classified once under `RowInk` and inside
/// `total()`, so the bytes a partial frame's records hold are visible and never double-counted.
#[test]
fn row_ink_is_reported_once_under_its_own_class() {
    let retention = RendererRetention { row_ink: amount(4096, 40), ..RendererRetention::default() };
    let classes = retention.seam_classes();
    let row_ink: Vec<_> =
        classes.iter().filter(|(class, _)| *class == ResourceClass::RowInk).collect();
    assert_eq!(row_ink.len(), 1);
    assert_eq!(row_ink[0].1, amount(4096, 40));
    assert_eq!(retention.total(), amount(4096, 40));
    assert!(matches!(ResourceClass::RowInk.coverage(), ClassCoverage::UnchargedRetention { .. }));
    assert_eq!(ResourceClass::RowInk.pane_seam_term(), PaneSeamTerm::NotChargedInProduction);
}

/// Pane cache eviction is one glyph-then-quad renderer operation.
#[test]
fn pane_cache_eviction_order_is_explicit() {
    const SOURCE: &str = include_str!("core.rs");
    let start = SOURCE.find("pub fn invalidate_pane_caches").expect("pane cache eviction seam");
    let body = &SOURCE[start..];
    let glyph = body.find("self.row_glyph_cache.invalidate_pane(pane_id)").expect("glyph eviction");
    let quad = body.find("self.line_quad_cache.invalidate_pane(pane_id)").expect("quad eviction");
    assert!(glyph < quad, "glyph cache must evict before quad cache");
}

/// Both row-cache parts remain separately classified and uncharged.
#[test]
fn row_cache_parts_have_distinct_uncharged_classes() {
    let classes = RendererRetention::default().seam_classes();
    for class in [ResourceClass::RowGlyphCache, ResourceClass::RowQuadCache] {
        assert!(
            classes.iter().any(|(reported, _)| *reported == class),
            "{class:?} must remain a named renderer-retention part"
        );
        assert!(
            matches!(class.coverage(), ClassCoverage::UnchargedRetention { .. }),
            "{class:?} is measured and reported but reaches no governor owner"
        );
        assert_eq!(class.pane_seam_term(), PaneSeamTerm::NotChargedInProduction);
    }
}

/// The row-cache coverage envelopes bound what each cache can retain. The glyph cache enforces
/// its envelope: the production payload and tracking budgets sum to exactly the recorded
/// figure, so reported storage cannot pass it however many panes are drawn. The quad envelope
/// is pinned to four maximum viewport working sets of its replay record.
#[test]
fn row_cache_coverage_envelopes_cover_maximum_visible_payloads() {
    use sonicterm_text::row_glyph_cache::{
        DEFAULT_PAYLOAD_BUDGET_BYTES, DEFAULT_TRACKING_BUDGET_BYTES,
    };
    let ClassCoverage::UnchargedRetention { per_owner_bytes: glyph_bound } =
        ResourceClass::RowGlyphCache.coverage()
    else {
        panic!("RowGlyphCache must record its uncharged high-water envelope");
    };
    assert_eq!(DEFAULT_PAYLOAD_BUDGET_BYTES + DEFAULT_TRACKING_BUDGET_BYTES, glyph_bound);
    assert_eq!(glyph_bound, 512 * 1024 * 1024);
    let cache = sonicterm_text::row_glyph_cache::RowGlyphCache::new();
    assert_eq!(cache.payload_budget() + cache.tracking_budget(), glyph_bound);

    let visible_cells =
        sonicterm_render_model::boundary::grid::grid::MAX_VISIBLE_GRID_CELLS as usize;
    let headroom = 4usize;
    let table_entry = std::mem::size_of::<((u64, u64, u64), (u64, Vec<u8>))>() + 2;
    let quad_required = visible_cells
        .saturating_mul(headroom)
        .saturating_mul(std::mem::size_of::<crate::quad::QuadInstance>())
        .saturating_add(usize::from(u16::MAX).saturating_mul(table_entry));
    let ClassCoverage::UnchargedRetention { per_owner_bytes: quad_bound } =
        ResourceClass::RowQuadCache.coverage()
    else {
        panic!("RowQuadCache must record its uncharged high-water envelope");
    };
    assert!(quad_bound >= quad_required, "quad bound {quad_bound} < {quad_required}");
}

/// The software frame is reported on every platform, zero where absent.
///
/// A caller reading renderer classes should not need a `#[cfg(windows)]`
/// branch — an absent part and an empty part read the same.
#[test]
fn the_software_frame_part_is_present_on_every_platform() {
    let retention = RendererRetention::default();
    let classes = retention.seam_classes();

    assert!(
        classes.iter().any(|(class, _)| *class == ResourceClass::SoftwareFrame),
        "SoftwareFrame must be classified on every platform, zero where there is no software path"
    );
    assert_eq!(retention.total(), ResourceAmount::default(), "a default renderer holds nothing");
}

/// An empty renderer reports nothing.
#[test]
fn an_empty_renderer_reports_nothing() {
    let classes = RendererRetention::default().seam_classes();
    assert!(classes.iter().all(|(_, part)| part.bytes == 0 && part.items == 0));
}

/// The resource table's `SoftwareFrame` bound is this crate's surface clamp.
///
/// `ClassCoverage::UnchargedRetention { per_owner_bytes }` asks how much can
/// hide in a class nothing charges. The software frame is
/// `width * height * 4`, so no single figure describes it and the honest
/// answer is the most one surface may hold — [`MAX_SURFACE_BYTES`], which
/// `validated_surface_size` enforces on every construction and resize.
///
/// The table previously carried one 4K frame, 33,177,600 bytes. That is one
/// common window, not a bound: a 5K window holds 178% of it and the clamp
/// admits 5.06x. Nothing noticed it drifting, because the figure was a literal
/// checked against nothing.
///
/// Asserted against the constant, not a copy of its value, so moving the clamp
/// without moving the table fails here.
#[test]
fn the_tabled_software_frame_bound_is_the_surface_clamp() {
    use sonicterm_types::{ClassCoverage, ResourceClass};

    let ClassCoverage::UnchargedRetention { per_owner_bytes } =
        ResourceClass::SoftwareFrame.coverage()
    else {
        panic!(
            "SoftwareFrame must be UnchargedRetention: this crate computes it and no \
             governor charges it"
        );
    };

    assert_eq!(
        u64::try_from(per_owner_bytes).expect("the bound fits u64"),
        MAX_SURFACE_BYTES,
        "the resource table's SoftwareFrame bound and this crate's surface clamp \
         disagree; the table would misstate what one frame can hold"
    );

    // And the clamp is reachable in principle, or the bound is fiction. The
    // per-axis cap is the binding constraint, not the byte cap.
    let max_pixels = u64::from(MAX_SURFACE_DIMENSION) * u64::from(MAX_SURFACE_DIMENSION);
    assert!(
        max_pixels * 4 >= MAX_SURFACE_BYTES,
        "the dimension cap makes the byte clamp unreachable, so the bound describes \
         a surface that cannot exist"
    );
}

#[test]
fn a_zero_area_glyph_is_recognised_as_degenerate() {
    use sonicterm_text::glyph_atlas::GlyphInfo;
    let base = GlyphInfo {
        uv: [0.1, 0.1, 0.2, 0.2],
        px_size: [8, 12],
        px_offset: [0, 0],
        advance: 8.0,
        is_color: false,
        is_subpixel: false,
        missing: false,
    };

    assert!(!glyph_draw_is_degenerate(&base), "an ordinary glyph must still draw");

    // The atlas's empty/failed-rasterization sentinel. `(0,0)` is the atlas
    // origin, which the shelf packer gives to the first glyph of the session,
    // so drawing this samples that glyph's corner ink.
    let sentinel = GlyphInfo { uv: [0.0, 0.0, 0.0, 0.0], px_size: [0, 0], ..base };
    assert!(glyph_draw_is_degenerate(&sentinel), "the zero-area sentinel must be skipped");

    // Zero on either axis alone is still nothing to draw.
    assert!(glyph_draw_is_degenerate(&GlyphInfo { px_size: [0, 12], ..base }));
    assert!(glyph_draw_is_degenerate(&GlyphInfo { px_size: [8, 0], ..base }));

    // An inverted or empty UV rect addresses no texels of its own.
    assert!(glyph_draw_is_degenerate(&GlyphInfo { uv: [0.2, 0.1, 0.2, 0.2], ..base }));
    assert!(glyph_draw_is_degenerate(&GlyphInfo { uv: [0.1, 0.2, 0.2, 0.2], ..base }));
    assert!(glyph_draw_is_degenerate(&GlyphInfo { uv: [0.3, 0.1, 0.2, 0.2], ..base }));
}

#[test]
fn production_instance_honours_wgpu_backend_selection() {
    // Protect deterministic CI and user diagnostics from an ignored `WGPU_BACKEND` override.
    const CORE_SRC: &str = include_str!("core.rs");
    assert!(CORE_SRC.contains("InstanceDescriptor::new_with_display_handle_from_env"));
    assert!(!CORE_SRC.contains("InstanceDescriptor::new_with_display_handle(Box::new"));
}

#[test]
fn successful_frame_counter_advances_only_after_native_presentation() {
    // Protect runtime smoke from accepting a skipped, occluded, outdated, lost, or failed frame.
    const CORE_SRC: &str = include_str!("core.rs");
    assert!(CORE_SRC.contains("pub fn successful_frame_count(&self) -> u64"));
    let finish_start = CORE_SRC.find("    fn finish_successful_frame(").expect("present cleanup");
    let finish_end = CORE_SRC[finish_start..]
        .find("\n    /// This function only emits")
        .map(|offset| finish_start + offset)
        .expect("bounded present cleanup");
    let finish = &CORE_SRC[finish_start..finish_end];
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
    const CORE_SRC: &str = include_str!("core.rs");
    assert!(CORE_SRC.contains("pub fn last_missing_chrome(&self) -> &[char]"));
    let assemble_start = CORE_SRC.find("    fn assemble_frame(").expect("assembly");
    let assemble_end = CORE_SRC[assemble_start..]
        .find("\n    /// Hand assembled batches to the presenter")
        .map(|offset| assemble_start + offset)
        .expect("bounded assembly");
    let assemble = &CORE_SRC[assemble_start..assemble_end];
    // One frame scope; the preedit cache opens a nested one to capture its own run.
    assert_eq!(
        assemble
            .matches("let missing_chrome_scope = chrome_text::MissingChromeScope::enter();")
            .count(),
        1
    );
    assert!(assemble.contains("missing_chrome_chars: missing_chrome_scope.finish()"));
    assert!(
        assemble.contains("cached.missing_chrome_chars"),
        "a preedit cache hit replays its tofu"
    );
    let finish_start = CORE_SRC.find("    fn finish_successful_frame(").expect("present cleanup");
    let finish = &CORE_SRC[finish_start..finish_start + 2000];
    assert!(finish.contains("self.last_missing_chrome_chars = missing_chrome_chars"));
}

fn selection_for_rows(start: u64, end: u64) -> Selection {
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

#[test]
fn selection_quads_follow_absolute_text_through_viewport_scroll() {
    let selection = selection_for_rows(10, 11);
    let at_ten = selection_quad_rects(&selection, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]);
    let at_nine = selection_quad_rects(&selection, 9, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]);

    assert_eq!(at_ten.len(), 2);
    assert_eq!(at_nine.len(), 2);
    assert_eq!(at_nine[0].1 - at_ten[0].1, 20.0);
    assert_eq!(at_nine[1].1 - at_ten[1].1, 20.0);
}

#[test]
fn selection_quads_are_absent_when_the_range_is_outside_the_viewport() {
    let above = selection_for_rows(2, 4);
    let below = selection_for_rows(20, 22);

    assert!(selection_quad_rects(&above, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]).is_empty());
    assert!(selection_quad_rects(&below, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]).is_empty());
}

#[test]
fn selection_quad_walk_is_bounded_by_viewport_rows() {
    let huge = selection_for_rows(0, u64::MAX);
    let rects = selection_quad_rects(&huge, 1_000_000, 12, 8, 0.0, 0.0, 10.0, 20.0, &[]);
    assert_eq!(rects.len(), 12);
}

#[test]
fn copy_mode_rows_use_the_transposed_coordinate_slot() {
    let mut copy = CopyModeState::new_at((7, 11));
    copy.start_select();
    copy.cursor = (9, 13);
    let (start, end) = copy.selected_range().expect("mutable copy selection");

    assert_eq!((start.1, end.1), (11, 13), "copy-mode rows live in tuple slot one");
    assert_eq!(GpuRenderer::viewport_relative_row(start.1, 10, 8), Some(1));
    assert_eq!(GpuRenderer::viewport_relative_row(start.0, 10, 8), None);
}

/// The vertex scratch is renderer-held CPU storage: `retained_amounts` reports the
/// presentation pipeline's figure together with both atlas uploads' rect lists, `total()` counts
/// it, and it is tagged as upload staging.
#[test]
fn vertex_scratch_is_part_of_the_retained_report() {
    let retention =
        RendererRetention { vertex_scratch: amount(68 * 4096, 1), ..RendererRetention::default() };
    assert_eq!(retention.total(), amount(68 * 4096, 1));
    assert!(retention
        .seam_classes()
        .contains(&(ResourceClass::UploadStaging, amount(68 * 4096, 1))));

    let core: String = include_str!("core.rs").split_whitespace().collect();
    let report = core.find("pubfnretained_amounts(&self)").expect("retained_amounts");
    let body = &core[report..report + 600];
    assert!(body.contains("vertex_scratch:self.upload_staging_retained(),"));
    let staging = core.split_once("fnupload_staging_retained(&self)").expect("helper").1;
    assert!(staging[..300].contains("self.present_pipeline.vertex_scratch_retained()"));
}

/// Frame assembly starts one glyph cache pass before the pane loop; each pane's pin phase pins
/// every emitted row's key before its first admission; the cache-hit and miss paths of
/// `emit_row_glyphs` each record exactly one span, through the shared projection; and the tab
/// titles are appended after the cursor recolors and before search recolor.
#[test]
fn row_spans_viewports_and_title_order_follow_the_assembly() {
    let core: String = include_str!("core.rs").split_whitespace().collect();
    let begin = core
        .find("begin_glyph_pass(&mutself.row_glyph_cache,&mutself.row_ink,")
        .expect("one pass start");
    let pane_loop = core
        .find("for(pane_index,pv)inpane_views.iter().enumerate().filter(|(_,pane)|pane.planned.full_clip.is_some()){")
        .expect("per-pane loop");
    assert!(begin < pane_loop, "the pass starts before any pane pins or admits");
    let seam = core.find("pub(crate)fnassemble_pane_glyph_rows(").expect("pane seam");
    let pin = seam + core[seam..].find("shaping.row_cache.pin(pane_id,&keys);").expect("pin");
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
    let titles = core.find("glyph_instances.extend(final_layout.glyphs);").expect("title append");
    assert_eq!(calls.len(), 3);
    assert!(calls[1] < titles && titles < calls[2], "cursor recolors, titles, then search");
}

/// The transition rule `set_hover_cursor` returns: the hovered tab differs
/// between the previous and the next pointer position. The source scan below
/// pins that the method's body is exactly this comparison.
fn hover_tab_changed(
    tabs: &TabBar,
    geometry: TabBarHoverGeometry,
    previous: Option<(f32, f32)>,
    next: Option<(f32, f32)>,
) -> bool {
    hovered_tab_at(tabs, geometry, previous) != hovered_tab_at(tabs, geometry, next)
}

/// Two-tab bar used by the hover tests, with its drawn tab rects.
fn hover_test_bar(
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
fn hover_test_geometries() -> [TabBarHoverGeometry; 2] {
    let top = TabBarHoverGeometry {
        width_px: 400.0,
        bar_height_px: 40.0,
        top_offset_px: 0.0,
        visible: true,
    };
    [top, TabBarHoverGeometry { top_offset_px: 560.0, ..top }]
}

#[test]
fn an_in_tab_pointer_move_keeps_the_hovered_tab_and_requests_no_redraw() {
    // The hovered tab is the only hover fact a frame draws, so a move that
    // stays inside one tab must not ask for a frame.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let tab = rects[0];
        let center_y = tab.y + tab.h / 2.0;
        let start = (tab.x + 2.0, center_y);
        let end = (tab.x + tab.w - 2.0, center_y);
        assert_eq!(hovered_tab_at(&tabs, geometry, Some(start)), 0);
        assert_eq!(hovered_tab_at(&tabs, geometry, Some(end)), 0);
        assert!(!hover_tab_changed(&tabs, geometry, Some(start), Some(end)));
    }
}

#[test]
fn crossing_from_one_tab_to_another_changes_the_hovered_tab_exactly_once() {
    // A 1 px sweep from the first tab's center to the second's must report a
    // change only on the step that moves the hover to the second tab.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let center_y = rects[0].y + rects[0].h / 2.0;
        let from_x = rects[0].x + rects[0].w / 2.0;
        let to_x = rects[1].x + rects[1].w / 2.0;
        let mut previous = Some((from_x, center_y));
        let mut change_count = 0;
        let mut pointer_x = from_x + 1.0;
        while pointer_x <= to_x {
            let next = Some((pointer_x, center_y));
            if hover_tab_changed(&tabs, geometry, previous, next) {
                change_count += 1;
            }
            previous = next;
            pointer_x += 1.0;
        }
        assert_eq!(hovered_tab_at(&tabs, geometry, previous), 1);
        // The inter-tab gap, when there is one, holds no tab, so crossing it
        // reports leaving the first tab and entering the second.
        let gap_px = rects[1].x - (rects[0].x + rects[0].w);
        let expected_changes = if gap_px > 1.0 { 2 } else { 1 };
        assert_eq!(change_count, expected_changes, "gap {gap_px} px");
    }
}

#[test]
fn leaving_the_bar_changes_the_hovered_tab_once() {
    // Leaving a hovered tab, through the terminal area or out of the window,
    // reports one change; later moves that hover no tab report none.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let tab = rects[0];
        let inside = Some((tab.x + tab.w / 2.0, tab.y + tab.h / 2.0));
        let terminal_y = if geometry.top_offset_px > 0.0 { 100.0 } else { 300.0 };
        let below = Some((tab.x + tab.w / 2.0, terminal_y));
        let further = Some((tab.x + tab.w / 2.0 + 30.0, terminal_y + 30.0));
        let path = [inside, below, further, None];
        let change_count = path
            .windows(2)
            .filter(|pair| hover_tab_changed(&tabs, geometry, pair[0], pair[1]))
            .count();
        assert_eq!(hovered_tab_at(&tabs, geometry, inside), 0);
        assert_eq!(change_count, 1);
        assert!(hover_tab_changed(&tabs, geometry, inside, None));
    }
}

#[test]
fn bar_space_outside_every_tab_hovers_no_tab() {
    // Empty bar space past the last tab hovers nothing, so moves inside it
    // need no frame even though they fall inside the bar's band.
    for geometry in hover_test_geometries() {
        let (tabs, rects) = hover_test_bar(geometry);
        let last = rects[rects.len() - 1];
        let empty_x = geometry.width_px - 2.0;
        assert!(last.x + last.w < empty_x, "two short tabs leave empty bar space");
        let center_y = last.y + last.h / 2.0;
        assert_eq!(hovered_tab_at(&tabs, geometry, Some((empty_x, center_y))), u32::MAX);
        assert!(!hover_tab_changed(
            &tabs,
            geometry,
            Some((empty_x, center_y)),
            Some((empty_x - 1.0, center_y)),
        ));
        let hidden = TabBarHoverGeometry { visible: false, ..geometry };
        let inside = Some((rects[0].x + 2.0, center_y));
        assert_eq!(hovered_tab_at(&tabs, hidden, inside), u32::MAX);
        assert!(!hover_tab_changed(&tabs, hidden, inside, None));
    }
}

#[test]
fn render_and_set_hover_cursor_share_one_hovered_tab_resolution() {
    // The renderer cannot be built in a unit test, so the source pins the
    // contract: both paths resolve the hovered tab through one method, and a
    // hover update never clears the frame key (the hovered tab is part of it).
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let hover_start = source.find("pub fn set_hover_cursor(").expect("set_hover_cursor");
    let hover_end = hover_start + source[hover_start..].find("\n    }\n").expect("body end");
    let hover_body = &source[hover_start..hover_end];
    assert!(hover_body.contains("tabs: &TabBar"), "callers pass their window's tab bar");
    assert!(hover_body
        .contains("self.hovered_tab_index(tabs, previous) != self.hovered_tab_index(tabs, pos)"));
    let index_start = source.find("fn hovered_tab_index(").expect("hovered_tab_index");
    let index_body = &source[index_start..index_start + 200];
    assert!(index_body.contains("hovered_tab_at(tabs, self.tab_bar_hover_geometry(), cursor)"));
    assert!(!hover_body.contains("last_frame_key"), "a hover move keeps the frame key");
    let render_start = source.find("pub fn render(").expect("render");
    let render_end =
        render_start + source[render_start..].find("fn finish_successful_frame(").expect("end");
    let render_body = &source[render_start..render_end];
    assert!(render_body.contains("self.hovered_tab_index(tabs, self.hover_cursor)"));
    assert!(!render_body.contains("t.hover_at("), "render keeps no second hit test");
    assert!(!source.contains("fn hover_change_touches_tab_bar("));
}

/// Absolute rows the cache tests seed: the slot-5 row of the scrolled-back view (102), the
/// live row 5 it is edited at (105), live row 8 (108), and live row 22 below the view (122).
const SEEDED_ABS_ROWS: [u64; 4] = [102, 105, 108, 122];

/// Plan one frame of a 24-row primary pane with 100 history rows scrolled back three rows,
/// after a baseline frame, with `dirty_live_rows` as the grid's dirty rows.
fn scrolled_back_cache_plan(dirty_live_rows: Vec<usize>) -> FramePlan {
    let frame_facts = || FrameFacts {
        window: WindowIdentity { width: 240, height: 600, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Never,
        degraded: false,
        tab_bar_top: None,
        scale: 1.0,
        previous_recolor: crate::cursor::RecolorRecord::default(),
    };
    let metadata = |revision, dirty_rows| PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 100, h: 484 },
        cols: 8,
        rows: 24,
        scrollback_len: 100,
        viewport_top_abs: Some(97),
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink: Vec::new(),
    };
    let baseline = FramePlan::build(frame_facts(), [metadata(1, Vec::new())], None);
    FramePlan::build(frame_facts(), [metadata(2, dirty_live_rows)], Some(&baseline.key))
}

/// Seed the background-quad cache with every row in `SEEDED_ABS_ROWS`, run the planned pane
/// through the quad invalidation `render_frame` makes, and report which seeded absolute rows it
/// still holds. The glyph cache is keyed by content and drops nothing for dirt.
fn surviving_cached_rows(plan: &FramePlan) -> Vec<u64> {
    let planned = &plan.panes[0];
    let mut quads = crate::row_quad_cache::LineQuadCache::new();
    quads.resize(planned.row_count);
    for row_abs in SEEDED_ABS_ROWS {
        quads.insert(planned.id, row_abs, 1, crate::row_quad_cache::CachedRowQuads::default());
    }
    invalidate_planned_quad_rows(&mut quads, planned);
    SEEDED_ABS_ROWS.into_iter().filter(|&row| quads.get(planned.id, row, 1).is_some()).collect()
}

/// The damage rectangle of one viewport slot of the scrolled-back cache plan.
fn cache_plan_slot_rect(plan: &FramePlan, slot: usize) -> PixelRect {
    let planned = &plan.panes[0];
    dirty_rows_damage_rect_with_ink_pad(
        [slot],
        planned.full_rect,
        planned.layout.x,
        planned.layout.y,
        planned.cols,
        10.0,
        20.0,
        0.0,
        240,
        600,
    )
    .expect("the slot has pixels")
}

/// A Full frame for an edit to live row 5 of a view scrolled back three rows drops the quad
/// cache's entry for absolute row 105 and damages slot 8, which draws it; it neither damages
/// slot 5 nor drops the entries for absolute row 102 (drawn at slot 5) or 108.
#[test]
fn scrolled_back_edit_invalidates_the_live_rows_absolute_entry_in_both_caches() {
    let plan = scrolled_back_cache_plan(vec![5]);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(
        plan.damage.intersect(cache_plan_slot_rect(&plan, 8)),
        Some(cache_plan_slot_rect(&plan, 8))
    );
    assert_eq!(plan.damage.intersect(cache_plan_slot_rect(&plan, 5)), None);
    assert_eq!(surviving_cached_rows(&plan), [102, 108, 122], "quad cache");
}

/// With one dirty live row drawn and one below the view, a Full frame drops both absolute
/// rows from the quad cache while its damage covers only the drawn row's slot.
#[test]
fn mixed_scrolled_back_edit_invalidates_both_rows_and_damages_one_slot() {
    let plan = scrolled_back_cache_plan(vec![5, 22]);
    assert_eq!(plan.mode, RenderMode::Full);
    assert_eq!(plan.damage, cache_plan_slot_rect(&plan, 8));
    assert_eq!(surviving_cached_rows(&plan), [102, 108], "quad cache");
}

/// The cursor is drawn when the view is live and hidden when it is scrolled back, even by
/// three rows of a 24-row view that still displays the cursor's live row 5 at slot 8: the
/// cursor follows the live top, not whether its row is in the viewport.
#[test]
fn terminal_cursor_is_drawn_only_at_the_live_view_top() {
    let (scrollback_len, rows, cursor_live_row) = (100_u64, 24_u16, 5_usize);
    let scrolled_view_top = scrollback_len - 3;
    let displayed_slot = crate::frame_plan::live_row_to_slot(
        scrollback_len,
        scrolled_view_top,
        rows,
        cursor_live_row,
    );
    assert_eq!(displayed_slot, Some(8), "the scrolled-back view still shows the cursor row");
    assert!(terminal_cursor_drawn_at_view(scrollback_len, scrollback_len), "live view");
    assert!(!terminal_cursor_drawn_at_view(scrolled_view_top, scrollback_len), "scrolled back");
}

/// The cursor draw path gates on the active view top through the tested predicate, once, and
/// the frame identity's drawn cursor cell gates through the same predicate, so neither can
/// drift from the condition the renderer applies.
#[test]
fn cursor_draw_path_calls_the_live_view_predicate() {
    let core: String = include_str!("core.rs").split_whitespace().collect();
    let gate =
        "letview_top=plan.active_view_top_abs;ifterminal_cursor_drawn_at_view(view_top,live_top){";
    assert_eq!(core.matches(gate).count(), 1, "cursor path must call the predicate");
    let identity = "&&terminal_cursor_drawn_at_view(view_top_abs,live_top);if!drawn{";
    assert_eq!(core.matches(identity).count(), 1, "the drawn cursor cell must call it");
    assert_eq!(
        core.matches("terminal_cursor_drawn_at_view(").count(),
        3,
        "definition, the draw call and the identity call"
    );
    assert_eq!(core.matches("ifview_top==live_top{").count(), 0, "no inline duplicate");
}

/// The `UploadStaging` row of `seam_classes` is exactly the vertex scratch, reported live.
///
/// That class's recorded coverage figure is the atlas staging ceiling only, and the scratch
/// has no fixed ceiling, so the row is not compared against it: a scratch larger than the
/// atlas figure is still reported as measured, once.
#[test]
fn upload_staging_row_is_the_live_vertex_scratch_not_the_atlas_figure() {
    let ClassCoverage::UnchargedRetention { per_owner_bytes: atlas_figure } =
        ResourceClass::UploadStaging.coverage()
    else {
        panic!("UploadStaging records the atlas staging ceiling");
    };
    let scratch = amount(atlas_figure + 4096, 1);
    let retention = RendererRetention { vertex_scratch: scratch, ..RendererRetention::default() };

    let rows: Vec<ResourceAmount> = retention
        .seam_classes()
        .iter()
        .filter(|(class, _)| *class == ResourceClass::UploadStaging)
        .map(|(_, part)| *part)
        .collect();

    assert_eq!(rows, vec![scratch], "one UploadStaging row, equal to the vertex scratch");
}

/// The packaged Rec Mono St.Helens stack, built the way production builds it from `assets/fonts`.
fn packaged_font_stack() -> sonicterm_engine::FontStack {
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    sonicterm_engine::FontStack::try_new_full_with_weight_and_font_dirs(
        "Rec Mono St.Helens",
        14.0,
        72,
        1.0,
        &[fonts],
    )
    .expect("the tracked packaged fonts build a stack")
}

/// A `cols`x4 grid with 8 rows of history: per-row foreground and background colours,
/// underlined cells, and non-ASCII text, so glyph position, colour and decoration all vary.
fn parity_grid(cols: u16) -> Grid {
    let mut grid = Grid::new(cols, 4);
    let text = ['a', 'B', '3', 'é', '→', 'x', 'Q', '7'];
    for row in 0..12u8 {
        if row > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for col in 0..cols as u8 {
            let foreground = Color::Rgb(250 - row * 10, 40 + col * 20, 120);
            let background = Color::Rgb(row * 20, col * 25, 90);
            let flags =
                if (row + col) % 3 == 0 { CellFlags::UNDERLINE } else { CellFlags::empty() };
            let character = text[(row as usize + col as usize) % text.len()];
            grid.put_char(character, foreground, background, flags);
        }
    }
    grid.clear_dirty();
    grid
}

/// One pointer-visible frame state: viewport top, pane origin, selection, focus, and the scale
/// the cell geometry is drawn at.
#[derive(Clone, Copy)]
struct ParityFrame {
    view_top_abs: u64,
    origin_x: f32,
    selection: Option<(u64, u64)>,
    focused: bool,
    scale: f32,
}

/// One frame's drawn output: quads, glyph instances, and the decorations kept as records.
#[derive(Debug, PartialEq)]
struct RenderedFrame {
    quads: Vec<u8>,
    glyphs: Vec<u8>,
    decorations: String,
}

/// The caches a renderer keeps between frames.
struct ParityCaches {
    glyph_rows: sonicterm_text::row_glyph_cache::RowGlyphCache,
    background_rows: crate::row_quad_cache::LineQuadCache,
}

impl ParityCaches {
    fn new() -> Self {
        Self {
            glyph_rows: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            background_rows: crate::row_quad_cache::LineQuadCache::new(),
        }
    }
}

/// Render one frame through the renderer's own steps: per-row backgrounds and glyphs (replayed
/// or shaped), the focus flash, the selection highlight, and the cursor recolour, in the
/// renderer's order. Returns the output and how many glyph and background rows replayed.
fn render_parity_frame(
    stack: &sonicterm_engine::FontStack,
    atlas: &mut GlyphAtlas,
    caches: &mut ParityCaches,
    grid: &Grid,
    frame: ParityFrame,
) -> (RenderedFrame, usize, usize) {
    let theme = Theme::default();
    let (cell_w, cell_h) = (10.0 * frame.scale, 20.0 * frame.scale);
    let baseline_y_in_cell = 16.0 * frame.scale;
    let surface = (400.0, 200.0);
    let pane_size = (f32::from(grid.cols) * cell_w, f32::from(grid.rows) * cell_h);
    let snapped = build_snapped_cell_x(frame.origin_x, cell_w, grid.cols);
    let selection = frame.selection.map(|(start, end)| selection_for_rows(start, end));
    let sel_bbox = selection.map(|sel| {
        let (lo, hi) = sel.normalized();
        (lo.0, lo.1, hi.0, hi.1)
    });
    caches.glyph_rows.begin_frame(&[(7, grid.rows, grid.cols)]);
    caches.background_rows.resize(grid.rows);
    let mut raster = stack.clone();
    let (mut quads, mut glyph_instances, mut underlines) = (Vec::new(), Vec::new(), Vec::new());
    let (mut missing_tofu, mut missing_chars, mut row_spans) = (Vec::new(), Vec::new(), Vec::new());
    let (mut glyph_replays, mut background_replays) = (0, 0);
    let background_geometry = RowBackgroundGeometry {
        origin: (frame.origin_x, 0.0),
        pane_size,
        cell_size: (cell_w, cell_h),
        surface,
        max_cols: grid.cols,
    };
    let mut shaping = GlyphShaping {
        atlas: &mut *atlas,
        row_cache: &mut caches.glyph_rows,
        font_stack: Some(stack),
        wt_raster: Some(&mut raster),
        style_rev: 0,
        theme: &theme,
        fg_default: ChromeColor::rgb(230, 230, 230),
        raster_px: 14.0,
        cell_size: (cell_w, cell_h),
        surface,
        baseline_y_in_cell,
        hovered_url_accent: [0.0; 4],
        software_presenter: false,
    };
    // Pin phase, as the production pass runs it: every row's key before any admission.
    let keys: Vec<u64> = (0..grid.rows)
        .map(|slot| emitted_row_key(&shaping, grid, frame.view_top_abs, slot, None))
        .collect();
    shaping.row_cache.pin(7, &keys);
    for slot in 0..grid.rows {
        let row = grid.row_at_abs(frame.view_top_abs + u64::from(slot)).expect("retained row");
        background_replays += usize::from(emit_row_background(
            &mut caches.background_rows,
            RowBackgroundRow { pane_id: 7, grid, view_top_abs: frame.view_top_abs, slot },
            row.iter(),
            (0, &theme, sel_bbox),
            &background_geometry,
            &snapped,
            &mut quads,
        ));
        glyph_replays += usize::from(emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id: 7,
                grid,
                view_top_abs: frame.view_top_abs,
                slot,
                origin: (frame.origin_x, 0.0),
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
                key: keys[usize::from(slot)],
            },
            GlyphFrame {
                glyph_instances: &mut glyph_instances,
                underlines: &mut underlines,
                missing_tofu: &mut missing_tofu,
                missing_chars_this_frame: &mut missing_chars,
                row_spans: &mut row_spans,
            },
        ));
        shaping.row_cache.stage_slot(7, slot, keys[usize::from(slot)]);
    }
    // Every parity frame is presented, so its slot keys commit.
    caches.glyph_rows.commit_slots();
    if frame.focused {
        // Focus chrome: the pane flash.
        quads.push(focus_flash_quad(
            [0.1, 0.1, 0.1, 1.0],
            (frame.origin_x, 0.0, pane_size.0, pane_size.1),
            0.5,
            surface,
        ));
    }
    if let Some(sel) = selection.as_ref() {
        push_selection_quads(
            &mut quads,
            sel,
            &SelectionGeometry {
                view_top_abs: frame.view_top_abs,
                grid_size: (grid.rows, grid.cols),
                origin: (frame.origin_x, 0.0),
                cell_size: (cell_w, cell_h),
                clip: (frame.origin_x, 0.0, pane_size.0, pane_size.1),
                surface,
            },
            &snapped,
            [0.2, 0.4, 0.8, 0.5],
        );
    }
    if frame.focused {
        // Focus chrome: the cursor recolours the glyph under it, after the rows were cached.
        let _ = recolor_cursor_glyphs_in(
            &mut glyph_instances,
            &row_spans,
            snapped[2],
            cell_h,
            cell_w,
            cell_h,
            surface.0,
            surface.1,
            [0.0, 0.0, 0.0, 1.0],
        );
    }
    let decorations = format!("{underlines:?} {missing_tofu:?} {missing_chars:?}");
    let rendered = RenderedFrame {
        quads: bytemuck::cast_slice(&quads).to_vec(),
        glyphs: bytemuck::cast_slice(&glyph_instances).to_vec(),
        decorations,
    };
    (rendered, glyph_replays, background_replays)
}

/// With clean grids (no row dirt), a renderer whose caches were warmed by the previous frame
/// draws each pointer operation exactly as a renderer with empty caches, at scales 1, 1.25, 1.5
/// and 2: glyph instance bytes (position, atlas region, colour, cursor recolour), underline and
/// tofu records, background, selection and focus chrome all match. Because the glyph key is the
/// row's content, every row whose cells are unchanged replays wherever it is drawn: a selection,
/// a focus change and a moved pane replay all four rows, a two-row viewport move replays the two
/// rows still shown, and only a resized pane, released with its old width, replays none.
#[test]
fn pointer_operations_render_the_same_from_warmed_caches_as_from_fresh_ones() {
    let stack = packaged_font_stack();
    let wide = parity_grid(6);
    let narrow = parity_grid(4);
    let live_top = wide.scrollback_len() as u64;
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let rest = ParityFrame {
            view_top_abs: live_top,
            origin_x: 0.0,
            selection: None,
            focused: false,
            scale,
        };
        let focused = ParityFrame { focused: true, ..rest };
        let cases = [
            // name, before frame and grid, after frame and grid, glyph and background replays.
            (
                "selection",
                (rest, &wide),
                (ParityFrame { selection: Some((live_top + 1, live_top + 1)), ..rest }, &wide),
                4,
                3,
            ),
            ("focus gained", (rest, &wide), (focused, &wide), 4, 4),
            ("focus lost", (focused, &wide), (rest, &wide), 4, 4),
            (
                "viewport",
                (rest, &wide),
                (ParityFrame { view_top_abs: live_top - 2, ..rest }, &wide),
                2,
                0,
            ),
            ("moved pane", (rest, &wide), (ParityFrame { origin_x: 120.0, ..rest }, &wide), 4, 0),
            ("resized pane", (rest, &wide), (rest, &narrow), 0, 0),
        ];
        for (name, (before, before_grid), (after, after_grid), glyph_hits, background_hits) in cases
        {
            let mut atlas = GlyphAtlas::new(1024, 1024);
            let mut warmed = ParityCaches::new();
            let (before_output, _, _) =
                render_parity_frame(&stack, &mut atlas, &mut warmed, before_grid, before);
            let (cached, glyph_replays, background_replays) =
                render_parity_frame(&stack, &mut atlas, &mut warmed, after_grid, after);
            // The forced-fresh render shares the atlas, so glyph regions keep their placement.
            let (fresh, fresh_replays, _) = render_parity_frame(
                &stack,
                &mut atlas,
                &mut ParityCaches::new(),
                after_grid,
                after,
            );
            assert!(!fresh.glyphs.is_empty(), "{name} at {scale}: the fixture draws real glyphs");
            assert_eq!(fresh_replays, 0, "{name} at {scale}: empty caches replay nothing");
            assert_eq!(cached, fresh, "{name} at {scale}: warmed caches draw what fresh ones do");
            assert_ne!(cached, before_output, "{name} at {scale}: the operation changes the frame");
            assert_eq!(
                (glyph_replays, background_replays),
                (glyph_hits, background_hits),
                "{name} at {scale}: rows whose content did not change still replay"
            );
        }
    }
}

#[test]
fn frame_texture_extent_is_one_pixel_under_the_windows_software_presenter() {
    // GDI presents from the CPU frame and never samples the wgpu frame texture, so it holds 1x1; the GPU
    // presenter needs the surface size, never zero.
    assert_eq!(frame_texture_extent(true, 1920, 1080), (1, 1));
    assert_eq!(frame_texture_extent(false, 1920, 1080), (1920, 1080));
    assert_eq!(frame_texture_extent(false, 0, 0), (1, 1));
    assert_eq!(frame_texture_payload_bytes((1, 1)), 4);
    assert_eq!(frame_texture_payload_bytes((1920, 1080)), 1920 * 1080 * 4);
}

/// The frame-texture inventory over the given sources: `create_frame_texture(` appears only in
/// `build_frame_texture`, and construction, resize, recovery prepare and the degrade switch all use it.
fn check_frame_texture_inventory(core: &str, rebind: &str, present: &str, atlas_lifecycle: &str) {
    // A CRLF checkout is read as LF, so the function-end delimiters match either way.
    let [core, rebind, present, atlas_lifecycle] =
        [core, rebind, present, atlas_lifecycle].map(|source| source.replace("\r\n", "\n"));
    let (core, rebind, present, atlas_lifecycle) =
        (core.as_str(), rebind.as_str(), present.as_str(), atlas_lifecycle.as_str());
    let sources = [
        ("core.rs", core),
        ("rebind.rs", rebind),
        ("present.rs", present),
        ("atlas_lifecycle.rs", atlas_lifecycle),
    ];
    let calls: usize =
        sources.iter().map(|(_, text)| text.matches("create_frame_texture(").count()).sum();
    // The definition and the one call inside `build_frame_texture`.
    assert_eq!(calls, 2, "create_frame_texture( outside build_frame_texture");
    let helper = core.split_once("fn build_frame_texture(").expect("helper").1;
    let helper = helper.split_once("\n}\n").unwrap().0;
    assert!(helper.contains("create_frame_texture("));
    assert!(helper.contains("frame_texture_extent("));
    // rustfmt may wrap these calls, so the checks compare text with whitespace removed.
    let squeeze = |text: &str| text.split_whitespace().collect::<String>();
    let construction = core.split_once("InitTiming::begin(\"frame_texture\")").unwrap().1;
    let construction = construction.split_once("InitTiming::finish").unwrap().0;
    assert!(squeeze(construction).contains("build_frame_texture(&device,software_presenter"));
    let resize = core.split_once("enter_gpu_work(\"try_resize\")").unwrap().1;
    assert!(resize
        .split_once("self.last_frame_key = None")
        .unwrap()
        .0
        .contains("self.rebuild_frame_texture()"));
    let degrade = core.split_once("pub fn set_software_render_degrade(").unwrap().1;
    let degrade = degrade.split_once("fn uses_windows_software_presenter(").unwrap().0;
    let branch =
        degrade.split_once("if used_software_presenter != uses_software_presenter {").unwrap().1;
    assert!(
        branch.contains("self.rebuild_frame_texture()"),
        "the degrade switch resizes the texture"
    );
    let rebuild = core.split_once("fn rebuild_frame_texture(").expect("rebuild helper").1;
    let rebuild = rebuild.split_once("\n    }\n").unwrap().0;
    assert!(
        rebuild.find("enter_gpu_work(").unwrap() < rebuild.find("build_frame_texture(").unwrap(),
        "a stopped device refuses the rebuild"
    );
    let prepare = rebind.split_once("fn prepare_rebind").unwrap().1;
    let software = prepare.find("let software_presenter =").unwrap();
    let build = prepare.find("build_frame_texture(").unwrap();
    assert!(software < build, "recovery decides the presenter before sizing the texture");
    assert!(squeeze(&prepare[build..])
        .starts_with("build_frame_texture(&context.device,software_presenter"));
    let wgpu = present.split_once("fn present_wgpu_frame(").unwrap().1;
    assert!(
        squeeze(wgpu)
            .contains("debug_assert_eq!(self.frame_texture_extent(),frame_texture_extent("),
        "the GPU presenter checks the extent"
    );
}

#[test]
fn every_frame_texture_comes_from_build_frame_texture() {
    // Construction, resize, recovery prepare and the degrade switch all size the texture through one
    // helper, so none of them can allocate the surface size under GDI. Windows CI checks sources out
    // with CRLF line ends, so the scan runs on a CRLF copy too.
    let lf = [
        include_str!("core.rs"),
        include_str!("rebind.rs"),
        include_str!("present.rs"),
        include_str!("atlas_lifecycle.rs"),
    ]
    .map(|source| source.replace("\r\n", "\n"));
    let crlf = lf.clone().map(|source| source.replace('\n', "\r\n"));
    for [core, rebind, present, atlas_lifecycle] in [&lf, &crlf] {
        check_frame_texture_inventory(core, rebind, present, atlas_lifecycle);
    }
}

#[test]
fn a_missing_glyph_draws_tofu_and_an_empty_glyph_is_skipped() {
    // The atlas caches an unresolved glyph as a missing sentinel, which the terminal draws as tofu;
    // an empty glyph such as a space keeps `missing` false and is skipped without a box.
    let empty = sonicterm_text::glyph_atlas::GlyphInfo {
        uv: [0.0; 4],
        px_size: [0, 0],
        px_offset: [0, 0],
        advance: 0.0,
        is_color: false,
        is_subpixel: false,
        missing: false,
    };
    let missing = sonicterm_text::glyph_atlas::GlyphInfo { missing: true, ..empty };
    assert_eq!(drawable_or_tofu(Some(missing)), None, "missing draws tofu");
    assert_eq!(drawable_or_tofu(None), None, "a refused glyph draws tofu");
    assert_eq!(drawable_or_tofu(Some(empty)), Some(empty), "an empty glyph is skipped, not tofu");
}

/// A rasterizer that resolves nothing, so the atlas caches a missing sentinel.
struct NoGlyphs;

impl sonicterm_text::glyph_atlas::Rasterizer for NoGlyphs {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        None
    }
}

#[test]
fn the_ascii_fast_path_draws_tofu_for_a_missing_glyph_and_skips_a_space() {
    // A printable ASCII cell whose atlas entry is a missing sentinel draws the outline box and is
    // reported missing, as the shaped path does; a space with the same sentinel draws nothing.
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut stack = crate::lib_tests::tracked_font_stack(14.0);
    let shaper = stack.clone();
    for (character, expect_box) in [('A', true), (' ', false)] {
        let mut atlas = GlyphAtlas::new(32, 32);
        let key = sonicterm_types::GlyphKey::new(character, false, false);
        assert!(atlas.get_or_insert(key, &mut NoGlyphs).unwrap().missing);
        let cell = Cell::plain(character, Color::Default, Color::Default, CellFlags::empty());
        let (mut glyphs, mut tofu, mut missing) = (Vec::new(), Vec::new(), Vec::new());
        let _complete = shape_run_for_test(
            ShapeRunFixture {
                atlas: &mut atlas,
                row: 1,
                style: RunStyle::from_cell(&cell),
                cells: &[(0, cell)],
                theme: &Theme::default(),
                fg_default: ChromeColor::rgb(255, 255, 255),
                cell_size: (10.0, 20.0),
                origin: (0.0, 4.0),
                surface: (100.0, 100.0),
                baseline_y_in_cell: 15.0,
                snapped_cell_x: &[0.0, 10.0],
                font_stack: Some(&shaper),
                wt_raster: Some(&mut stack),
                hovered_url_cells: None,
                hovered_url_accent: [0.0; 4],
                software_presenter: false,
            },
            &mut glyphs,
            &mut tofu,
            &mut missing,
        );
        assert!(glyphs.is_empty(), "{character:?} draws no tile");
        if expect_box {
            let inset = 20.0_f32 * 0.12;
            assert_eq!(tofu.len(), 1, "one outline box for {character:?}");
            let (left, top, width, height, _) = tofu[0];
            assert_eq!(
                (left, top, width, height),
                (inset, 4.0 + 20.0 + inset, 10.0 - 2.0 * inset, 20.0 - 2.0 * inset)
            );
            assert_eq!(missing, vec![character]);
        } else {
            assert!(tofu.is_empty() && missing.is_empty(), "a space is never tofu");
        }
    }
}

#[test]
fn a_real_space_passes_through_the_atlas_and_emission_without_tofu() {
    // The bundled font rasterizes a space to a valid empty tile: inserted fresh into the atlas it
    // is not missing, and terminal emission draws neither a glyph nor a tofu box for it, while a
    // printable neighbour in the same run still draws its glyph.
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut stack = crate::lib_tests::tracked_font_stack(14.0);
    let shaper = stack.clone();
    let mut atlas = GlyphAtlas::new(64, 64);
    let mut raster = stack.clone();
    let space = atlas
        .get_or_insert(sonicterm_types::GlyphKey::new(' ', false, false), &mut raster)
        .expect("the space is admitted");
    assert!(!space.missing, "a real space is empty, not missing");
    assert_eq!(space.px_size, [0, 0]);
    let cells = [
        (0, Cell::plain('A', Color::Default, Color::Default, CellFlags::empty())),
        (1, Cell::plain(' ', Color::Default, Color::Default, CellFlags::empty())),
    ];
    let (mut glyphs, mut tofu, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    let _complete = shape_run_for_test(
        ShapeRunFixture {
            atlas: &mut atlas,
            row: 0,
            style: RunStyle::from_cell(&cells[0].1),
            cells: &cells,
            theme: &Theme::default(),
            fg_default: ChromeColor::rgb(255, 255, 255),
            cell_size: (10.0, 20.0),
            origin: (0.0, 0.0),
            surface: (100.0, 100.0),
            baseline_y_in_cell: 15.0,
            snapped_cell_x: &[0.0, 10.0, 20.0],
            font_stack: Some(&shaper),
            wt_raster: Some(&mut stack),
            hovered_url_cells: None,
            hovered_url_accent: [0.0; 4],
            software_presenter: false,
        },
        &mut glyphs,
        &mut tofu,
        &mut missing,
    );
    assert_eq!(glyphs.len(), 1, "only A draws a glyph");
    assert!(tofu.is_empty(), "the space draws no tofu box");
    assert!(missing.is_empty(), "nothing is reported missing");
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
    let core = include_str!("core.rs").replace("\r\n", "\n");
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    for split in ["pub fn assemble", "present_assembled", "AssembledFrame", "pub enum Assembled"] {
        assert!(!source.contains(split), "a split API remains: {split}");
    }
    assert!(source.contains("\nenum Assembled {\n"), "Assembled is private and has no lifetime");
    let call = source.split_once("    pub fn render_releasing(").unwrap().1;
    let call = call.split_once("\n    }\n").unwrap().0;
    // The call lends through `lend_and_assemble` once, and that function lends its source once.
    assert_eq!(call.matches("lend_and_assemble(").count(), 1);
    let seam = source.split_once("\nfn lend_and_assemble(").unwrap().1;
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
    let assemble = source.split_once("    fn assemble_frame(").unwrap().1;
    let assemble = assemble.split_once("    /// Hand assembled batches").unwrap().0;
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let body = source.split_once("    pub fn take_stopped_render_outcome(").unwrap().1;
    let body = body.split_once("\n    }\n").unwrap().0;
    let finalize = body
        .find("self.finalize_growth_episodes_if_device_stopped();")
        .expect("the stop finalizes");
    let first_return = body.find("return None;").expect("the early return");
    assert!(finalize < first_return, "finalization precedes the early return");
}

/// A readback row is padded to wgpu's copy alignment, and unpadding keeps each row's pixels and
/// drops the padding, so two renderers' frames compare byte for byte whatever their padding.
#[test]
fn readback_rows_pad_to_the_copy_alignment_and_unpad_to_tight_rows() {
    assert_eq!(padded_readback_row_bytes(1), 256, "one pixel pads to one alignment unit");
    assert_eq!(padded_readback_row_bytes(64), 256, "exactly one unit needs no padding");
    assert_eq!(padded_readback_row_bytes(65), 512);
    // Two rows of one pixel each, padded to 256 bytes; the padding holds a marker byte.
    let mut mapped = vec![0xEE; 2 * 256];
    mapped[..4].copy_from_slice(&[1, 2, 3, 4]);
    mapped[256..260].copy_from_slice(&[5, 6, 7, 8]);
    assert_eq!(unpad_readback_rows(&mapped, 1, 2, 256), vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// Production never asks for a copyable retained frame: the constructor and every rebuild pass the
/// test-only readback flag, which only `__enable_retained_frame_readback` sets, and only that flag
/// adds `COPY_SRC`.
#[test]
fn only_the_test_readback_flag_makes_the_retained_frame_copyable() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let create = source.split_once("\nfn create_frame_texture(").unwrap().1;
    let create = create.split_once("\n}\n").unwrap().0;
    assert!(create.contains("if copy_source"), "COPY_SRC is conditional");
    let compact: String = source.split_whitespace().collect();
    let constructor = compact.split_once("asyncfnnew_async(").unwrap().1;
    let build =
        "build_frame_texture(&device,software_presenter,config.width,config.height,format,false,)";
    assert!(constructor.contains(build), "the constructor builds without COPY_SRC");
    assert!(source.contains("            retained_frame_readback: false,\n"));
    let enable = source.split_once("    pub fn __enable_retained_frame_readback(").unwrap().1;
    assert!(enable
        .split_once("\n    }\n")
        .unwrap()
        .0
        .contains("self.retained_frame_readback = true;"));
    assert_eq!(source.matches("self.retained_frame_readback = true;").count(), 1);
}

/// A source that owns its grids and records when it is lent and when it is dropped.
struct OwningSource {
    grids: Vec<Grid>,
    log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
}

impl sonicterm_render_model::FrameSource for OwningSource {
    fn lend<R>(
        mut self,
        assemble: impl for<'slice, 'grid> FnOnce(
            &'slice mut [sonicterm_render_model::PaneRender<'grid>],
        ) -> R,
    ) -> R {
        self.log.borrow_mut().push("lend");
        let mut panes: Vec<_> = self
            .grids
            .iter_mut()
            .enumerate()
            .map(|(index, grid)| sonicterm_render_model::PaneRender {
                id: index as u64 + 1,
                rect_px: PixelRect { x: 0, y: 0, w: 80, h: 40 },
                grid,
                viewport_top_abs: None,
                is_active: index == 0,
                cursor_style: sonicterm_render_model::CursorStyle::default(),
                is_broadcast_participant: false,
                scrollbar_alpha: 0.0,
                inline_images: Vec::new(),
            })
            .collect();
        assemble(&mut panes)
    }
}

impl Drop for OwningSource {
    // Lifecycle: dropping the source releases what it owns; the log records when.
    fn drop(&mut self) {
        self.log.borrow_mut().push("drop");
    }
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

/// A stamp at `allocation`, `identity` and `growths` on device generation 7.
fn growth_stamp(allocation: u64, identity: u64, growths: u64) -> GlyphContentStamp {
    GlyphContentStamp {
        device_generation: 7,
        allocation_generation: allocation,
        content_identity: identity,
        growths,
    }
}

/// Only a change that grew the atlas, on the same device and allocation and with no eviction,
/// takes the growth retry; an eviction, a reset or a device change takes the reset path.
#[test]
fn only_a_pure_growth_takes_the_growth_retry() {
    let before = growth_stamp(1, 4, 0);
    assert!(growth_only_change(before, growth_stamp(1, 5, 1), 3, 3), "growth alone");
    assert!(growth_only_change(before, growth_stamp(1, 7, 2), 3, 3), "two growths in one frame");
    assert!(!growth_only_change(before, growth_stamp(1, 6, 1), 3, 4), "growth with eviction");
    assert!(!growth_only_change(before, growth_stamp(2, 6, 1), 3, 3), "a reset in place");
    assert!(!growth_only_change(before, growth_stamp(1, 5, 0), 3, 4), "eviction alone");
    let other_device = GlyphContentStamp { device_generation: 8, ..growth_stamp(1, 5, 1) };
    assert!(!growth_only_change(before, other_device, 3, 3), "a new device");
}

/// `Normal` takes the scale-1 start up to 1.5 and the scale-2 start above it; `Minimum` is the floor.
#[test]
fn start_dim_follows_scale_and_start_kind() {
    use sonicterm_text::glyph_atlas::{MIN_ATLAS_DIM, START_ATLAS_DIM_1X, START_ATLAS_DIM_2X};
    for scale in [1.0, 1.25, 1.5] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Normal), START_ATLAS_DIM_1X, "{scale}");
    }
    for scale in [1.75, 2.0, 3.0] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Normal), START_ATLAS_DIM_2X, "{scale}");
    }
    for scale in [1.0, 2.0] {
        assert_eq!(start_dim(scale, GlyphAtlasStart::Minimum), MIN_ATLAS_DIM);
    }
    assert_eq!(GlyphAtlasStart::default(), GlyphAtlasStart::Normal);
}

/// Only the renderer's glyph atlas is growable: `GlyphAtlas::growable(` appears once in production
/// gpu sources, at the glyph atlas construction, and the image atlas keeps fixed constructors.
#[test]
fn only_the_glyph_atlas_is_built_growable() {
    let sources = [
        ("core.rs", include_str!("core.rs")),
        ("atlas_lifecycle.rs", include_str!("atlas_lifecycle.rs")),
        ("present.rs", include_str!("present.rs")),
        ("atlas_upload.rs", include_str!("atlas_upload.rs")),
        ("chrome_text.rs", include_str!("chrome_text.rs")),
    ]
    .map(|(name, text)| (name, text.replace("\r\n", "\n")));
    let calls: Vec<&str> = sources
        .iter()
        .flat_map(|(name, text)| text.matches("GlyphAtlas::growable(").map(move |_| *name))
        .collect();
    assert_eq!(calls, ["core.rs"]);
    let core = &sources[0].1;
    let at = core.find("GlyphAtlas::growable(").unwrap();
    assert!(
        core[..at].trim_end().ends_with("let glyph_atlas ="),
        "the call builds the glyph atlas"
    );
    let lifecycle = &sources[1].1;
    assert!(lifecycle.contains("self.image_atlas = GlyphAtlas::default_size();"));
}

/// The promoted image atlas is the fixed 2048 atlas and never grows.
#[test]
fn the_promoted_image_atlas_is_fixed_at_the_maximum() {
    let promoted = GlyphAtlas::default_size();
    assert_eq!((promoted.width(), promoted.height()), (2048, 2048));
    assert_eq!(promoted.growth_policy(), sonicterm_text::glyph_atlas::GrowthPolicy::Fixed);
    assert_eq!(promoted.growths(), 0);
}

/// The snapshot facts read the glyph atlas as it is: a grown atlas reports its new dimension,
/// its growth, the packed area and largest tile of its resident tiles, and its fit label.
#[test]
fn glyph_atlas_facts_read_a_grown_atlas() {
    use sonicterm_text::glyph_atlas::{RasterTile, ATLAS_DIM, MIN_ATLAS_DIM};
    let mut atlas = GlyphAtlas::growable(MIN_ATLAS_DIM, ATLAS_DIM);
    assert_eq!(GlyphAtlasFacts::of(&atlas).dim, MIN_ATLAS_DIM, "a fresh atlas is at its start");
    // A 30×40 coverage tile, so the packed area and largest tile are known.
    struct FactsTile;
    impl sonicterm_text::glyph_atlas::Rasterizer for FactsTile {
        fn rasterize(&mut self, _: sonicterm_types::GlyphKey) -> Option<RasterTile> {
            Some(RasterTile {
                width: 30,
                height: 40,
                offset_x: 0,
                offset_y: 0,
                advance: 30.0,
                coverage: vec![200; 30 * 40],
                is_color: false,
                is_subpixel: false,
            })
        }
    }
    let key = sonicterm_types::GlyphKey::new('x', false, false);
    let _info = atlas.get_or_insert(key, &mut FactsTile);
    assert!(atlas.grow_to(512), "the next doubling is allowed");
    let facts = GlyphAtlasFacts::of(&atlas);
    assert_eq!((facts.dim, facts.growths, facts.evictions), (512, 1, 0));
    assert_eq!((facts.packed_pixels, facts.max_tile), (30 * 40, [30, 40]));
    assert_eq!(facts.fit, atlas.fit_outcome().label());
}

/// The command-status hash changes only when the drawn badge changes: an inactive running tab
/// keeps one hash until its badge appears past five seconds, an active running tab (never badged)
/// keeps one hash throughout, a finished badge changes at its expiry, and an idle tab differs from
/// a running one so a state change still repaints once.
#[test]
fn command_status_hash_follows_only_the_drawn_badge() {
    use sonicterm_render_model::boundary::ui::tabs::CommandStatus;
    use std::time::Duration;
    let started = Instant::now();
    let running = CommandStatus::Running(started);
    let at = |seconds: u64| started + Duration::from_secs(seconds);
    let inactive_early: Vec<u64> =
        (0..=5).map(|seconds| command_status_hash(&running, at(seconds), false)).collect();
    assert!(inactive_early.iter().all(|hash| *hash == inactive_early[0]), "{inactive_early:?}");
    let inactive_late = command_status_hash(&running, at(6), false);
    assert_ne!(inactive_late, inactive_early[0], "the badge appears past five seconds");
    assert_eq!(inactive_late, command_status_hash(&running, at(30), false));
    let active: Vec<u64> =
        (0..=30).map(|seconds| command_status_hash(&running, at(seconds), true)).collect();
    assert!(active.iter().all(|hash| *hash == active[0]), "an active tab draws no badge");

    let until = at(5);
    let done = CommandStatus::Done { exit: Some(0), until };
    for is_active in [false, true] {
        assert_eq!(
            command_status_hash(&done, at(1), is_active),
            command_status_hash(&done, at(4), is_active)
        );
        assert_ne!(
            command_status_hash(&done, at(4), is_active),
            command_status_hash(&done, until, is_active),
            "the finished badge expires at `until`"
        );
    }

    let idle = command_status_hash(&CommandStatus::Idle, at(1), true);
    assert_ne!(idle, command_status_hash(&running, at(1), true), "idle and running differ");
}

/// The tab-bar hash judges each tab's badge as drawn for its activity: a running active tab
/// leaves the bar's hash unchanged as seconds pass, so no frame is planned for an invisible tick.
#[test]
fn tab_bar_hash_ignores_the_running_seconds_of_an_unbadged_tab() {
    use sonicterm_render_model::boundary::ui::tabs::{CommandStatus, Tab};
    use std::time::Duration;
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("active"));
    tabs.push(Tab::new("inactive"));
    tabs.activate(0);
    let started = Instant::now();
    tabs.set_command_status(0, CommandStatus::Running(started));
    tabs.set_command_status(1, CommandStatus::Running(started));
    let at = |seconds: u64| started + Duration::from_secs(seconds);
    let early = tab_bar_hash_with_limits(&tabs, at(1), 240.0, 320.0);
    assert_eq!(early, tab_bar_hash_with_limits(&tabs, at(4), 240.0, 320.0));
    assert_ne!(early, tab_bar_hash_with_limits(&tabs, at(6), 240.0, 320.0), "inactive badge");
}

/// The frame identity's cursor cell follows the draw's own rule: a cursor is drawn only when it
/// is visible, the window is focused, the pane is not read-only and the view is at the live top;
/// it sits at the cursor's viewport slot, and on either half of a wide character covers both.
#[test]
fn drawn_cursor_cell_follows_the_draw_condition_and_wide_span() {
    use sonicterm_render_model::boundary::grid::grid::{CellFlags, Color, Grid};
    let mut grid = Grid::new(8, 4);
    grid.linefeed();
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    let live_top = grid.scrollback_len() as u64;
    let narrow = drawn_cursor_cell(&grid, 7, live_top, true, true, false);
    assert_eq!(narrow, Some(CursorCell { pane_id: 7, slot: 1, col: 1, span: 1 }));
    for (visible, focused, read_only) in
        [(false, true, false), (true, false, false), (true, true, true)]
    {
        assert_eq!(drawn_cursor_cell(&grid, 7, live_top, visible, focused, read_only), None);
    }
    for _ in 0..6 {
        grid.linefeed();
    }
    let scrolled_live_top = grid.scrollback_len() as u64;
    assert!(scrolled_live_top > 0, "history exists to scroll back into");
    assert_eq!(drawn_cursor_cell(&grid, 7, scrolled_live_top - 1, true, true, false), None);

    let mut wide = Grid::new(8, 4);
    wide.put_char('中', Color::Default, Color::Default, CellFlags::empty());
    assert!(wide.row(0)[0].flags.contains(CellFlags::WIDE), "the lead half is marked wide");
    wide.cursor.col = 1;
    let on_trail = drawn_cursor_cell(&wide, 7, 0, true, true, false);
    assert_eq!(on_trail, Some(CursorCell { pane_id: 7, slot: 0, col: 0, span: 2 }));
    wide.cursor.col = 0;
    let on_lead = drawn_cursor_cell(&wide, 7, 0, true, true, false);
    assert_eq!(on_lead, Some(CursorCell { pane_id: 7, slot: 0, col: 0, span: 2 }));
}

/// The source of `name`'s body in `source`, from its signature to the next top-level item.
fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start + signature.len()..]
        .find("\n    pub fn ")
        .map_or(source.len(), |offset| start + signature.len() + offset);
    &source[start..end]
}

/// Cursor, blink and focus changes reach the planner as damage classes, so their setters keep
/// the retained frame key: the next frame is not a whole-surface first frame. Line endings are
/// normalized before scanning.
#[test]
fn cursor_and_focus_setters_keep_the_frame_key() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    for signature in [
        "    pub fn set_cursor_shape(",
        "    pub fn set_cursor_blink(",
        "    pub fn set_window_focused(",
    ] {
        let body = function_body(&source, signature);
        assert!(!body.contains("last_frame_key = None"), "{signature} clears the frame key");
    }
    // Blink still restarts its phase when the setting changes.
    assert!(function_body(&source, "    pub fn set_cursor_blink(").contains("self.blink_epoch ="));
}

/// Production wiring of the damage classes: the identity records the drawn cursor cell, both
/// cursor recolor sites accumulate one record, the plan's damage is widened by it before the
/// presenter reads damage, the record is kept only by a presented frame and fed back to the next
/// plan, row emission follows the coverage helper, and the scrollbar draws from the geometry the
/// planner damages.
#[test]
fn damage_classes_are_wired_through_the_renderer() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let compact: String = source.split_whitespace().collect();
    assert!(compact.contains("cursor_cell:drawn_cursor_cell("));
    assert!(compact.contains("previous_recolor:self.last_recolor,"));
    assert_eq!(compact.matches("frame_recolor=frame_recolor.merge(record);").count(), 2);
    // `assemble_frame` hands both recolor records to the shared pass-end helper, which widens
    // the damage by them before reading receipts, and the layers are built only after it.
    assert!(compact.contains("current_recolor:frame_recolor,"));
    let helper = method_body(&source, "    fn finish_assembly_pass(");
    let widen = helper
        .find("plan.widen_for_recolor(pass.previous_recolor, pass.current_recolor);")
        .expect("the helper widens by the recolors");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").expect("receipts");
    assert!(widen < receipts, "damage is widened before the receipts are read");
    let call = source.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    let layers = source.find("Ok(Assembled::Layers(Box::new(AssembledLayers {").unwrap();
    assert!(call < layers, "damage is widened before the layers carry it");
    // The record is stored after the presenter reports `Presented` and before the frame finishes.
    let present = function_body(&source, "    fn present_layers(");
    let guard = present.find("return Ok(FrameOutcome::without_receipts(outcome));").unwrap();
    let store = present.find("self.last_recolor = recolor;").expect("the record is stored");
    let finish = present.find("self.finish_successful_frame(").unwrap();
    assert!(guard < store && store < finish);
    assert_eq!(source.matches("self.last_recolor = ").count(), 1, "one writer");
    assert!(function_body(&source, "pub fn emit_pane_scrollbar(")
        .contains("crate::frame_plan::pane_scrollbar_geometry("));
}

/// Every presented frame that records its damage share also records its waste, from the same
/// final damage and the parts it unions.
#[test]
fn presented_frames_record_damage_waste_beside_damage() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let finish: String =
        function_body(&source, "    fn finish_successful_frame(").split_whitespace().collect();
    let damage = finish.find("crate::frame_stats::note_damage(||").expect("damage recorded");
    let waste = finish.find("crate::frame_stats::note_damage_waste(||").expect("waste recorded");
    assert!(damage < waste);
    assert!(finish.contains(
        "crate::frame_stats::damage_waste_permille(&plan.damage,&plan.damage_parts,surface_width,surface_height,)"
    ));
}

/// A presented frame's damage is narrow only when it is not a first frame and covers less than
/// the surface, so a whole-surface repaint can never pass a narrow-damage assertion: a first
/// frame, a surface-sized rectangle and a larger-than-surface rectangle are all not narrow.
#[test]
fn presented_damage_is_narrow_only_below_the_surface_and_after_the_first_frame() {
    let surface = PixelRect { x: 0, y: 0, w: 200, h: 100 };
    let row = PixelRect { x: 0, y: 40, w: 200, h: 24 };
    let narrow = PresentedDamage { first_frame: false, damage: row, surface };
    assert!(narrow.is_narrow());
    assert!(!PresentedDamage { first_frame: true, ..narrow }.is_narrow());
    assert!(!PresentedDamage { damage: surface, ..narrow }.is_narrow());
    let oversized = PixelRect { x: -10, y: -10, w: 400, h: 400 };
    assert!(!PresentedDamage { damage: oversized, ..narrow }.is_narrow());
}

/// The test glyph seam is appended after every terminal row and the scrollbar and before the
/// selection, copy-mode and block-cursor recolors, so those recolors see it as a real glyph; the
/// presented-damage readout is written beside the frame key, so it describes presented pixels.
/// Line endings are normalized before scanning.
#[test]
fn the_injected_glyph_precedes_every_cursor_recolor_and_damage_is_read_beside_the_key() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let assembly = source.split_once("    fn assemble_frame(").expect("assemble_frame exists").1;
    let scrollbar = assembly.find("emit_pane_scrollbar(").expect("scrollbar emit");
    let inject = assembly.find("self.push_injected_test_glyph(").expect("the seam is called");
    let selection = assembly.find("if let Some(sel) = selection {").expect("selection quads");
    let first_recolor = assembly.find("recolor_cursor_glyphs_in(").expect("a cursor recolor");
    assert!(scrollbar < inject && inject < selection && inject < first_recolor);
    let finish = source.split_once("    fn finish_successful_frame(").expect("finish exists").1;
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let assembly = source.split_once("    fn assemble_frame(").expect("assemble_frame exists").1;
    let start = assembly.find("let tab_glyph_start = glyph_instances.len();").expect("start");
    let bar = assembly.find("        if self.tab_bar_visible {\n").expect("tab bar block");
    let measure =
        assembly.find("glyph_ink_bounds(&glyph_instances[tab_glyph_start..]").expect("ink");
    let search = assembly.find("// -------- Search highlights").expect("search block");
    assert!(start < bar && bar < measure && measure < search);
    let handoff = assembly.find("previous_tab_ink: self.last_tab_ink,").expect("handed off");
    assert!(assembly.contains("current_tab_ink: tab_ink,"));
    let call = assembly.find("Self::finish_assembly_pass(&mut plan, panes, pass_end)").unwrap();
    assert!(handoff < call, "the title ink reaches the shared pass-end helper");
    let helper = method_body(&source, "    fn finish_assembly_pass(");
    let widen = helper
        .find("plan.widen_for_tab_ink(pass.previous_tab_ink, pass.current_tab_ink);")
        .expect("widen");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").expect("receipts");
    assert!(widen < receipts, "damage is widened before the layers carry it");
    let present = source.split_once("    fn present_layers(").expect("present_layers").1;
    let guard = present.find("if !matches!(outcome, PresentOutcome::Presented)").expect("guard");
    let kept = present.find("self.last_tab_ink = tab_ink;").expect("ink kept");
    assert!(guard < kept, "only a presented frame keeps its title ink");
}

/// A presented-damage recorder that was never enabled, as in production, keeps no snapshot and
/// never builds one: its builder is not called, so a presented frame pays only the branch. Once
/// enabled it keeps the last snapshot until a read takes it.
#[test]
fn a_recorder_never_enabled_keeps_no_presented_damage_snapshot() {
    let mut recorder = PresentedDamageRecorder::default();
    recorder.record(|| panic!("a disabled recorder builds no snapshot"));
    assert_eq!(recorder.take(), None);
    let surface = PixelRect { x: 0, y: 0, w: 200, h: 100 };
    let snapshot = PresentedDamage { first_frame: false, damage: surface, surface };
    recorder.enable();
    recorder.record(|| snapshot);
    assert_eq!(recorder.take(), Some(snapshot));
    assert_eq!(recorder.take(), None, "a read takes the snapshot");
}

/// The test hooks are opt-in: the renderer starts with a disabled recorder, only
/// `__enable_presented_damage` enables it, and assembly tests the injected glyph's `Option`
/// before calling the seam. Line endings are normalized before scanning.
#[test]
fn presented_damage_recording_and_glyph_injection_are_opt_in() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    assert!(source.contains("            presented_damage: PresentedDamageRecorder::default(),\n"));
    assert_eq!(source.matches("self.presented_damage.enable();").count(), 1);
    let enable = source.split_once("    pub fn __enable_presented_damage(").expect("hook").1;
    let enable = enable.split_once("\n    }\n").expect("hook body").0;
    assert!(enable.contains("self.presented_damage.enable();"));
    let assembly = source.split_once("    fn assemble_frame(").expect("assemble_frame exists").1;
    let guard = assembly.find("if self.injected_test_glyph.is_some() {").expect("Option guard");
    let call = assembly.find("self.push_injected_test_glyph(").expect("seam call");
    assert!(guard < call, "the seam is reached only when a glyph is injected");
}

/// A resize to the configured size is `Unchanged`, a new valid size `Changed`, and an
/// unrepresentable one `Rejected`; a zero size clamps to one pixel before it is compared.
#[test]
fn resize_outcome_distinguishes_changed_unchanged_and_rejected() {
    let max = MAX_SURFACE_DIMENSION;
    let same = validated_surface_size(800, 600, max);
    assert_eq!(classify_resize((800, 600), same.as_ref()), ResizeOutcome::Unchanged);
    let wider = validated_surface_size(801, 600, max);
    assert_eq!(classify_resize((800, 600), wider.as_ref()), ResizeOutcome::Changed);
    let unsafe_size = validated_surface_size(u32::MAX, u32::MAX, max);
    assert_eq!(classify_resize((800, 600), unsafe_size.as_ref()), ResizeOutcome::Rejected);
    let zero = validated_surface_size(0, 0, max);
    assert_eq!(classify_resize((1, 1), zero.as_ref()), ResizeOutcome::Unchanged);
}

/// `try_resize_outcome` returns `Unchanged` through `classify_resize` before it reconfigures the
/// surface or clears the retained key, and reports `Changed` only after the new size is applied.
#[test]
fn try_resize_outcome_returns_unchanged_before_any_reconfiguration() {
    let core = include_str!("core.rs").replace("\r\n", "\n");
    let body = core.split("pub fn try_resize_outcome(").nth(1).expect("the resize body");
    let body = &body[..body.find("\n    }\n").expect("the body ends")];
    let unchanged =
        body.find("return ResizeOutcome::Unchanged;").expect("an unchanged early return");
    assert!(body[..unchanged].contains("classify_resize("), "{body}");
    let configure = body.find("self.surface.configure(").expect("the surface is configured");
    let key = body.find("self.last_frame_key = None;").expect("the retained key is cleared");
    assert!(unchanged < configure && unchanged < key, "{body}");
    assert!(body.trim_end().ends_with("ResizeOutcome::Changed"), "{body}");
}

/// The body of the method whose signature starts at `signature`, through its closing brace at
/// method indentation.
fn method_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let end = source[start..].find("\n    }\n").map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// Row emission reads the plan's per-slot bitset: no dirty-row scan and no whole-frame flag is
/// left, and both the glyph and the background row loop test `emit_rows[` right after they
/// start. Checked on an LF and a CRLF checkout, as Windows CI checks it out.
#[test]
fn both_row_loops_emit_by_the_planned_bitset() {
    for source in
        [include_str!("core.rs").to_owned(), include_str!("core.rs").replace('\n', "\r\n")]
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
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
    let finish = method_body(&source, "    fn finish_successful_frame(");
    assert!(finish.contains("&PresentOutcome::Presented,"), "finishing settles as presented");
    let present = method_body(&source, "    fn present_layers(");
    let guard = present.find("return Ok(FrameOutcome::without_receipts(outcome));").unwrap();
    let unpresented = present.find("settle_retained_frame(").unwrap();
    let finish_call = present.find("self.finish_successful_frame(").unwrap();
    assert!(unpresented < guard && guard < finish_call, "only a presented frame finishes");
    let assemble = method_body(&source, "    fn assemble_frame(");
    let begin = assemble.find("begin_glyph_pass(").expect("a fresh stage per assembly");
    let pass_start = source.split_once("pub(crate) fn begin_glyph_pass<").expect("helper").1;
    let pass_start = &pass_start[..pass_start.find("\n}\n").expect("helper end")];
    assert!(pass_start.contains("row_ink.begin_frame();"), "the shared pass start stages afresh");
    let glyph_loop = assemble.find("assemble_pane_glyph_rows(").unwrap();
    assert!(begin < glyph_loop);
}

/// The post-assembly check runs after both widenings and before the layers carry the damage; a
/// partial plan whose final damage reaches a non-emitted row is reported back, and
/// `render_releasing` reassembles that frame `Full` in the same call.
#[test]
fn a_partial_plan_reaching_unemitted_ink_is_reassembled_full() {
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let helper = method_body(&source, "    fn finish_assembly_pass(");
    let widen = helper.find("plan.widen_for_tab_ink(").unwrap();
    let check = helper.find("plan.partial_reaches_unemitted_ink()").expect("checked");
    let receipts = helper.find("Ok(presented_receipts(plan, panes))").unwrap();
    assert!(widen < check && check < receipts);
    assert!(helper.contains("return Err(Assembled::PartialFallback);"));
    let assemble = method_body(&source, "    fn assemble_frame(");
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let start = source.find("pub(crate) fn assemble_pane_glyph_rows(").expect("pane seam");
    let pane = &source[start..start + source[start..].find("\n}\n").expect("seam end")];
    let row_loop = pane.find("    for (slot, _) in planned.rows() {").unwrap();
    let emit = row_loop + pane[row_loop..].find("emit_row_glyphs(").unwrap();
    let inject = pane.find("push_injected_row_glyph(\n").expect("the seam is pushed");
    let ink = pane.find("crate::row_ink::emitted_row_ink(").unwrap();
    assert!(row_loop < emit && emit < inject && inject < ink);
}

/// Facts for the fallback orchestration tests: a 4-row pane on a 240x160 surface, no tab bar.
fn fallback_facts() -> FrameFacts {
    FrameFacts {
        window: WindowIdentity { width: 240, height: 160, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [2.0; 4],
        vertical_ink_pad: 0.0,
        scrollbar_mode: ScrollbarMode::Never,
        degraded: false,
        tab_bar_top: None,
        scale: 1.0,
        previous_recolor: crate::cursor::RecolorRecord::default(),
    }
}

/// The 4-row pane of `fallback_facts` at `revision`, with `dirty_rows` and per-slot `row_ink`.
fn fallback_pane(
    revision: u64,
    dirty_rows: Vec<usize>,
    row_ink: Vec<Option<PixelRect>>,
) -> PaneMetadata {
    PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 100, h: 84 },
        cols: 8,
        rows: 4,
        scrollback_len: 0,
        viewport_top_abs: None,
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink,
    }
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

// ---------------------------------------------------------------------------
// Content-keyed row glyph cache: production seams driven with real fonts and plans.
// ---------------------------------------------------------------------------

/// One emitted row: whether it replayed, its key, its projected glyphs, and its decorations.
struct EmittedRow {
    replayed: bool,
    key: u64,
    glyphs: Vec<GlyphInstance>,
    decorations: String,
    missing: Vec<char>,
}

impl EmittedRow {
    /// The row's projected glyph bytes, for bit-exact comparison.
    fn glyph_bytes(&self) -> Vec<u8> {
        bytemuck::cast_slice(&self.glyphs).to_vec()
    }
}

/// A renderer's glyph state for driving `emit_row_glyphs` and `assemble_pane_glyph_rows`
/// directly: the atlas, the content-keyed cache, the fonts and the row geometry.
struct GlyphRig {
    atlas: GlyphAtlas,
    cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    stack: Option<sonicterm_engine::FontStack>,
    raster: Option<sonicterm_engine::FontStack>,
    theme: Theme,
    cell_size: (f32, f32),
    baseline_y_in_cell: f32,
    origin: (f32, f32),
    surface: (f32, f32),
    software_presenter: bool,
}

impl GlyphRig {
    /// A rig with the packaged fonts, 10x20 cells, baseline 16 and a 400x200 surface.
    fn new(software_presenter: bool) -> Self {
        Self::with_stack(packaged_font_stack(), software_presenter)
    }

    /// A rig shaping and rasterizing with `stack`, otherwise as [`GlyphRig::new`] builds one.
    fn with_stack(stack: sonicterm_engine::FontStack, software_presenter: bool) -> Self {
        Self {
            atlas: GlyphAtlas::new(1024, 1024),
            cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            raster: Some(stack.clone()),
            stack: Some(stack),
            theme: Theme::default(),
            cell_size: (10.0, 20.0),
            baseline_y_in_cell: 16.0,
            origin: (0.0, 0.0),
            surface: (400.0, 200.0),
            software_presenter,
        }
    }

    /// The rig's shaping inputs, borrowed for one call.
    fn shaping(&mut self) -> GlyphShaping<'_> {
        GlyphShaping {
            atlas: &mut self.atlas,
            row_cache: &mut self.cache,
            font_stack: self.stack.as_ref(),
            wt_raster: self.raster.as_mut(),
            style_rev: 0,
            theme: &self.theme,
            fg_default: ChromeColor::rgb(230, 230, 230),
            raster_px: 14.0,
            cell_size: self.cell_size,
            surface: self.surface,
            baseline_y_in_cell: self.baseline_y_in_cell,
            hovered_url_accent: [0.0; 4],
            software_presenter: self.software_presenter,
        }
    }

    /// Start one cache pass drawing pane 7 at `grid`'s size.
    fn begin(&mut self, grid: &Grid) {
        self.cache.begin_frame(&[(7, grid.rows, grid.cols)]);
    }

    /// Emit the row at `slot` of a view whose top is `view_top_abs`, pinning its key with the
    /// committed slots first and staging it after, as the pane pass does for one row.
    fn emit(&mut self, grid: &Grid, view_top_abs: u64, slot: u16) -> EmittedRow {
        let snapped = build_snapped_cell_x(self.origin.0, self.cell_size.0, grid.cols);
        let origin = self.origin;
        let mut shaping = self.shaping();
        let key = emitted_row_key(&shaping, grid, view_top_abs, slot, None);
        shaping.row_cache.pin(7, &[key]);
        let (mut glyphs, mut underlines, mut tofu) = (Vec::new(), Vec::new(), Vec::new());
        let (mut missing, mut spans) = (Vec::new(), Vec::new());
        let replayed = emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id: 7,
                grid,
                view_top_abs,
                slot,
                origin,
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
                key,
            },
            GlyphFrame {
                glyph_instances: &mut glyphs,
                underlines: &mut underlines,
                missing_tofu: &mut tofu,
                missing_chars_this_frame: &mut missing,
                row_spans: &mut spans,
            },
        );
        shaping.row_cache.stage_slot(7, slot, key);
        EmittedRow {
            replayed,
            key,
            glyphs,
            decorations: format!("{underlines:?} {tofu:?}"),
            missing,
        }
    }
}

/// Run `work` inside a counting scope and return its output with the counters it moved.
fn counted<Output>(work: impl FnOnce() -> Output) -> (Output, crate::frame_stats::FrameStats) {
    let sink = crate::frame_stats::FrameStatsSink::default();
    let output = {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        work()
    };
    (output, sink.snapshot())
}

/// Overwrite visible row `row` from column 0 with `text` in default colours.
fn write_row(grid: &mut Grid, row: u16, text: &str) {
    grid.goto(row, 0);
    for character in text.chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
}

/// A grid of `cols` columns holding one visible row per entry of `lines`, with no dirt.
fn text_grid(cols: u16, lines: &[&str]) -> Grid {
    let mut grid = Grid::new(cols, lines.len() as u16);
    for (row, line) in lines.iter().enumerate() {
        write_row(&mut grid, row as u16, line);
    }
    grid.clear_dirty();
    grid
}

/// A row of base characters each carrying the most combining marks a cell may hold shapes to
/// more records than its columns' share of the envelope: it is drawn but never admitted, and
/// every emission draws exactly what a cold draw does.
#[test]
fn refused_rows_draw_like_cold_rows() {
    let mut grid = Grid::new(2, 1);
    for base in ['a', 'b'] {
        grid.put_char(base, Color::Default, Color::Default, CellFlags::empty());
        // Each U+0301 is two bytes, so 32 fill the cell's 64-byte extras bound.
        for _ in 0..32 {
            grid.put_char('\u{301}', Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let limit = sonicterm_text::row_glyph_cache::row_payload_limit(2);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let first = rig.emit(&grid, 0, 0);
    assert!(
        first.glyphs.len() * std::mem::size_of::<sonicterm_text::row_glyph_cache::RowGlyph>()
            > limit,
        "the fixture's records exceed the row envelope: {} glyphs",
        first.glyphs.len()
    );
    assert!(!first.replayed && !rig.cache.contains(7, first.key), "an oversized row is refused");
    let second = rig.emit(&grid, 0, 0);
    assert!(!second.replayed, "a refused row is shaped again");
    assert_eq!(second.glyph_bytes(), first.glyph_bytes(), "and draws what a cold draw does");
}

/// At the history cap a scroll shifts every absolute row, yet only the new row reshapes: the
/// three rows that moved one slot up replay with no shape request, and the unique shaping row
/// written after the linefeed (the positive control) is shaped. A wheel move starts cold.
#[test]
fn scrolling_one_line_at_the_history_cap_reshapes_only_the_new_row() {
    let mut grid = Grid::new(8, 4);
    grid.set_scrollback_limit(4);
    for index in 0..10 {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in format!("r{index}=>é").chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    assert_eq!(grid.scrollback_len(), 4);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let top = grid.scrollback_len() as u64;
    for slot in 0..4 {
        rig.emit(&grid, top, slot);
    }
    rig.cache.commit_slots();
    grid.carriage_return();
    grid.linefeed();
    for character in "r10=>é".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    assert_eq!(grid.scrollback_len(), 4, "the history stayed at its cap");
    rig.begin(&grid);
    let top = grid.scrollback_len() as u64;
    for slot in 0..4u16 {
        let (row, stats) = counted(|| rig.emit(&grid, top, slot));
        if slot < 3 {
            assert!(row.replayed, "slot {slot} moved up and replays");
            assert_eq!(stats.shape_requests, 0, "slot {slot} is not shaped");
        } else {
            assert!(!row.replayed, "the new row misses");
            assert!(stats.shape_requests >= 1, "the new row is shaped");
        }
    }

    // A wheel move up one row from a cold cache and history: three rows replay one slot lower.
    let mut grid = Grid::new(8, 4);
    for index in 0..8 {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in format!("w{index}=>é").chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let mut rig = GlyphRig::new(false);
    let live = grid.scrollback_len() as u64;
    rig.begin(&grid);
    for slot in 0..4 {
        rig.emit(&grid, live, slot);
    }
    rig.cache.commit_slots();
    rig.begin(&grid);
    let replays: Vec<bool> = (0..4).map(|slot| rig.emit(&grid, live - 1, slot).replayed).collect();
    assert_eq!(replays, [false, true, true, true]);
}

/// The inputs one frozen oracle instance reads.
struct OracleCell {
    snapped: Vec<f32>,
    cell_size: (f32, f32),
    top_inset: f32,
    row: u16,
    baseline_y_in_cell: f32,
    surface: (f32, f32),
}

/// Frozen, test-local copy of the pre-record status-marker predicate and fit arithmetic. It is
/// deliberately independent of production, so a change to either the predicate or the fit is
/// caught by `record_projection_matches_the_frozen_emission_geometry`.
fn frozen_fit_single_cell_status_marker(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    if !matches!(ch, '\u{23fa}' | '\u{25ef}' | '\u{25cf}')
        || cluster_cells != 1
        || is_wide
        || has_extras
    {
        return natural;
    }
    let (_, _, glyph_w, glyph_h) = natural;
    let (cell_x, cell_y, cell_w, cell_h) = cell;
    if glyph_w <= 0.0 || glyph_h <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return natural;
    }
    let scale = (cell_w / glyph_w).min(cell_h / glyph_h);
    let fitted_w = glyph_w * scale;
    let fitted_h = glyph_h * scale;
    (cell_x + (cell_w - fitted_w) * 0.5, cell_y + (cell_h - fitted_h) * 0.5, fitted_w, fitted_h)
}

/// Frozen copy of the pre-record HarfBuzz placement: offsets move the tile, never resize it.
fn frozen_positioned_shaped_glyph_rect(
    natural: (f32, f32, f32, f32),
    x_offset: f32,
    y_offset: f32,
) -> (f32, f32, f32, f32) {
    (natural.0 + x_offset, natural.1 + y_offset, natural.2, natural.3)
}

/// Frozen copy of the pre-record software block target: integer edges, at least one pixel.
fn frozen_software_block_glyph_target_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> (f32, f32, f32, f32) {
    let left = (left - 0.5).ceil();
    let top = (top - 0.5).ceil();
    let right = (right - 0.5).ceil().max(left + 1.0);
    let bottom = (bottom - 0.5).ceil().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

/// Frozen copy of the pre-record instance flags: colour in x, subpixel coverage in y.
fn frozen_glyph_flags(is_color: bool, is_subpixel: bool) -> [f32; 4] {
    [if is_color { 1.0 } else { 0.0 }, if is_subpixel { 1.0 } else { 0.0 }, 0.0, 0.0]
}

/// The whole instance the pre-record renderer pushed for `rect` from `info` in `rgba`.
fn frozen_instance(
    cell: &OracleCell,
    rect: (f32, f32, f32, f32),
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let (glyph_x, glyph_y, glyph_w, glyph_h) = rect;
    GlyphInstance {
        rect: px_to_ndc(glyph_x, glyph_y, glyph_w, glyph_h, cell.surface.0, cell.surface.1),
        uv: info.uv,
        color: rgba,
        flags: frozen_glyph_flags(info.is_color, info.is_subpixel),
    }
}

/// Frozen copy of the pre-record ASCII emission.
fn oracle_natural(
    cell: &OracleCell,
    col: u16,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell.cell_size.1;
    let inv_s = 1.0_f32;
    let glyph_x = cell_left_px + info.px_offset[0] as f32 * inv_s;
    let glyph_y = cell_top_px + cell.baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
    let glyph_w = info.px_size[0] as f32 * inv_s;
    let glyph_h = info.px_size[1] as f32 * inv_s;
    let rect = sonicterm_render_model::geometry::snap_to_device_pixels(
        (glyph_x, glyph_y, glyph_w, glyph_h),
        1.0,
    );
    frozen_instance(cell, rect, info, rgba)
}

/// Frozen copy of the pre-record fallback and shaped emission.
fn oracle_shaped(
    cell: &OracleCell,
    lead: (u16, char, usize, bool, bool),
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    shape_offset: (f32, f32),
    rgba: [f32; 4],
) -> GlyphInstance {
    let (col, character, cluster_cells, is_wide, has_extras) = lead;
    let (cell_w, cell_h) = cell.cell_size;
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell_h;
    let inv_s = 1.0_f32;
    let glyph_x = cell_left_px + info.px_offset[0] as f32 * inv_s;
    let glyph_y = cell_top_px + cell.baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
    let glyph_w = info.px_size[0] as f32 * inv_s;
    let glyph_h = info.px_size[1] as f32 * inv_s;
    let positioned = frozen_positioned_shaped_glyph_rect(
        (glyph_x, glyph_y, glyph_w, glyph_h),
        shape_offset.0,
        shape_offset.1,
    );
    let cell_right = cell.snapped.get(col as usize + 1).copied().unwrap_or(cell_left_px + cell_w);
    let fitted = frozen_fit_single_cell_status_marker(
        character,
        cluster_cells,
        is_wide,
        has_extras,
        positioned,
        (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h),
    );
    let rect = sonicterm_render_model::geometry::snap_to_device_pixels(fitted, 1.0);
    frozen_instance(cell, rect, info, rgba)
}

/// Frozen copy of the pre-record block emission.
fn oracle_block(
    cell: &OracleCell,
    col: u16,
    span: usize,
    software_presenter: bool,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> GlyphInstance {
    let cell_h = cell.cell_size.1;
    let cell_left_px = cell.snapped[col as usize];
    let cell_top_px = cell.top_inset + f32::from(cell.row) * cell_h;
    let end_col = (col as usize + span).min(cell.snapped.len() - 1);
    let cell_right = cell.snapped[end_col];
    let rect = if software_presenter {
        let cell_bottom = cell.top_inset + (f32::from(cell.row) + 1.0) * cell_h;
        frozen_software_block_glyph_target_rect(cell_left_px, cell_top_px, cell_right, cell_bottom)
    } else {
        (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h)
    };
    frozen_instance(cell, rect, info, rgba)
}

/// One instance's bytes, so comparisons are bit-exact rather than f32 equality.
fn instance_bytes(instance: &GlyphInstance) -> Vec<u8> {
    bytemuck::bytes_of(instance).to_vec()
}

/// Projecting position-free records reproduces the frozen pre-record emission byte for byte,
/// whole instances included, for every kind: separate raster and shaping offsets (including
/// 1.9, 7 and 8.7), a small surface, a fractional origin and pitch, status-marker eligibility,
/// wide and combining lead cells, colour (emoji-style) and subpixel tiles, and blocks spanning
/// one and two columns on both presenters. The oracle is independent of production arithmetic.
#[test]
fn record_projection_matches_the_frozen_emission_geometry() {
    use sonicterm_text::glyph_atlas::GlyphInfo;
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let info = |offset: [i32; 2], size: [u32; 2], is_color: bool| GlyphInfo {
        uv: [0.1, 0.2, 0.3, 0.4],
        px_size: size,
        px_offset: offset,
        advance: 0.0,
        is_color,
        is_subpixel: !is_color,
        missing: false,
    };
    let geometries = [
        ((0.0, 0.0), (10.0, 20.0), 16.0, (400.0, 200.0)),
        ((1.9, 7.0), (8.7, 17.3), 13.84, (80.0, 80.0)),
        ((0.5, 0.1), (10.5, 20.25), 16.2, (123.0, 77.0)),
    ];
    let mut compared = 0;
    for (origin, cell_size, baseline, surface) in geometries {
        for row in [0u16, 1, 3] {
            let cell = OracleCell {
                snapped: build_snapped_cell_x(origin.0, cell_size.0, 6),
                cell_size,
                top_inset: origin.1,
                row,
                baseline_y_in_cell: baseline,
                surface,
            };
            let at = RowPlacement {
                slot: row,
                origin,
                cols: 6,
                snapped_cell_x: &cell.snapped,
                cell_size,
                baseline_y_in_cell: baseline,
                surface,
            };
            for is_color in [false, true] {
                let rgba = if is_color { [1.0; 4] } else { [0.3, 0.6, 0.9, 1.0] };
                let tile = info([1, -12], [7, 13], is_color);
                let natural = tile_record(RowGlyphKind::Natural, 2, &tile, rgba);
                assert_eq!(
                    instance_bytes(&project_row_glyph(&natural, &at, false)),
                    instance_bytes(&oracle_natural(&cell, 2, &tile, rgba)),
                    "natural colour={is_color}"
                );
                compared += 1;
                let leads = [
                    ('\u{25cf}', 1usize, false, false),
                    ('\u{23fa}', 1, false, false),
                    ('\u{25ef}', 1, false, false),
                    ('\u{25cf}', 1, true, false),
                    ('\u{25cf}', 1, false, true),
                    ('\u{25cf}', 2, false, false),
                    ('x', 1, false, false),
                    ('😀', 2, true, false),
                ];
                for (character, cluster_cells, is_wide, has_extras) in leads {
                    for kind in [RowGlyphKind::Fallback, RowGlyphKind::Shaped] {
                        for shape_offset in [(0.0, 0.0), (1.9, -7.0), (8.7, 2.5)] {
                            let tile = info([2, -9], [9, 9], is_color);
                            let mut record = tile_record(kind, 3, &tile, rgba);
                            let mut lead_cell = Cell::plain(
                                character,
                                Color::Default,
                                Color::Default,
                                CellFlags::empty(),
                            );
                            if is_wide {
                                lead_cell.flags = CellFlags::WIDE;
                            }
                            if has_extras {
                                lead_cell.set_extras(Some("\u{301}".into()));
                            }
                            let shaped = sonicterm_text::shape::ShapedGlyph {
                                lead_col: 3,
                                cluster_cells: cluster_cells as u16,
                                font_slot: 0,
                                glyph_id: 5,
                                x_advance: 0.0,
                                x_offset: 0.0,
                                y_offset: 0.0,
                                ch: character,
                            };
                            let eligible = status_marker_fit_eligible(
                                character,
                                cluster_cells,
                                is_wide,
                                has_extras,
                            );
                            set_shaped_bits(
                                &mut record,
                                [shape_offset.0, shape_offset.1],
                                eligible,
                                &shaped,
                                is_wide,
                                &lead_cell,
                            );
                            let expected = oracle_shaped(
                                &cell,
                                (3, character, cluster_cells, is_wide, has_extras),
                                &tile,
                                shape_offset,
                                rgba,
                            );
                            assert_eq!(
                                instance_bytes(&project_row_glyph(&record, &at, false)),
                                instance_bytes(&expected),
                                "{kind:?} {character:?} cells={cluster_cells} wide={is_wide} \
                                 extras={has_extras} offset={shape_offset:?} colour={is_color}"
                            );
                            compared += 1;
                        }
                    }
                }
                for span in [1usize, 2] {
                    for software in [false, true] {
                        let tile = info([0, 0], [10, 20], is_color);
                        let mut record = tile_record(RowGlyphKind::Block, 4, &tile, rgba);
                        record.raster_offset = [0.0; 2];
                        record.end_col = (4 + span).min(cell.snapped.len() - 1) as u16;
                        assert_eq!(
                            instance_bytes(&project_row_glyph(&record, &at, software)),
                            instance_bytes(&oracle_block(&cell, 4, span, software, &tile, rgba)),
                            "block span {span} software {software} colour={is_color}"
                        );
                        compared += 1;
                    }
                }
            }
        }
    }
    assert_eq!(compared, 3 * 3 * 2 * (1 + 8 * 2 * 3 + 2 * 2), "every case was compared");
}

/// The tracked font faces only (no OS font discovery), warm and cold, draw the same bytes. The
/// grid covers normal, bold, italic and bold-italic runs, a ligature trigger, box drawing, a
/// combining cluster, and CJK, emoji and wide clusters. Fallback is controlled: the stack has no
/// system font source, so CJK draws from the tracked faces and the emoji is a stable missing
/// glyph. Exactly these rows are drawn once to
/// warm the atlas and fonts; then only the row cache is cleared, a cold pass misses every row,
/// and a warm pass hits every row with identical glyphs, decorations and missing characters.
#[test]
fn tracked_fonts_draw_the_same_warm_and_cold() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let styles = [
        CellFlags::empty(),
        CellFlags::BOLD,
        CellFlags::ITALIC,
        CellFlags::BOLD | CellFlags::ITALIC,
    ];
    let mut grid = Grid::new(12, 8);
    for (row, flags) in styles.iter().enumerate() {
        grid.goto(row as u16, 0);
        for character in "ab=>c->d".chars() {
            grid.put_char(character, Color::Default, Color::Default, *flags);
        }
    }
    write_row(&mut grid, 4, "│─┼█▌ e");
    grid.put_char('\u{301}', Color::Default, Color::Default, CellFlags::empty());
    write_row(&mut grid, 5, "中文 x");
    write_row(&mut grid, 6, "😀 ok");
    write_row(&mut grid, 7, "a中😀b");
    grid.clear_dirty();
    let rows = grid.rows;
    let mut rig = GlyphRig::with_stack(crate::lib_tests::tracked_font_stack(14.0), false);
    rig.surface = (400.0, 400.0);
    rig.begin(&grid);
    for slot in 0..rows {
        rig.emit(&grid, 0, slot);
    }
    rig.cache.invalidate_all();
    rig.begin(&grid);
    let (cold, cold_stats) =
        counted(|| (0..rows).map(|slot| rig.emit(&grid, 0, slot)).collect::<Vec<_>>());
    rig.begin(&grid);
    let (warm, warm_stats) =
        counted(|| (0..rows).map(|slot| rig.emit(&grid, 0, slot)).collect::<Vec<_>>());
    let row_count = u64::from(rows);
    assert_eq!((cold_stats.row_cache_hits, cold_stats.row_cache_misses), (0, row_count));
    assert_eq!((warm_stats.row_cache_hits, warm_stats.row_cache_misses), (row_count, 0));
    assert_eq!(warm_stats.shape_requests, 0, "the warm pass shapes nothing");
    for (slot, (cold_row, warm_row)) in cold.iter().zip(&warm).enumerate() {
        assert!(!cold_row.glyphs.is_empty() || !cold_row.missing.is_empty(), "slot {slot} draws");
        assert_eq!(warm_row.glyph_bytes(), cold_row.glyph_bytes(), "slot {slot} glyph bytes");
        assert_eq!(warm_row.decorations, cold_row.decorations, "slot {slot} decorations");
        assert_eq!(warm_row.missing, cold_row.missing, "slot {slot} missing characters");
    }
    let style_bytes: Vec<Vec<u8>> = cold[..4].iter().map(EmittedRow::glyph_bytes).collect();
    for (index, bytes) in style_bytes.iter().enumerate().skip(1) {
        assert_ne!(*bytes, style_bytes[0], "style row {index} draws other faces than normal");
    }
    // `ConfigDirsOnly` installs no system source, so CJK draws from the tracked face's own
    // coverage and the emoji, which no tracked face has, is a stable missing glyph.
    assert!(
        !cold[5].glyphs.is_empty() && cold[5].missing.is_empty(),
        "CJK draws from tracked faces"
    );
    assert!(cold[6].missing.contains(&'😀'), "emoji resolves to the controlled missing glyph");
}

/// A pane left untracked by a zero tracking budget still draws: every row emitted through the
/// renderer seam equals a cold draw from a tracked cache, glyphs, decorations and missing
/// characters alike, and nothing is cached for it.
#[test]
fn untracked_panes_draw_like_cold_rows() {
    let grid = text_grid(10, &["ab=>c", "│─┼█", "plain", "→é x"]);
    let mut untracked = GlyphRig::new(false);
    untracked.cache = sonicterm_text::row_glyph_cache::RowGlyphCache::with_budgets(
        sonicterm_text::row_glyph_cache::DEFAULT_PAYLOAD_BUDGET_BYTES,
        0,
    );
    untracked.begin(&grid);
    assert!(!untracked.cache.is_tracked(7), "a zero tracking budget leaves the pane untracked");
    let mut cold = GlyphRig::new(false);
    cold.begin(&grid);
    for slot in 0..4 {
        let drawn = untracked.emit(&grid, 0, slot);
        let reference = cold.emit(&grid, 0, slot);
        assert!(!drawn.replayed, "slot {slot}: an untracked pane never replays");
        assert!(!reference.glyphs.is_empty(), "slot {slot}: the reference draws glyphs");
        assert_eq!(drawn.glyph_bytes(), reference.glyph_bytes(), "slot {slot} glyph bytes");
        assert_eq!(drawn.decorations, reference.decorations, "slot {slot} decorations");
        assert_eq!(drawn.missing, reference.missing, "slot {slot} missing characters");
    }
    assert!(untracked.cache.is_empty() && untracked.cache.retained_amount().bytes == 0);
}

/// The glyph atlas and the cold and warm glyph lists of one scrolled frame on the software
/// presenter at a fractional pitch: the warm list replays rows cached at other slots, and both
/// lists are returned with the frame's surface size, for compositor parity tests.
pub(crate) fn warm_and_cold_row_glyphs(
) -> (GlyphAtlas, Vec<GlyphInstance>, Vec<GlyphInstance>, (u32, u32)) {
    let mut grid = Grid::new(8, 4);
    let lines = ["ab=>c", "│─┼█", "→ é x", "plain", "▌▐ AB", "q->r", "last", "zz"];
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            grid.carriage_return();
            grid.linefeed();
        }
        for character in line.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let mut rig = GlyphRig::new(true);
    rig.atlas = GlyphAtlas::new(512, 512);
    rig.cell_size = (10.5, 20.3);
    rig.baseline_y_in_cell = 16.2;
    rig.origin = (0.5, 0.1);
    rig.surface = (85.0, 82.0);
    let live = grid.scrollback_len() as u64;
    let scrolled = live - 2;
    let frame = |rig: &mut GlyphRig, top: u64| -> Vec<EmittedRow> {
        rig.begin(&grid);
        let rows = (0..4).map(|slot| rig.emit(&grid, top, slot)).collect();
        rig.cache.commit_slots();
        rows
    };
    frame(&mut rig, scrolled);
    let warm = frame(&mut rig, live);
    assert!(warm.iter().any(|row| row.replayed), "the warm frame replays moved rows");
    rig.cache.invalidate_all();
    let cold = frame(&mut rig, live);
    assert!(cold.iter().all(|row| !row.replayed), "the cold frame shapes every row");
    let flatten = |rows: Vec<EmittedRow>| rows.into_iter().flat_map(|row| row.glyphs).collect();
    (rig.atlas, flatten(cold), flatten(warm), (85, 82))
}

/// Warm and cold glyph lists of the scrolled software frame are byte-identical.
#[test]
fn warm_and_cold_scrolled_frames_emit_identical_glyphs() {
    let (_atlas, cold, warm, _size) = warm_and_cold_row_glyphs();
    assert!(!cold.is_empty());
    assert_eq!(bytemuck::cast_slice::<_, u8>(&warm), bytemuck::cast_slice::<_, u8>(&cold));
}

/// The plan fixture of the cache policy tests: one primary pane of `rows` rows by 8 columns at
/// layout and origin y 0, padding 0, 10x20 cells and `vertical_ink_pad`, cursor hidden, no
/// overlay, not degraded.
fn policy_plan(
    rows: u16,
    vertical_ink_pad: f32,
    revision: u64,
    dirty_rows: Vec<usize>,
    row_ink: Vec<Option<PixelRect>>,
    previous: Option<&FrameKey>,
) -> FramePlan {
    let facts = FrameFacts {
        window: WindowIdentity { width: 240, height: 200, ..Default::default() },
        cell_w: 10.0,
        cell_h: 20.0,
        padding: [0.0; 4],
        vertical_ink_pad,
        scrollbar_mode: ScrollbarMode::Never,
        degraded: false,
        tab_bar_top: None,
        scale: 1.0,
        previous_recolor: crate::cursor::RecolorRecord::default(),
    };
    let pane = PaneMetadata {
        id: 7,
        revision,
        dirty_generation: 0,
        rect: PixelRect { x: 0, y: 0, w: 80, h: u32::from(rows) * 20 },
        cols: 8,
        rows,
        scrollback_len: 0,
        viewport_top_abs: None,
        is_active: true,
        is_alt: false,
        scrollbar_alpha: 0.0,
        dirty_rows,
        row_ink,
    };
    FramePlan::build(facts, [pane], previous)
}

/// Each slot's committed ink set to its own strip `[20 s, 20 s + 20)`.
fn strip_records(rows: u16) -> Vec<Option<PixelRect>> {
    (0..i32::from(rows)).map(|slot| Some(PixelRect { x: 0, y: 20 * slot, w: 80, h: 20 })).collect()
}

/// Assemble one pass of `plan`'s pane through the production pane seam, after the pass's
/// `begin_frame`, returning the glyphs and the counters the pass moved.
fn assemble_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats) {
    begin_pass(rig, ink, grid, plan);
    emit_pass(rig, ink, grid, plan)
}

/// Start one assembly pass through `begin_glyph_pass`, the helper `assemble_frame` starts every
/// pass with, so the fixture exercises production's pass start rather than a copy of it.
fn begin_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) {
    begin_glyph_pass(&mut rig.cache, ink, plan.panes.iter().map(|planned| (planned, grid.cols)));
}

/// Emit `plan`'s pane rows through the production pane seam into a pass already begun,
/// returning the glyphs and the counters the emission moved.
fn emit_pass(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats) {
    let (glyphs, stats, _slots) = emit_pass_recording(rig, ink, grid, plan);
    (glyphs, stats)
}

/// [`emit_pass`], also returning the slots the pane seam emitted, in emission order, as its
/// recording sink saw them at each `emit_row_glyphs` call.
fn emit_pass_recording(
    rig: &mut GlyphRig,
    ink: &mut crate::row_ink::RowInkTable,
    grid: &Grid,
    plan: &FramePlan,
) -> (Vec<GlyphInstance>, crate::frame_stats::FrameStats, Vec<u16>) {
    let planned = &plan.panes[0];
    let mut emitted_slots = Vec::new();
    let snapped = build_snapped_cell_x(planned.layout.x, rig.cell_size.0, grid.cols);
    let (mut glyphs, mut underlines, mut tofu) = (Vec::new(), Vec::new(), Vec::new());
    let (mut missing, mut spans, mut owners) = (Vec::new(), Vec::new(), Vec::new());
    let ((), stats) = counted(|| {
        assemble_pane_glyph_rows(
            rig.shaping(),
            PaneGlyphRows {
                pane_id: planned.id,
                grid,
                planned,
                origin: (planned.layout.x, planned.layout.y),
                snapped_cell_x: &snapped,
                pane_hovered_url: None,
            },
            GlyphFrame {
                glyph_instances: &mut glyphs,
                underlines: &mut underlines,
                missing_tofu: &mut tofu,
                missing_chars_this_frame: &mut missing,
                row_spans: &mut spans,
            },
            PaneGlyphSinks {
                row_ink: ink,
                ink_surface: plan.surface,
                underline_owners: &mut owners,
                injected_row_glyph: None,
                emitted_slots: Some(&mut emitted_slots),
                row_keys: &mut Vec::new(),
            },
        )
    });
    (glyphs, stats, emitted_slots)
}

/// Settle a pass as presented: commit its staged glyph slot keys and ink records.
fn present_pass(rig: &mut GlyphRig, ink: &mut crate::row_ink::RowInkTable, plan: &FramePlan) {
    rig.cache.commit_slots();
    ink.commit(&plan.drawn_row_counts());
}

/// Presentation-only dirt (every row marked dirty with unchanged cells) replays every row
/// through the production pane seam with no shaping, and the glyph cache's invalidation
/// counters stay 0 because nothing drops a glyph row for dirt.
#[test]
fn presentation_only_dirt_replays_through_the_production_seam() {
    let mut grid = text_grid(8, &["p0=>é", "p1=>é", "p2=>é", "p3=>é"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let first = policy_plan(4, 0.0, 1, Vec::new(), Vec::new(), None);
    let (_, warm) = assemble_pass(&mut rig, &mut ink, &grid, &first);
    assert_eq!(warm.row_cache_misses, 4);
    present_pass(&mut rig, &mut ink, &first);
    grid.mark_all_dirty();
    let dirty: Vec<usize> = grid.dirty_rows().collect();
    let second = policy_plan(4, 0.0, 2, dirty, Vec::new(), Some(&first.key));
    assert!(second.panes[0].emit_rows.iter().all(|emitted| *emitted));
    let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &second);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (4, 0), "every row replays");
    assert_eq!(stats.shape_requests, 0, "nothing is shaped again");
    assert_eq!((stats.row_cache_invalidate_visits, stats.row_cache_invalidate_us), (0, 0));
}

/// A software-presenter block row is accepted only where every block still rasterizes at its
/// stored size: at a fractional cell height two slots with different integer heights miss, and
/// the second's shape replaces the entry; a slot with the second's height then hits. A
/// half-pixel origin at a fractional pitch changes a block's width and misses. GPU blocks and
/// ASCII rows hit at any slot.
#[test]
fn software_block_rows_hit_only_where_their_size_holds() {
    let block_rows: Vec<&str> = vec!["██"; 64];
    let grid = text_grid(2, &block_rows);
    let height_at = |cell_h: f32, top_inset: f32, slot: u16| {
        let top = top_inset + f32::from(slot) * cell_h;
        let bottom = top_inset + (f32::from(slot) + 1.0) * cell_h;
        software_block_glyph_target_rect(0.0, top, 10.0, bottom).3
    };
    let (cell_h, top_inset) = (7.3, 0.1);
    let first_slot = 0u16;
    let other = (1..64)
        .find(|slot| {
            height_at(cell_h, top_inset, *slot) != height_at(cell_h, top_inset, first_slot)
        })
        .unwrap();
    let same_as_other = (0..64)
        .find(|slot| {
            *slot != other
                && height_at(cell_h, top_inset, *slot) == height_at(cell_h, top_inset, other)
        })
        .unwrap();
    let mut rig = GlyphRig::new(true);
    rig.cell_size = (10.0, cell_h);
    rig.origin = (0.0, top_inset);
    rig.surface = (40.0, 480.0);
    rig.begin(&grid);
    assert!(!rig.emit(&grid, 0, first_slot).replayed, "the first draw is shaped");
    assert!(!rig.emit(&grid, 0, other).replayed, "another integer height misses");
    assert!(rig.emit(&grid, 0, same_as_other).replayed, "the replacement fits its own height");
    assert!(!rig.emit(&grid, 0, first_slot).replayed, "and no longer fits the first height");

    let mut rig = GlyphRig::new(true);
    rig.cell_size = (10.5, 20.0);
    rig.surface = (40.0, 480.0);
    let width_at = |origin_x: f32| {
        let snapped = build_snapped_cell_x(origin_x, 10.5, 2);
        software_block_glyph_target_rect(snapped[0], 0.0, snapped[1], 20.0).2
    };
    assert_ne!(width_at(0.0), width_at(0.5), "the fixture changes a block's width");
    rig.begin(&grid);
    rig.emit(&grid, 0, 0);
    rig.origin = (0.5, 0.0);
    assert!(!rig.emit(&grid, 0, 0).replayed, "a half-pixel origin changes the width");
    assert!(rig.emit(&grid, 0, 0).replayed, "the same width hits");

    let mut rig = GlyphRig::new(false);
    rig.cell_size = (10.0, cell_h);
    rig.origin = (0.0, top_inset);
    rig.surface = (40.0, 480.0);
    rig.begin(&grid);
    rig.emit(&grid, 0, first_slot);
    assert!(rig.emit(&grid, 0, other).replayed, "GPU blocks hit at any slot");
    let ascii = text_grid(8, &["text", "text"]);
    let mut rig = GlyphRig::new(true);
    rig.begin(&ascii);
    rig.emit(&ascii, 0, 0);
    assert!(rig.emit(&ascii, 0, 1).replayed, "ASCII rows hit at any slot");
}

/// A block that drew nothing is never cached: on the software presenter at cell height 2048.4
/// and top 0.1 the block's target is 2049 pixels at slot 1, past a 2048 atlas, so it is
/// skipped and the row is not admitted; at slot 0 the target is 2048, so the row misses and
/// draws the block as a cold draw does. A glyph the atlas refuses because its entry index is
/// full with eviction barred keeps its row out of the cache too, while drawing tofu.
#[test]
fn blocks_that_drew_nothing_are_never_cached() {
    let height_at = |slot: u16| {
        let top = 0.1 + f32::from(slot) * 2048.4;
        software_block_glyph_target_rect(0.0, top, 10.0, top + 2048.4).3
    };
    assert_eq!((height_at(0), height_at(1)), (2048.0, 2049.0));
    let grid = text_grid(2, &["█", "█"]);
    let mut rig = GlyphRig::new(true);
    rig.atlas = GlyphAtlas::new(2048, 2048);
    rig.cell_size = (10.0, 2048.4);
    rig.origin = (0.0, 0.1);
    rig.surface = (40.0, 4200.0);
    rig.begin(&grid);
    let skipped = rig.emit(&grid, 0, 1);
    assert!(skipped.glyphs.is_empty(), "the block drew nothing at slot 1");
    assert!(!rig.cache.contains(7, skipped.key), "and its row was not admitted");
    let drawn = rig.emit(&grid, 0, 0);
    assert!(!drawn.replayed && !drawn.glyphs.is_empty(), "slot 0 misses and draws the block");
    rig.cache.invalidate_all();
    assert_eq!(rig.emit(&grid, 0, 0).glyph_bytes(), drawn.glyph_bytes(), "as a cold draw does");

    let grid = text_grid(4, &["AB"]);
    let mut rig = GlyphRig::new(false);
    rig.atlas = GlyphAtlas::new(256, 256);
    rig.atlas.__set_entry_cap_for_test(1);
    rig.atlas.set_eviction_enabled(false);
    rig.begin(&grid);
    let refused = rig.emit(&grid, 0, 0);
    assert_eq!(refused.missing, vec!['B'], "the refused glyph draws tofu");
    assert!(!rig.cache.contains(7, refused.key), "a row with a refused glyph is not admitted");
}

/// A run whose shaping fails is drawn (as nothing) but its row is never cached; the next
/// emission at the same pane, slot, origin, surface, cells, style and atlas identity, with no
/// cache clear and no dirt, shapes it again and admits it, and the one after replays. A row
/// whose first style run fails still draws its later run and is not admitted, and a row drawn
/// with no font stack is not admitted.
#[test]
fn failed_shaping_runs_are_drawn_but_never_cached() {
    let grid = text_grid(8, &["a=>b"]);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    fail_next_shape_for_test();
    let failed = rig.emit(&grid, 0, 0);
    assert!(failed.glyphs.is_empty(), "the failed run draws nothing");
    assert!(!rig.cache.contains(7, failed.key), "a row with a failed run is not admitted");
    let (second, stats) = counted(|| rig.emit(&grid, 0, 0));
    assert!(!second.replayed && stats.shape_requests >= 1, "the run is shaped again");
    assert!(!second.glyphs.is_empty() && rig.cache.contains(7, second.key), "and admitted");
    let (third, stats) = counted(|| rig.emit(&grid, 0, 0));
    assert!(third.replayed && stats.shape_requests == 0, "then it replays");
    rig.cache.invalidate_all();
    assert_eq!(rig.emit(&grid, 0, 0).glyph_bytes(), second.glyph_bytes(), "a cold draw agrees");

    let mut mixed = Grid::new(8, 1);
    for (character, flags) in [
        ('a', CellFlags::BOLD),
        ('=', CellFlags::BOLD),
        ('>', CellFlags::empty()),
        ('b', CellFlags::empty()),
    ] {
        mixed.put_char(character, Color::Default, Color::Default, flags);
    }
    let mut rig = GlyphRig::new(false);
    rig.begin(&mixed);
    fail_next_shape_for_test();
    let partial = rig.emit(&mixed, 0, 0);
    assert!(!partial.glyphs.is_empty(), "the later run still draws");
    assert!(!rig.cache.contains(7, partial.key), "the row is not admitted");

    let mut rig = GlyphRig::new(false);
    rig.stack = None;
    rig.raster = None;
    rig.begin(&grid);
    let unshaped = rig.emit(&grid, 0, 0);
    assert!(!rig.cache.contains(7, unshaped.key), "no font stack: the row is not admitted");
}

/// A Partial frame reuses cached rows: with F0 an edit of slot 2 emits slot 2 alone, one miss
/// that adds a seventh entry and evicts nothing; with the production-like pad of F20 the same
/// edit emits slots 0 to 4, four of them hits. A later Full repaint hits all six rows, five
/// unchanged and the admitted edited row.
#[test]
fn partial_frames_reuse_cached_rows() {
    for (pad, mask, hits) in [(0.0, vec![2usize], 0u64), (20.0, vec![0, 1, 2, 3, 4], 4)] {
        let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
        let mut rig = GlyphRig::new(false);
        let mut ink = crate::row_ink::RowInkTable::default();
        let warm = policy_plan(6, pad, 1, Vec::new(), Vec::new(), None);
        assemble_pass(&mut rig, &mut ink, &grid, &warm);
        present_pass(&mut rig, &mut ink, &warm);
        assert_eq!(rig.cache.len(), 6);
        write_row(&mut grid, 2, "EDIT");
        let edit = policy_plan(6, pad, 2, vec![2], strip_records(6), Some(&warm.key));
        assert_eq!(edit.mode, RenderMode::Partial, "pad {pad}");
        let emitted: Vec<usize> = (0..6).filter(|slot| edit.panes[0].emit_rows[*slot]).collect();
        assert_eq!(emitted, mask, "pad {pad}: the planned mask");
        let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &edit);
        assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (hits, 1), "pad {pad}");
        assert_eq!(rig.cache.len(), 7, "pad {pad}: one admission and no eviction");
        present_pass(&mut rig, &mut ink, &edit);
        let full = policy_plan(6, pad, 3, Vec::new(), Vec::new(), None);
        let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &full);
        assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (6, 0), "pad {pad}");
    }
}

/// Pinned rows survive quota churn: a hundred presented Partial frames each edit slot 5 of a
/// six-row pane (quota 24) with new content, and the committed rows of slots 0 to 4, never
/// touched since the warm frame and the oldest entries, survive; the final Full repaint hits
/// all six rows.
#[test]
fn pinned_rows_survive_quota_churn() {
    let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let mut previous = policy_plan(6, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &grid, &previous);
    present_pass(&mut rig, &mut ink, &previous);
    let unchanged: Vec<u64> =
        (0..5).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    for frame in 0..100u64 {
        write_row(&mut grid, 5, &format!("e{frame:03}"));
        let edit = policy_plan(6, 0.0, 2 + frame, vec![5], strip_records(6), Some(&previous.key));
        assert_eq!(edit.mode, RenderMode::Partial);
        assemble_pass(&mut rig, &mut ink, &grid, &edit);
        present_pass(&mut rig, &mut ink, &edit);
        previous = edit;
    }
    for (slot, key) in unchanged.iter().enumerate() {
        assert!(rig.cache.contains(7, *key), "slot {slot}'s committed row survived");
    }
    let full = policy_plan(6, 0.0, 500, Vec::new(), Vec::new(), None);
    let (_, stats) = assemble_pass(&mut rig, &mut ink, &grid, &full);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (6, 0));
}

/// A moved row is pinned before any admission: with three rows (quota 12), eleven entries of
/// which the committed `[A, B, C]` are the oldest, a Full pass emitting `[X, Y, A]` admits X
/// and Y, evicting only unpinned filler, and A hits. When that pass is not presented, its
/// staged keys are discarded, the committed keys stay `[A, B, C]`, and the retry draws the
/// same bytes. The history-cap drain that shifts every absolute row is covered by
/// `scrolling_one_line_at_the_history_cap_reshapes_only_the_new_row`.
#[test]
fn moved_rows_are_pinned_before_any_admission() {
    let first = text_grid(8, &["A=>1", "B=>2", "C=>3"]);
    let moved = text_grid(8, &["X=>4", "Y=>5", "A=>1"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let plan = policy_plan(3, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &first, &plan);
    present_pass(&mut rig, &mut ink, &plan);
    let committed: Vec<u64> =
        (0..3).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    rig.cache.begin_frame(&[(7, 3, 8)]);
    // Fillers are admitted on a later pass, with the committed slots pinned as every pass pins them.
    rig.cache.pin(7, &[]);
    let identity = row_cache_atlas_identity(&rig.atlas);
    for filler in 0..8u64 {
        let row = sonicterm_text::row_glyph_cache::CachedRow::default();
        assert!(rig.cache.insert(7, 1_000 + filler, identity, row));
    }
    assert_eq!(rig.cache.len(), 11);
    let full = policy_plan(3, 0.0, 2, Vec::new(), Vec::new(), None);
    let (glyphs, stats) = assemble_pass(&mut rig, &mut ink, &moved, &full);
    assert_eq!((stats.row_cache_hits, stats.row_cache_misses), (1, 2), "A hits, X and Y miss");
    for key in &committed {
        assert!(rig.cache.contains(7, *key), "committed row {key} stayed pinned");
    }
    rig.cache.discard_staged();
    let after: Vec<u64> = (0..3).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
    assert_eq!(after, committed, "an unpresented pass commits nothing");
    let (retry, stats) = assemble_pass(&mut rig, &mut ink, &moved, &full);
    assert_eq!(stats.row_cache_hits, 3, "the retry replays all three rows");
    assert_eq!(bytemuck::cast_slice::<_, u8>(&retry), bytemuck::cast_slice::<_, u8>(&glyphs));
}

/// Identical rows in one pass share one entry: the first misses and is admitted, the others
/// hit it and draw what a cold draw of their own slot does. An atlas growth changes the atlas
/// identity, so the clean retry misses every row rather than replaying stale UVs.
#[test]
fn identical_rows_share_an_entry_and_atlas_growth_misses() {
    let grid = text_grid(8, &["same=>x", "same=>x", "same=>x"]);
    let mut rig = GlyphRig::new(false);
    rig.atlas = GlyphAtlas::growable(256, 2048);
    rig.begin(&grid);
    let replays: Vec<bool> = (0..3).map(|slot| rig.emit(&grid, 0, slot).replayed).collect();
    assert_eq!(replays, [false, true, true]);
    rig.begin(&grid);
    let warm_slot_two = rig.emit(&grid, 0, 2);
    rig.cache.invalidate_all();
    rig.begin(&grid);
    assert_eq!(rig.emit(&grid, 0, 2).glyph_bytes(), warm_slot_two.glyph_bytes());
    assert!(rig.atlas.grow_to(512), "the atlas doubles");
    rig.begin(&grid);
    assert!(!rig.emit(&grid, 0, 0).replayed, "a grown atlas's identity misses");
}

/// A Partial first pass that falls back admits its real entries; the forced-Full second pass
/// starts with an empty stage, replays the admitted row, draws what a cold Full does, stages
/// one ink record per emitted slot, and its keys, not the first pass's, are the ones committed.
#[test]
fn a_partial_fallbacks_staged_keys_never_commit() {
    let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
    let mut rig = GlyphRig::new(false);
    let mut ink = crate::row_ink::RowInkTable::default();
    let warm = policy_plan(6, 0.0, 1, Vec::new(), Vec::new(), None);
    assemble_pass(&mut rig, &mut ink, &grid, &warm);
    present_pass(&mut rig, &mut ink, &warm);
    let warm_key = rig.cache.committed_slot(7, 2).unwrap();
    write_row(&mut grid, 2, "EDIT");
    let partial = policy_plan(6, 0.0, 2, vec![2], strip_records(6), Some(&warm.key));
    assert_eq!(partial.mode, RenderMode::Partial);
    let (_, first) = assemble_pass(&mut rig, &mut ink, &grid, &partial);
    assert_eq!(first.row_cache_misses, 1, "the first pass admits the edited row");
    let edited_key = rig.cache.staged_slot(7, 2).unwrap();
    assert_ne!(edited_key, warm_key);
    let mut forced = policy_plan(6, 0.0, 2, vec![2], strip_records(6), Some(&warm.key));
    forced.force_full();
    let (forced_glyphs, second) = assemble_pass(&mut rig, &mut ink, &grid, &forced);
    assert_eq!((second.row_cache_hits, second.row_cache_misses), (6, 0));
    assert_eq!(ink.staged_len(), 6, "one surviving ink record per emitted slot");
    present_pass(&mut rig, &mut ink, &forced);
    assert_eq!(rig.cache.committed_slot(7, 2), Some(edited_key));
    rig.cache.invalidate_all();
    let (cold, _) = assemble_pass(&mut rig, &mut ink, &grid, &forced);
    assert_eq!(bytemuck::cast_slice::<_, u8>(&forced_glyphs), bytemuck::cast_slice::<_, u8>(&cold));

    // The first pass's stage is cleared before the second pass emits anything.
    rig.cache.begin_frame(&[(7, 6, 8)]);
    rig.cache.stage_slot(7, 2, 99);
    rig.cache.begin_frame(&[(7, 6, 8)]);
    assert_eq!(rig.cache.staged_slot(7, 2), Some(0));
}

/// How one fallback pass changes the glyph atlas while it assembles, as eviction, a reset or a
/// growth can during a real assembly.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AtlasChange {
    /// The atlas is left alone.
    Unchanged,
    /// The atlas is reset in place, giving it a new content identity.
    Reset,
    /// The atlas doubles, recomputing every UV.
    Growth,
}

/// Which of the last presented frame's ink reaches slot 0, the row a partial plan does not emit:
/// the cursor recolor or the tab-title ink. Either widens the plan's damage onto that row.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Widening {
    /// The last frame recolored glyphs over slot 0.
    Recolor,
    /// The last frame drew tab-title ink over slot 0.
    TabInk,
}

/// A six-row pane presented in full, then edited at slot 2, for the fallback orchestration tests:
/// the rig (with a growable atlas), its ink table, the edited grid, the presented plan, slot 2's
/// committed key before the edit, and the stage each pass found at its start.
struct FallbackFixture {
    rig: GlyphRig,
    ink: crate::row_ink::RowInkTable,
    grid: Grid,
    warm: FramePlan,
    committed_before: Vec<u64>,
    stages_at_pass_start: Vec<Vec<u64>>,
    committed_after_pass: Vec<Vec<u64>>,
}

impl FallbackFixture {
    fn new() -> Self {
        let mut grid = text_grid(8, &["row0", "row1", "row2", "row3", "row4", "row5"]);
        let mut rig = GlyphRig::new(false);
        rig.atlas = GlyphAtlas::growable(256, 2048);
        let mut ink = crate::row_ink::RowInkTable::default();
        let warm = policy_plan(6, 0.0, grid.revision(), Vec::new(), Vec::new(), None);
        assemble_pass(&mut rig, &mut ink, &grid, &warm);
        present_pass(&mut rig, &mut ink, &warm);
        write_row(&mut grid, 2, "EDIT");
        let committed_before =
            (0..6).map(|slot| rig.cache.committed_slot(7, slot).unwrap()).collect();
        Self {
            rig,
            ink,
            grid,
            warm,
            committed_before,
            stages_at_pass_start: Vec::new(),
            committed_after_pass: Vec::new(),
        }
    }

    /// The edit's plan at the grid's current revision, `Partial` unless `force_full`.
    fn plan(&self, force_full: bool) -> FramePlan {
        let mut plan = policy_plan(
            6,
            0.0,
            self.grid.revision(),
            vec![2],
            strip_records(6),
            Some(&self.warm.key),
        );
        if force_full {
            plan.force_full();
        }
        plan
    }

    /// Every slot's committed glyph key.
    fn committed(&self) -> Vec<u64> {
        (0..6).map(|slot| self.rig.cache.committed_slot(7, slot).unwrap()).collect()
    }

    /// Every slot's staged glyph key.
    fn staged(&self) -> Vec<u64> {
        (0..6).map(|slot| self.rig.cache.staged_slot(7, slot).unwrap()).collect()
    }

    /// One assembly pass through the production pass helpers: `begin_glyph_pass` starts it, the
    /// pane seam emits its rows, `change` is applied to the atlas as assembly could change it,
    /// and `GpuRenderer::finish_assembly_pass` takes the atlas check, the damage widening by the
    /// last frame's `widening` ink over slot 0, the fallback decision and the receipts, exactly
    /// as `assemble_frame` does.
    fn assemble(
        &mut self,
        force_full: bool,
        change: AtlasChange,
        widening: Widening,
    ) -> Result<Assembled> {
        let mut plan = self.plan(force_full);
        let atlas_stamp_at_start = GlyphContentStamp::capture(1, 1, &self.rig.atlas);
        let atlas_evictions_at_start = self.rig.atlas.evictions();
        begin_pass(&mut self.rig, &mut self.ink, &self.grid, &plan);
        self.stages_at_pass_start.push(self.staged());
        let (glyphs, _) = emit_pass(&mut self.rig, &mut self.ink, &self.grid, &plan);
        self.committed_after_pass.push(self.committed());
        match change {
            AtlasChange::Unchanged => {}
            AtlasChange::Reset => self.rig.atlas.reset_in_place(),
            AtlasChange::Growth => {
                let doubled = self.rig.atlas.width() * 2;
                assert!(self.rig.atlas.grow_to(doubled), "the atlas doubles");
            }
        }
        let slot_zero = crate::cursor::RecolorBounds::Rect(strip_records(6)[0].expect("strip"));
        let (previous_recolor, previous_tab_ink) = match widening {
            Widening::Recolor => (
                crate::cursor::RecolorRecord { bounds: slot_zero, hash: 1 },
                crate::cursor::RecolorBounds::Empty,
            ),
            Widening::TabInk => (crate::cursor::RecolorRecord::default(), slot_zero),
        };
        let pass_end = PassEnd {
            atlas_stamp_at_start,
            atlas_stamp_now: GlyphContentStamp::capture(1, 1, &self.rig.atlas),
            atlas_evictions_at_start,
            previous_recolor,
            current_recolor: crate::cursor::RecolorRecord::default(),
            previous_tab_ink,
            current_tab_ink: crate::cursor::RecolorBounds::Empty,
        };
        let panes = [sonicterm_render_model::PaneRender {
            id: 7,
            rect_px: PixelRect { x: 0, y: 0, w: 80, h: 120 },
            grid: &mut self.grid,
            viewport_top_abs: None,
            is_active: true,
            cursor_style: sonicterm_render_model::CursorStyle::default(),
            is_broadcast_participant: false,
            scrollbar_alpha: 0.0,
            inline_images: Vec::new(),
        }];
        let ended = GpuRenderer::finish_assembly_pass(&mut plan, &panes, pass_end);
        drop(panes);
        let receipts = match ended {
            Ok(receipts) => receipts,
            Err(early_exit) => return Ok(early_exit),
        };
        Ok(Assembled::Layers(Box::new(AssembledLayers {
            surface_width: 240.0,
            surface_height: 200.0,
            subpixel_aa: SubpixelAaMode::Off,
            scratch: frame_scratch::FrameScratch { glyphs, ..Default::default() },
            field_candidates: PresentedFields::default(),
            missing_chars: Vec::new(),
            missing_chrome_chars: Vec::new(),
            gpu_timing: None,
            plan,
            receipts,
            recolor: crate::cursor::RecolorRecord::default(),
            tab_ink: crate::cursor::RecolorBounds::Empty,
        })))
    }
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
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let releasing = method_body(&source, "    pub fn render_releasing(");
    assert_eq!(releasing.matches("self.row_glyph_cache.discard_staged();").count(), 3);
    let error_arm = releasing.find("Err(error) => {").unwrap();
    assert!(releasing[error_arm..error_arm + 400].contains("discard_staged()"));
    let layers = releasing.find("self.present_layers(*layers).unwrap_or_else(|error| {").unwrap();
    assert!(releasing[layers..layers + 200].contains("discard_staged()"));
    let assemble = method_body(&source, "    fn assemble_frame(");
    let staged = assemble.find("staged_ranges.push((pv.pane_id,").unwrap();
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

/// The body of the item whose signature is `signature` in `source`: from the signature to the
/// closing brace at the signature's own indentation, so a method ends at its own `}`.
fn item_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let indent = &source[line_start..start];
    let indent = &indent[..indent.len() - indent.trim_start().len()];
    let close = format!("\n{indent}}}\n");
    let body = &source[start..];
    let end = body.find(&close).map_or(body.len(), |offset| offset + close.len());
    &body[..end]
}

#[test]
fn run_flush_clones_no_cells() {
    // Style runs borrow the grid's cells: the row walk records runs as slices of borrowed cells
    // and the run builder finds a cluster's lead cell by column without a cloned lookup table.
    let source = include_str!("core.rs").replace("\r\n", "\n");
    let emit = item_body(&source, "pub(crate) fn emit_row_glyphs(");
    assert!(!emit.contains("(*cell).clone()"), "emit_row_glyphs clones no cell into a run");
    assert!(!emit.contains("Vec<(u16, Cell)>"), "emit_row_glyphs keeps no owned run");
    let build = item_body(&source, "fn build_shape_run(");
    assert!(!build.contains("cell_by_col"), "build_shape_run builds no column-to-cell table");
    assert!(!build.contains("c.clone()"), "build_shape_run clones no cell");
}

/// Draw `grid`'s row 0 the way an independent oracle does: group the row's non-continuation
/// cells into consecutive style runs, build each run from owned copies of its cells and project
/// it. Returns the projected glyphs, tofu and missing characters and the row's completeness.
fn oracle_row(
    grid: &Grid,
    stack: &sonicterm_engine::FontStack,
) -> (Vec<GlyphInstance>, Vec<(f32, f32, f32, f32, ChromeColor)>, Vec<char>, bool) {
    let row = grid.row(0);
    let visible: Vec<(u16, Cell)> = row
        .iter()
        .enumerate()
        .filter(|(_, cell)| !cell.flags.contains(CellFlags::WIDE_CONT))
        .map(|(col, cell)| (col as u16, cell.clone()))
        .collect();
    let mut runs: Vec<(RunStyle, Vec<(u16, Cell)>)> = Vec::new();
    for (col, cell) in visible {
        let style = RunStyle::from_cell(&cell);
        match runs.last_mut() {
            Some((open, cells)) if *open == style => cells.push((col, cell)),
            _ => runs.push((style, vec![(col, cell)])),
        }
    }
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let mut raster = stack.clone();
    let theme = Theme::default();
    let snapped = build_snapped_cell_x(0.0, 10.0, grid.cols);
    let (mut glyphs, mut tofu, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    let mut complete = true;
    for (style, cells) in &runs {
        complete &= shape_run_for_test(
            ShapeRunFixture {
                atlas: &mut atlas,
                row: 0,
                style: *style,
                cells,
                theme: &theme,
                fg_default: ChromeColor::rgb(230, 230, 230),
                cell_size: (10.0, 20.0),
                origin: (0.0, 0.0),
                surface: (400.0, 200.0),
                baseline_y_in_cell: 16.0,
                snapped_cell_x: &snapped,
                font_stack: Some(stack),
                wt_raster: Some(&mut raster),
                hovered_url_cells: None,
                hovered_url_accent: [0.0; 4],
                software_presenter: false,
            },
            &mut glyphs,
            &mut tofu,
            &mut missing,
        );
    }
    (glyphs, tofu, missing, complete)
}

/// A one-row grid of `cols` columns written cell by cell with each `(character, flags)`.
fn styled_grid(cols: u16, cells: &[(char, CellFlags)]) -> Grid {
    let mut grid = Grid::new(cols, 1);
    for (character, flags) in cells {
        grid.put_char(*character, Color::Default, Color::Default, *flags);
    }
    grid.clear_dirty();
    grid
}

#[test]
fn borrowed_runs_emit_the_same_records_and_completeness() {
    // Flushing runs as borrowed cells changes no record: wide, combining, mixed-style and
    // ligature rows draw exactly what an oracle building each run from owned cell copies draws,
    // and a row is admitted exactly when the oracle calls it complete. A row whose first of three
    // shaped runs fails still draws its later runs and is not admitted.
    let plain = CellFlags::empty();
    let rows = [
        ("wide", styled_grid(8, &[('你', plain), ('好', plain), ('a', plain)])),
        ("combining", styled_grid(8, &[('e', plain), ('\u{301}', plain), ('x', plain)])),
        (
            "mixed-style",
            styled_grid(
                8,
                &[
                    ('a', CellFlags::BOLD),
                    ('b', CellFlags::BOLD),
                    ('=', plain),
                    ('>', plain),
                    ('c', CellFlags::ITALIC),
                ],
            ),
        ),
        ("ligature", text_grid(8, &["a=>b!=c"])),
    ];
    for (name, grid) in &rows {
        let stack = packaged_font_stack();
        let (glyphs, tofu, missing, complete) = oracle_row(grid, &stack);
        let mut rig = GlyphRig::with_stack(stack, false);
        rig.begin(grid);
        let emitted = rig.emit(grid, 0, 0);
        assert!(!emitted.replayed, "{name}: the cold row misses");
        assert_eq!(
            emitted.glyph_bytes(),
            bytemuck::cast_slice::<GlyphInstance, u8>(&glyphs).to_vec(),
            "{name}: the same glyph records"
        );
        assert_eq!(emitted.missing, missing, "{name}: the same missing characters");
        assert!(emitted.decorations.ends_with(&format!("{tofu:?}")), "{name}: the same tofu");
        assert_eq!(rig.cache.contains(7, emitted.key), complete, "{name}: admitted iff complete");
    }

    let three_runs = styled_grid(
        8,
        &[
            ('a', CellFlags::BOLD),
            ('=', CellFlags::BOLD),
            ('b', plain),
            ('=', plain),
            ('c', CellFlags::ITALIC),
            ('=', CellFlags::ITALIC),
        ],
    );
    let stack = packaged_font_stack();
    let (whole, _, _, whole_complete) = oracle_row(&three_runs, &stack);
    assert!(whole_complete, "the row is complete when every run shapes");
    let mut rig = GlyphRig::with_stack(stack, false);
    rig.begin(&three_runs);
    fail_next_shape_for_test();
    let failed = rig.emit(&three_runs, 0, 0);
    assert!(!failed.glyphs.is_empty(), "runs 2 and 3 still draw");
    assert!(failed.glyphs.len() < whole.len(), "run 1 draws nothing");
    assert!(!rig.cache.contains(7, failed.key), "an incomplete row is not admitted");
}

#[test]
fn face_replacement_sites_clear_both_caches() {
    // Every place the renderer replaces faces without a title-key change drops the kept chrome
    // runs and titles beside its row-cache invalidation; `set_font` reaches the clear only
    // through `adopt_font_stacks`, which the test adoption seam shares. Scanned CRLF-normalized.
    let source = include_str!("core.rs").replace("\r\n", "\n");
    for signature in ["fn adopt_font_stacks(", "fn rebuild_for_sf(", "pub fn clear_shape_cache("] {
        let body = item_body(&source, signature);
        assert!(body.contains("self.chrome_caches.clear_runs();"), "{signature} clears both caches");
        assert!(body.contains("row_glyph_cache.invalidate_all()"), "{signature} beside the rows");
    }
    let set_font = item_body(&source, "pub fn set_font(");
    assert!(set_font.contains("self.adopt_font_stacks("), "set_font adopts through the seam");
    assert!(!set_font.contains("clear_runs"), "and does not clear on its own");
    let seam = item_body(&source, "pub fn __test_adopt_body_font_stack(");
    assert!(seam.contains("self.adopt_font_stacks("), "the test seam shares the clear");
}

#[test]
fn fill_snapped_cell_x_matches_build_bit_for_bit() {
    // Filling a reused edge buffer gives the column edges the allocating builder gives, bit for
    // bit, for fractional origins and cell widths such as fractional-DPI raster sizes, and the
    // same as the snapping formula applied directly.
    let mut seed: u64 = 0x5eed_1555;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as u32
    };
    let mut edges = Vec::new();
    for _ in 0..200 {
        let origin_x = next() as f32 / 997.0 % 300.0;
        let cell_w = 3.0 + (next() % 4000) as f32 / 333.0;
        let cols = (next() % 400) as u16;
        fill_snapped_cell_x(&mut edges, origin_x, cell_w, cols);
        let built = build_snapped_cell_x(origin_x, cell_w, cols);
        let oracle: Vec<f32> = (0..=cols)
            .map(|col| {
                sonicterm_render_model::geometry::snap_to_device_pixels(
                    (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
                    1.0,
                )
                .0
            })
            .collect();
        let bits = |values: &[f32]| values.iter().map(|value| value.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&edges), bits(&built), "origin {origin_x} cell {cell_w} cols {cols}");
        assert_eq!(bits(&edges), bits(&oracle));
    }
}
