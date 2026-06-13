
use super::*;

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
fn preedit_cache_matches_only_on_identical_inputs_and_atlas_epoch() {
    // Issue #714: the cache may only be reused when text + placement + color
    // AND the atlas eviction epoch are identical — an epoch bump means a tile
    // may have been recycled, so the stored UVs could be stale.
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_epoch: 7,
        glyphs: Vec::new(),
    };
    // Exact match.
    assert!(c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, 7));
    // Any single field differing must miss.
    assert!(!c.matches("ni'ha", 14.0, 100.0, 50.0, 0xAABBCCFF, 7)); // text grew
    assert!(!c.matches("ni'hao", 15.0, 100.0, 50.0, 0xAABBCCFF, 7)); // font size
    assert!(!c.matches("ni'hao", 14.0, 101.0, 50.0, 0xAABBCCFF, 7)); // x (scroll)
    assert!(!c.matches("ni'hao", 14.0, 100.0, 51.0, 0xAABBCCFF, 7)); // y
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0x11223344, 7)); // color
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, 8)); // atlas evicted
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

    assert_eq!(
        damage,
        Some(sonicterm_render_model::geometry::PixelRect { x: 8, y: 22, w: 60, h: 36 })
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
fn inverse_swaps_foreground_and_background_for_rendering() {
    let theme = Theme::default();
    let cell = Cell::plain('x', Color::Indexed(1), Color::Indexed(2), CellFlags::INVERSE);
    assert_eq!(cell_fg(&cell, &theme, ChromeColor::WHITE), indexed(2, &theme).unwrap());
    assert_eq!(
        cell_bg_rgba(&cell, &theme),
        Some(chrome_color_to_linear_rgba(indexed(1, &theme).unwrap()))
    );
}

#[test]
fn palette_cursor_slice_tracks_current_character() {
    assert_eq!(cursor_char_slice_at("abc", 0), Some("a"));
    assert_eq!(cursor_char_slice_at("a中b", 1), Some("中"));
    assert_eq!(cursor_char_slice_at("a中b", "a中".len()), Some("b"));
    assert_eq!(cursor_char_slice_at("a中", "a中".len()), None);
}

#[test]
fn palette_cursor_slice_handles_non_boundary_offsets() {
    let s = "a中b";
    assert_eq!(cursor_char_slice_at(s, 2), Some("中"));
}

#[test]
fn plain_url_hover_does_not_need_accent_palette() {
    use sonicterm_render_model::inputs::HoveredUrlCells;

    assert!(!hovered_url_needs_accent(None));
    assert!(!hovered_url_needs_accent(Some(HoveredUrlCells {
        row: 0,
        start_col: 1,
        end_col: 5,
        active: false,
    })));
    assert!(hovered_url_needs_accent(Some(HoveredUrlCells {
        row: 0,
        start_col: 1,
        end_col: 5,
        active: true,
    })));
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
            y_offset: 0.0,
            ch: '✔',
        },
        ShapedGlyph {
            lead_col: 0,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 2,
            x_advance: 0.0,
            y_offset: 0.0,
            ch: '✔',
        },
        ShapedGlyph {
            lead_col: 1,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 3,
            x_advance: 0.0,
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
            y_offset: 0.0,
            ch: 'x',
        },
        ShapedGlyph {
            lead_col: 0,
            cluster_cells: 1,
            font_slot: 0,
            glyph_id: 2,
            x_advance: 0.0,
            y_offset: 0.0,
            ch: 'y',
        },
    ];

    assert!(!shaped_glyph_columns_are_monotonic(&glyphs));
}
