use super::*;

// --- Inline IME preedit opaque background -------------------------

#[test]
fn search_scroll_keeps_the_full_block_cursor_visible_without_following_suffix() {
    assert_eq!(search_text_scroll(40.0, 10.0, 100.0), 0.0);
    assert_eq!(search_text_scroll(95.0, 10.0, 100.0), 5.0);
    assert_eq!(search_text_scroll(140.0, 10.0, 100.0), 50.0);
    assert_eq!(search_text_scroll(0.0, 0.0, 0.0), 0.0);
}

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
                tab_count: tabs.tabs().len(),
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
    for channel in 0..4 {
        assert!(
            unfocused_accent.color[channel] < focused_accent.color[channel],
            "channel {channel} must dim: {} vs {}",
            unfocused_accent.color[channel],
            focused_accent.color[channel]
        );
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
            tab_count: tabs.tabs().len(),
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
            tab_count: tabs.tabs().len(),
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
fn preedit_cache_matches_only_on_identical_inputs_and_atlas_epoch() {
    // the cache may only be reused when text + placement + color
    // AND the atlas eviction epoch are identical — an epoch bump means a tile
    // may have been recycled, so the stored UVs could be stale.
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_epoch: GlyphAtlasEpoch { generation: 1, evictions: 7 },
        glyphs: Vec::new(),
    };
    // Exact match.
    let epoch = GlyphAtlasEpoch { generation: 1, evictions: 7 };
    assert!(c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch));
    // Any single field differing must miss.
    assert!(!c.matches("ni'ha", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // text grew
    assert!(!c.matches("ni'hao", 15.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // font size
    assert!(!c.matches("ni'hao", 14.0, 101.0, 50.0, 0xAABBCCFF, epoch)); // x (scroll)
    assert!(!c.matches("ni'hao", 14.0, 100.0, 51.0, 0xAABBCCFF, epoch)); // y
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0x11223344, epoch)); // color
    let evicted_epoch = GlyphAtlasEpoch { generation: 1, evictions: 8 };
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, evicted_epoch));
}

#[test]
fn preedit_cache_rejects_same_eviction_count_after_atlas_replacement() {
    let old_epoch = GlyphAtlasEpoch { generation: 3, evictions: 0 };
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_epoch: old_epoch,
        glyphs: Vec::new(),
    };
    let replacement_epoch = GlyphAtlasEpoch { generation: 4, evictions: 0 };

    assert!(
        !c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, replacement_epoch),
        "equal eviction counts from different atlas allocations must not reuse cached UVs"
    );
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

#[test]
fn atlas_eviction_during_frame_requires_retry() {
    let mut atlas = GlyphAtlas::new(1, 1);
    let mut raster = OnePixelAtlasGlyph;
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('a', false, false), &mut raster)
        .expect("first glyph fills the atlas");
    let frame_epoch = atlas.evictions();

    atlas.tick_frame();
    atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('b', false, false), &mut raster)
        .expect("second glyph evicts and reuses the only slot");

    assert!(
        atlas_evicted_during_frame(frame_epoch, &atlas),
        "a frame must not present instances whose UV rectangles may have been recycled"
    );
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
        InlineImagePlacement { image: &older, origin_x: 0.0, origin_y: 0.0, painter_order: 0 },
        InlineImagePlacement { image: &newer, origin_x: 0.0, origin_y: 0.0, painter_order: 1 },
    ];

    let skipped =
        emit_inline_image_instances(&mut atlas, &mut instances, &placements, 1.0, 1.0, 10.0, 10.0);

    let newer_key = sonicterm_types::GlyphKey {
        ch: '\u{fffc}',
        font_slot: 0xFE,
        weight_bold: false,
        italic: false,
        glyph_id: fold_u64_to_u32(2),
    };
    let older_key = sonicterm_types::GlyphKey { glyph_id: fold_u64_to_u32(1), ..newer_key };
    assert_eq!(skipped, 1);
    assert_eq!(atlas.evictions(), 0, "image pressure must never recycle atlas rectangles");
    assert!(atlas.get(newer_key).is_some(), "newest image should win bounded capacity");
    assert!(atlas.get(older_key).is_none(), "older image should be skipped once full");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].flags[2], 1.0, "image instances select the image atlas");
}

#[test]
fn cursor_color_uses_theme_cursor_accent() {
    let theme = Theme::default();
    assert_eq!(cursor_color_from_theme(&theme), hex_to_rgba(theme.colors.cursor.0.as_str(), 1.0));
    assert_eq!(theme.colors.cursor, theme.colors.tab.active_fg);
}

#[test]
fn cursor_text_color_uses_theme_cursor_text() {
    let theme = Theme::default();
    assert_eq!(
        cursor_text_color_from_theme(&theme),
        hex_to_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
    );
    assert_ne!(
        cursor_text_color_from_theme(&theme),
        cursor_color_from_theme(&theme),
        "cursor text must not reuse the colored cursor accent"
    );
}

#[test]
fn cursor_stays_opaque_while_blink_phase_changes() {
    let yellow = [1.0, 0.5, 0.0, 1.0];

    assert_eq!(active_cursor_color(yellow, CursorShape::Block, 0.25)[3], 1.0);
    assert_eq!(active_cursor_color(yellow, CursorShape::Bar, 0.25), yellow);
    assert_eq!(active_cursor_color(yellow, CursorShape::Underline, 0.25), yellow);
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

#[test]
fn underline_key_ignores_blank_cells() {
    let mut blank = Cell::plain(' ', Color::Indexed(1), Color::Default, CellFlags::UNDERLINE);
    blank.set_underline_style(UnderlineStyle::Dashed);
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

#[test]
fn cursor_slice_tracks_search_and_palette_unicode_characters() {
    assert_eq!(cursor_char_slice_at("abc", 0), Some("a"));
    assert_eq!(cursor_char_slice_at("a中b", 1), Some("中"));
    assert_eq!(cursor_char_slice_at("a🙂b", 1), Some("🙂"));
    assert_eq!(cursor_char_slice_at("a中b", "a中".len()), Some("b"));
    assert_eq!(cursor_char_slice_at("a中", "a中".len()), None);
    assert_eq!(cursor_char_slice_at("", 0), None);
}

#[test]
fn palette_cursor_slice_handles_non_boundary_offsets() {
    let s = "a中b";
    assert_eq!(cursor_char_slice_at(s, 2), Some("中"));
}

#[test]
fn palette_cursor_uses_placeholder_only_for_an_empty_query() {
    assert_eq!(palette_cursor_char("", 0, Some("Search commands…")), Some("S"));
    assert_eq!(palette_cursor_char("", 0, Some("搜索命令")), Some("搜"));
    assert_eq!(palette_cursor_char("abc", 0, Some("Search commands…")), Some("a"));
    assert_eq!(palette_cursor_char("abc", 3, Some("Search commands…")), None);
    assert_eq!(palette_cursor_char("", 0, None), None);
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

// --- Atlas reset / row-cache invalidation pairing ------------------

/// Every atlas reset must flush the row glyph cache and defeat the frame-skip
/// guard, in the same function.
///
/// The row cache tags entries with `glyph_atlas.evictions()` alone — the
/// atlas-local counter, with no allocation-generation component. That filter
/// cannot tell an old atlas's epoch-0 from a fresh atlas's epoch-0, so a row
/// entry that outlived a reset could match against tiles it was never built
/// for and sample whatever now occupies those rectangles.
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
    const CORE_SRC: &str = include_str!("core.rs");
    const RESET: &str = "self.reset_glyph_atlas_in_place(";
    const INVALIDATE: &str = "self.row_glyph_cache.invalidate_all()";
    const CLEAR_FRAME_KEY: &str = "self.last_frame_key = None";

    let lines: Vec<&str> = CORE_SRC.lines().collect();
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
                    || line.starts_with("impl ")
                    || line.starts_with("}")
            })
            .map_or(lines.len(), |(index, _)| index);

        let body = &lines[start..end];
        let invalidated = body.iter().any(|line| line.contains(INVALIDATE));
        let frame_key_cleared = body.iter().any(|line| line.contains(CLEAR_FRAME_KEY));
        assert!(
            invalidated,
            "core.rs:{} resets the glyph atlas without calling \
             `row_glyph_cache.invalidate_all()` in the same function.\n\
             The row cache keys on the atlas eviction count alone, which resets \
             with the atlas, so surviving entries can match a fresh atlas and \
             sample tiles they were not built against.\n\
             Offending line: {}",
            site + 1,
            lines[site].trim()
        );
        assert!(
            frame_key_cleared,
            "core.rs:{} resets the glyph atlas without clearing \
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
        software_frame: amount(4 * 1024 * 1024, 1),
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
