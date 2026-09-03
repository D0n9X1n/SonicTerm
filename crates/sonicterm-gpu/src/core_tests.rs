use super::*;

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
    assert!(matches!(device_descriptor_for(true).memory_hints, wgpu::MemoryHints::MemoryUsage));
    assert!(matches!(device_descriptor_for(false).memory_hints, wgpu::MemoryHints::Performance));
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
    let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor_for(true)))
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
            &[clear],
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

#[test]
fn status_marker_fit_is_wired_before_both_terminal_glyph_emissions() {
    // Contract: fallback and shaped producers both fit before creating GlyphInstance rectangles.
    const SOURCE: &str = include_str!("core.rs");
    const CALL: &str = "let (gx, gy, gw, gh) = fit_single_cell_status_marker(";
    const PUSH: &str = "glyph_instances.push(GlyphInstance";
    let fallback_start = SOURCE.find("if g.glyph_id == 0 {").expect("fallback branch");
    let shaped_offset = SOURCE[fallback_start..]
        .find("let key = sonicterm_types::glyph_key::GlyphKey::shaped(")
        .expect("shaped branch");
    let shaped_start = fallback_start + shaped_offset;

    assert_eq!(SOURCE.matches(CALL).count(), 2);
    for section in [&SOURCE[fallback_start..shaped_start], &SOURCE[shaped_start..]] {
        let fit = section.find(CALL).expect("shared marker fit call");
        let push = section.find(PUSH).expect("glyph instance emission");
        assert!(section.contains("lead_cell.extras().is_some()"));
        assert!(fit < push, "marker fitting must precede GlyphInstance creation");
    }
}

/// Glyph flags preserve color selection in x and raw subpixel coverage in y.
#[test]
fn glyph_flags_keep_color_and_subpixel_axes_independent() {
    assert_eq!(glyph_flags(false, false), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(true, false), [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(glyph_flags(false, true), [0.0, 1.0, 0.0, 0.0]);
}

/// Windows software glyph drawing continues to consume the original CPU atlas bytes directly.
#[test]
fn windows_software_presenter_keeps_cpu_glyph_source_byte_compatible() {
    const SOURCE: &str = include_str!("software_windows.rs");

    assert!(SOURCE.contains("let atlas_pixels = atlas.pixels_bgra();"));
    assert!(SOURCE.contains("if color_glyph"));
    assert!(SOURCE.contains("blend_premul_bgra(&mut self.pixels[dst_off..dst_off + 4], sample);"));
    assert!(!SOURCE.contains("copy_rect_into_scratch"));
}

/// Atlas roles keep linear-filtered images separate from dual-view nearest glyph sampling.
#[test]
fn atlas_sync_and_bind_groups_are_wired_by_role() {
    const SOURCE: &str = include_str!("core.rs");
    let source = SOURCE.replace("\r\n", "\n");

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
        raster_variant: GlyphRasterVariant::Normal,
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
    let baseline = FrameKey { pane_scrollbar_alpha: vec![(7, 0)], ..Default::default() };
    let visible = FrameKey { pane_scrollbar_alpha: vec![(7, u16::MAX)], ..baseline.clone() };

    assert_ne!(baseline, visible);
}

/// Changing process privilege must invalidate otherwise-identical retained tab chrome.
#[test]
fn process_privilege_change_invalidates_the_frame_key() {
    let ordinary = FrameKey { process_privileged: false, ..Default::default() };
    let privileged = FrameKey { process_privileged: true, ..ordinary.clone() };

    assert_ne!(ordinary, privileged);
}

/// Process elevation warns every tab, while foreground elevation warns only its owning tab.
#[test]
fn privilege_badge_combines_process_and_per_tab_foreground_state() {
    assert!(!tab_requires_privilege_badge(false, false));
    assert!(tab_requires_privilege_badge(true, false));
    assert!(tab_requires_privilege_badge(false, true));
    assert!(tab_requires_privilege_badge(true, true));
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

/// Privileged title layout reserves a bounded lock slot while ordinary capacity stays unchanged.
#[test]
fn privilege_badge_reserves_title_width_without_leaving_the_title_rect() {
    let rect = TabTitleRect { x: 20.0, y: 4.0, w: 180.0, h: 36.0 };
    let ordinary = tab_title_capacity(rect, 10.0, false, 1.0);
    let privileged = tab_title_capacity(rect, 10.0, true, 1.0);
    let ordinary_placement = tab_title_block_placement(rect, 92.0, false, 1.0);
    let placement = tab_title_block_placement(rect, 92.0, true, 1.0);
    let badge = placement.badge_rect.expect("privileged title has a badge");

    assert_eq!(ordinary, 18);
    assert_eq!(ordinary_placement.badge_rect, None);
    assert_eq!(ordinary_placement.text_x, 64.0);
    assert_eq!(ordinary_placement.text_clip, rect);
    assert!(privileged < ordinary);
    assert!(badge.x >= rect.x && badge.y >= rect.y);
    assert!(badge.x + badge.w <= rect.x + rect.w);
    assert!(badge.y + badge.h <= rect.y + rect.h);
    assert!(placement.text_x >= badge.x + badge.w);
    assert!(placement.text_clip.x >= badge.x + badge.w);
    assert!(placement.text_clip.x + placement.text_clip.w <= rect.x + rect.w);
}

/// Privileged truncation keeps the stored index and running-process glyph ahead of the body.
#[test]
fn privileged_title_text_preserves_existing_identity_and_command_status() {
    let folder = '\u{f07b}';
    let stored = format!("#12 {folder} workspace/project");

    let display = tab_title_display_text(&stored, Some("✓"), 14);

    assert_eq!(display, format!("✓ #12 {folder} works…"));
    assert_eq!(stored, format!("#12 {folder} workspace/project"));
    assert_eq!(tab_title_display_text("abcdefgh", None, 5), "abcd…");
    assert_eq!(tab_title_display_text(&stored, Some("✓"), 5), "✓ #1…");
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
    let mut frame =
        crate::software_windows::WindowsSoftwareFrame::new(40, 40, [0.0, 0.0, 0.0, 1.0])
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
        .find("\n    /// Apply a new DPI scale factor")
        .map(|offset| set_font_start + offset)
        .expect("set_font has a bounded body");
    let set_font = &CORE_SRC[set_font_start..set_font_end];
    assert!(
        set_font.contains("renderer_font_stacks(family, size, dpi, weight_scale, &self.font_dirs)")
    );
    for assignment in [
        "self.font_stack = new_stacks.body;",
        "self.tab_title_font_stack = new_stacks.tab_title;",
        "self.palette_footer_font_stack = new_stacks.palette_footer;",
    ] {
        assert!(set_font.contains(assignment), "missing stack replacement: {assignment}");
    }
}

#[test]
fn title_size_helper_is_used_only_for_title_stack_and_title_rendering() {
    // Protect title sizing from body/footer leaks while keeping vector privilege chrome font-independent.
    const CORE_SRC: &str = include_str!("core.rs");
    assert_eq!(CORE_SRC.matches("tab_title_font_size(").count(), 2);
    assert!(CORE_SRC.contains("self.tab_title_font_stack.as_ref(), tab_rasterizer.as_mut()"));
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
    for mode in
        [CommandPaletteMode::Commands, CommandPaletteMode::RenameTab, CommandPaletteMode::TabColor]
    {
        let mut palette = CommandPalette::new();
        match mode {
            CommandPaletteMode::Commands => palette.open(),
            CommandPaletteMode::RenameTab => palette.start_rename_tab("tab"),
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

    let available = supported_width - PALETTE_FOOTER_INSET_X;
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
    let baseline = 41;
    let first = hovered_url_for_pane_row(Some(hovered), 7, 2).unwrap();
    let second = hovered_url_for_pane_row(Some(hovered), 7, 3).unwrap();

    assert_ne!(hovered_url_row_cache_key(baseline, Some(first), 2), baseline);
    assert_ne!(hovered_url_row_cache_key(baseline, Some(second), 3), baseline);
    assert_ne!(
        hovered_url_row_cache_key(baseline, Some(first), 2),
        hovered_url_row_cache_key(baseline, Some(second), 3)
    );
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

    assert_eq!(hovered_url_row_cache_key(41, Some(row), 3), 41);
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
