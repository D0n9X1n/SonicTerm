//! Renderer row shaping contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

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
            vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../sonicterm-font/test-fonts/rec-mono")],
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
        oversize: false,
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
        oversize: false,
    };
    let missing = sonicterm_text::glyph_atlas::GlyphInfo { missing: true, ..empty };
    assert_eq!(drawable_or_tofu(Some(missing)), None, "missing draws tofu");
    // The fallback paths keep their behaviour for a tile too large to place: its zero-area sentinel
    // passes through, so the caller skips it like an empty glyph and draws no tofu.
    let oversize = sonicterm_text::glyph_atlas::GlyphInfo { oversize: true, ..empty };
    assert_eq!(drawable_or_tofu(Some(oversize)), Some(oversize), "an oversize glyph is skipped");
    assert_eq!(drawable_or_tofu(None), None, "a refused glyph draws tofu");
    assert_eq!(drawable_or_tofu(Some(empty)), Some(empty), "an empty glyph is skipped, not tofu");
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
        let stack = rec_mono_font_stack();
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
    let stack = rec_mono_font_stack();
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
fn borrowed_run_records_follow_each_lead_cell() {
    // The run builder finds each shaped cluster's lead cell among the borrowed cells by column.
    // Over gapped columns (a wide cell's continuation is never in the run), wide cells, a combining
    // cluster and a distinct colour per cell, every record's colour, wide bit and extras bit equal
    // those of the cell an independent linear scan finds at the record's lead column, and every
    // cell of the run leads at least one record.
    let stack = rec_mono_font_stack();
    let colours = [(200, 30, 30), (30, 200, 30), (30, 30, 200), (200, 200, 30), (30, 200, 200)];
    let mut combining =
        Cell::plain('e', Color::Rgb(200, 30, 200), Color::Default, CellFlags::empty());
    combining.set_extras(Some("\u{301}".to_string().into_boxed_str()));
    let owned: Vec<(u16, Cell)> = vec![
        (
            0,
            Cell::plain(
                'W',
                Color::Rgb(colours[0].0, colours[0].1, colours[0].2),
                Color::Default,
                CellFlags::WIDE,
            ),
        ),
        (
            2,
            Cell::plain(
                'a',
                Color::Rgb(colours[1].0, colours[1].1, colours[1].2),
                Color::Default,
                CellFlags::empty(),
            ),
        ),
        (3, combining),
        (
            4,
            Cell::plain(
                'M',
                Color::Rgb(colours[2].0, colours[2].1, colours[2].2),
                Color::Default,
                CellFlags::WIDE,
            ),
        ),
        (
            6,
            Cell::plain(
                'z',
                Color::Rgb(colours[3].0, colours[3].1, colours[3].2),
                Color::Default,
                CellFlags::empty(),
            ),
        ),
        (
            7,
            Cell::plain(
                'é',
                Color::Rgb(colours[4].0, colours[4].1, colours[4].2),
                Color::Default,
                CellFlags::empty(),
            ),
        ),
    ];
    let cells: Vec<(u16, &Cell)> = owned.iter().map(|(col, cell)| (*col, cell)).collect();
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    let style = RunStyle { bold: false, italic: false };
    assert!(
        build_run_records(&mut atlas, &stack, style, &cells, &mut records),
        "the run is complete"
    );
    let theme = Theme::default();
    let fg_default = ChromeColor::rgb(230, 230, 230);
    let mut led = std::collections::BTreeSet::new();
    for record in &records.glyphs {
        let lead = oracle_cell_at(&cells, record.lead_col).expect("a record leads at a run column");
        led.insert(record.lead_col);
        let bits = record.bits();
        assert_eq!(
            bits.is_wide,
            lead.flags.contains(CellFlags::WIDE),
            "column {}",
            record.lead_col
        );
        assert_eq!(bits.has_extras, lead.extras().is_some(), "column {}", record.lead_col);
        if !bits.is_color {
            let expected = chrome_color_to_linear_rgba(cell_fg(lead, &theme, fg_default));
            assert_eq!(
                record.color, expected,
                "column {} draws its own cell's colour",
                record.lead_col
            );
        }
    }
    for tofu in &records.tofu {
        led.insert(tofu.lead_col);
    }
    let columns: std::collections::BTreeSet<u16> = cells.iter().map(|(col, _)| *col).collect();
    assert_eq!(led, columns, "every cell of the run leads a record");
}

#[test]
fn later_runs_keep_their_exact_records_after_the_first_fails() {
    // Three styled runs share one row's records. When the first run's shaping fails, it adds no
    // record and the row is incomplete, while runs 2 and 3 add exactly the records each builds
    // on its own over the same atlas order: the failed run leaves nothing behind.
    let stack = rec_mono_font_stack();
    let run_cells = |text: &str, first_col: u16, flags: CellFlags, tint: u8| -> Vec<(u16, Cell)> {
        text.chars()
            .enumerate()
            .map(|(offset, character)| {
                let col = first_col + offset as u16;
                let colour = Color::Rgb(tint, 255 - tint, (col as u8).wrapping_mul(37));
                (col, Cell::plain(character, colour, Color::Default, flags))
            })
            .collect()
    };
    let first = run_cells("a=é", 0, CellFlags::BOLD, 40);
    let second = run_cells("b=ñ", 3, CellFlags::empty(), 120);
    let third = run_cells("c=ü", 6, CellFlags::ITALIC, 200);
    let styles = [
        RunStyle { bold: true, italic: false },
        RunStyle { bold: false, italic: false },
        RunStyle { bold: false, italic: true },
    ];

    let mut atlas = GlyphAtlas::new(1024, 1024);
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    fail_next_shape_for_test();
    let first_done =
        build_run_records(&mut atlas, &stack, styles[0], &borrow_cells(&first), &mut records);
    assert!(!first_done, "the failed first run is incomplete");
    assert!(records.glyphs.is_empty() && records.tofu.is_empty(), "and adds no record");
    let after_first = records.glyphs.len();
    assert!(build_run_records(&mut atlas, &stack, styles[1], &borrow_cells(&second), &mut records));
    let after_second = records.glyphs.len();
    assert!(build_run_records(&mut atlas, &stack, styles[2], &borrow_cells(&third), &mut records));

    let mut oracle_atlas = GlyphAtlas::new(1024, 1024);
    let mut expected_second = sonicterm_text::row_glyph_cache::CachedRow::default();
    let mut expected_third = sonicterm_text::row_glyph_cache::CachedRow::default();
    build_run_records(
        &mut oracle_atlas,
        &stack,
        styles[1],
        &borrow_cells(&second),
        &mut expected_second,
    );
    build_run_records(
        &mut oracle_atlas,
        &stack,
        styles[2],
        &borrow_cells(&third),
        &mut expected_third,
    );
    assert!(!expected_second.glyphs.is_empty() && !expected_third.glyphs.is_empty());
    assert_eq!(&records.glyphs[after_first..after_second], &expected_second.glyphs[..], "run 2");
    assert_eq!(&records.glyphs[after_second..], &expected_third.glyphs[..], "run 3");
}

/// An ASCII fast-path run draws its glyphs from each cell's key and never builds run text, even
/// with a font stack present. Pins existing behaviour; the count is scoped to this emit.
#[test]
fn an_ascii_fast_run_builds_no_run_text() {
    let plain = CellFlags::empty();
    let grid = styled_grid(8, &[('a', plain), ('b', plain), ('c', plain)]);
    let mut rig = GlyphRig::new(false);
    let (emitted, materialized) = emit_counting_materialized(&mut rig, &grid);
    assert!(!emitted.glyphs.is_empty(), "the fast path still draws");
    assert_eq!(materialized, 0, "no run text is built for an ASCII fast-path run");
}

/// A non-ASCII run with no font stack returns incomplete before any run text is built; the
/// rasterizer is kept, so the ASCII path's behaviour is untouched. Pins existing behaviour.
#[test]
fn a_shaped_run_without_a_font_stack_builds_no_run_text() {
    let grid = styled_grid(8, &[('中', CellFlags::empty())]);
    let mut rig = GlyphRig::new(false);
    rig.stack = None;
    let (emitted, materialized) = emit_counting_materialized(&mut rig, &grid);
    assert_eq!(materialized, 0, "no run text is built without a font stack");
    assert!(!rig.cache.contains(7, emitted.key), "the incomplete row is not admitted");
}

/// With a font stack, each non-ASCII run is materialized exactly once and an ASCII run not at
/// all: a bold `中` run, then a plain `ab` run, builds one run text.
#[test]
fn only_the_shaped_run_builds_its_text_once() {
    let grid = styled_grid(
        8,
        &[('中', CellFlags::BOLD), ('a', CellFlags::empty()), ('b', CellFlags::empty())],
    );
    let mut rig = GlyphRig::new(false);
    let (emitted, materialized) = emit_counting_materialized(&mut rig, &grid);
    assert_eq!(materialized, 1, "one shaped run, one materialization");
    assert!(rig.cache.contains(7, emitted.key), "a complete row is admitted");
}

/// A run that cannot complete does not stop the runs after it: with no font stack, a bold `中`
/// run is incomplete, and the plain `abc` run after it still draws its three glyphs.
#[test]
fn later_runs_are_processed_after_an_incomplete_run() {
    let plain = CellFlags::empty();
    let grid = styled_grid(8, &[('中', CellFlags::BOLD), ('a', plain), ('b', plain), ('c', plain)]);
    let mut rig = GlyphRig::new(false);
    rig.stack = None;
    let (emitted, _) = emit_counting_materialized(&mut rig, &grid);
    assert_eq!(emitted.glyphs.len(), 3, "the ASCII run after the incomplete one draws");
    assert!(!rig.cache.contains(7, emitted.key), "the row is not admitted");
}

/// A shaped glyph with a real id whose rasterization produced nothing is drawn as nothing, so it
/// is reported missing and its row is not cached, instead of passing as an empty glyph.
#[test]
fn a_shaped_glyph_that_rasterizes_nothing_is_reported_missing() {
    let key = shaped_key_of('用');
    let grid = text_grid(2, &["用"]);
    let mut rig = GlyphRig::new(false);
    // The atlas caches the failed rasterization as its missing sentinel under the shaped key.
    let _sentinel = rig.atlas.get_or_insert(key, &mut NoGlyphs);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    assert_eq!(drawn.missing, vec!['用'], "the missing glyph is reported");
    assert!(!rig.cache.contains(7, drawn.key), "and its row is not cached");
}

/// A shaped glyph with a real id that the atlas refuses (its entry index full, eviction barred) is
/// reported missing as well as keeping its row out of the cache.
#[test]
fn a_shaped_glyph_the_atlas_refuses_is_reported_missing() {
    let grid = text_grid(2, &["用"]);
    let mut rig = GlyphRig::new(false);
    rig.atlas.__set_entry_cap_for_test(1);
    rig.atlas.set_eviction_enabled(false);
    // One unrelated entry fills the one-entry index, so the shaped glyph cannot be admitted.
    let filler = sonicterm_types::glyph_key::GlyphKey::new('\u{E000}', false, false);
    let _filler = rig.atlas.get_or_insert(filler, &mut NoGlyphs);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    assert_eq!(drawn.missing, vec!['用'], "the refused glyph is reported");
    assert!(!rig.cache.contains(7, drawn.key), "and its row is not cached");
}

/// A shaped glyph whose tile is larger than the atlas can ever place is cached as a zero-area
/// sentinel; it draws nothing, so it is reported missing and its row is not cached.
#[test]
fn a_shaped_glyph_too_large_for_the_atlas_is_reported_missing() {
    let grid = text_grid(2, &["用"]);
    let mut rig = GlyphRig::new(false);
    // A fixed 8x8 atlas cannot place a 14 px CJK tile.
    rig.atlas = GlyphAtlas::new(8, 8);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    assert_eq!(drawn.missing, vec!['用'], "the oversize glyph is reported");
    assert!(!rig.cache.contains(7, drawn.key), "and its row is not cached");
}

/// An intentionally empty glyph (an ideographic space) records nothing and leaves its row complete:
/// the strengthened diagnostics report only glyphs that should have drawn.
#[test]
fn an_intentionally_empty_glyph_records_nothing_and_its_row_is_cached() {
    let grid = text_grid(2, &["\u{3000}"]);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    assert!(drawn.missing.is_empty(), "a blank is never missing: {:?}", drawn.missing);
    assert!(rig.cache.contains(7, drawn.key), "and its row is cached");
}

/// The ASCII fast path, one of the character-fallback paths, keeps its behaviour for a tile too
/// large to place: the oversize sentinel draws nothing, records no missing character or tofu, and
/// leaves the row complete, so the row is cached. Only the shaped nonzero-id path reports it.
#[test]
fn an_oversize_ascii_fallback_tile_draws_nothing_and_its_row_is_cached() {
    let grid = text_grid(2, &["A"]);
    let mut rig = GlyphRig::new(false);
    // A fixed 4x4 atlas cannot place a 14 px Latin tile.
    rig.atlas = GlyphAtlas::new(4, 4);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    let key = sonicterm_types::glyph_key::GlyphKey::new('A', false, false);
    let cached = rig.atlas.get(key).expect("the fast path cached the glyph");
    assert!(cached.oversize, "the tile is the oversize sentinel: {cached:?}");
    assert!(drawn.missing.is_empty(), "no missing character is recorded: {:?}", drawn.missing);
    assert!(drawn.glyphs.is_empty(), "the oversize tile draws no glyph");
    assert!(rig.cache.contains(7, drawn.key), "the row stays complete and is cached");
}

/// The allocation-free hit test equals the edge-vector scan for every pixel across panes with
/// fractional origins and cell widths, including padding on both sides and exact boundaries.
#[test]
fn snapped_col_at_matches_the_edge_vector_scan() {
    for (origin_x, cell_w, cols) in [(10.0, 7.5, 8), (0.0, 8.25, 120), (3.3, 9.7, 1), (5.0, 7.0, 0)]
    {
        let edges = build_snapped_cell_x(origin_x, cell_w, cols);
        let right = origin_x + cell_w * f32::from(cols) + 4.0;
        let mut px = origin_x - 4.0;
        while px <= right {
            assert_eq!(
                snapped_col_at(px, origin_x, cell_w, cols),
                pixel_to_local_col(px, &edges, cols),
                "origin {origin_x} width {cell_w} cols {cols} px {px}"
            );
            px += 0.25;
        }
        for edge in &edges {
            assert_eq!(
                snapped_col_at(*edge, origin_x, cell_w, cols),
                pixel_to_local_col(*edge, &edges, cols)
            );
        }
    }
}
