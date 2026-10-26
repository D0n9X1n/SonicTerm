//! Renderer font contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn body_title_and_footer_stacks_share_configuration_and_native_size_identity() {
    // Contract: chrome roles share font configuration but retain distinct native size identities.
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
    let helper_start = CORE_SRC.find("fn renderer_font_stacks(").expect("stack builder");
    let helper_end = CORE_SRC[helper_start..]
        .find("fn software_block_glyph_target_rect(")
        .map(|offset| helper_start + offset)
        .expect("stack builder end");
    let helper = &CORE_SRC[helper_start..helper_end];
    assert_eq!(helper.matches("try_new_full_with_weight_and_font_dirs(").count(), 1);
    assert!(helper.contains("font_dirs"));
    // Tab title, palette footer and status line are sibling views of the one body stack.
    assert_eq!(helper.matches("with_font_size(").count(), 3);

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
        "self.status_line_font_stack = new_stacks.status_line;",
    ] {
        assert!(set_font.contains(assignment), "missing stack replacement: {assignment}");
    }
    // The scale rebuild rescales the tab-title font through the same device-free state.
    assert!(CORE_SRC.contains("self.tab_title_font.set_scale_factor(sf, fs_dpi);"));
}

#[test]
fn title_size_helper_is_used_only_for_title_stack_and_title_rendering() {
    // Protect title sizing from body/footer leaks while keeping vector privilege chrome font-independent.
    const CORE_SRC: &str = crate::core::CORE_SOURCE;
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
        CommandPaletteMode::Confirm,
        CommandPaletteMode::StatusSegments,
        CommandPaletteMode::FontFamily,
    ] {
        let mut palette = CommandPalette::new();
        match mode {
            CommandPaletteMode::Commands => palette.open(),
            CommandPaletteMode::RenameTab => palette.start_rename_tab("tab"),
            CommandPaletteMode::RenameWindow => palette.start_rename_window("window"),
            CommandPaletteMode::TabColor => palette.start_tab_color_picker("tab", Vec::new()),
            CommandPaletteMode::Confirm => {
                palette.start_confirm("title", "Apply", "Cancel", Vec::new())
            }
            // The checklist footer comes from the host; check the longest localized one.
            CommandPaletteMode::StatusSegments => palette.start_status_segments(
                "title",
                longest_localized_footer("status-checklist-footer"),
                Vec::new(),
                None,
            ),
            // The font picker footer also comes from the host.
            CommandPaletteMode::FontFamily => palette.start_font_family(
                "title",
                longest_localized_footer("font-picker-footer"),
                Vec::new(),
                "",
            ),
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
fn face_replacement_sites_clear_both_caches() {
    // Every place the renderer replaces faces without a title-key change drops the kept chrome
    // runs and titles beside its row-cache invalidation; `set_font` reaches the clear only
    // through `adopt_font_stacks`, which the test adoption seam shares. Scanned CRLF-normalized.
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    for signature in ["fn adopt_font_stacks(", "fn rebuild_for_sf(", "pub fn clear_shape_cache("] {
        let body = item_body(&source, signature);
        assert!(
            body.contains("self.chrome_caches.clear_runs();"),
            "{signature} clears both caches"
        );
        assert!(body.contains("row_glyph_cache.invalidate_all()"), "{signature} beside the rows");
    }
    let set_font = item_body(&source, "pub fn set_font(");
    assert!(set_font.contains("self.adopt_font_stacks("), "set_font adopts through the seam");
    assert!(!set_font.contains("clear_runs"), "and does not clear on its own");
    let seam = item_body(&source, "pub fn __test_adopt_body_font_stack(");
    assert!(seam.contains("self.adopt_font_stacks("), "the test seam shares the clear");
}
