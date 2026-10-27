//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// A strip moved from the top to the bottom keeps routing to the band still on screen when the
/// frame at the new edge fails to present; the next presented frame commits the bottom band,
/// and the insets reserve it there, so no pane area overlaps it.
pub(super) fn moved_strip_routes_until_presented(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    _active: &ActiveEventLoop,
) -> Result<(), String> {
    use sonicterm_render_model::boundary::cfg::status_line::StatusPosition;
    let mut scene = single(layout);
    renderer.set_status_line_position(StatusPosition::Top);
    renderer.set_status_line_visible(true);
    present(renderer, &mut scene)?;
    let top = renderer.status_line_hit().ok_or("the top strip presented")?;
    let top_end = top.top + top.height;
    check(
        top_end <= renderer.top_inset() + 0.5,
        &format!("the panes start at {} below the top band ending {top_end}", renderer.top_inset()),
    )?;
    check(renderer.set_status_line_position(StatusPosition::Bottom), "a shown strip moves")?;
    renderer.__fail_next_present();
    let outcome = frame(renderer, &mut scene);
    check(matches!(outcome, PresentOutcome::Failed(_)), &format!("Failed: {outcome:?}"))?;
    let kept = renderer.status_line_hit().ok_or("the failed frame keeps the presented strip")?;
    check(kept.top == top.top, &format!("still the top band: {} vs {}", kept.top, top.top))?;
    present(renderer, &mut scene)?;
    let bottom = renderer.status_line_hit().ok_or("the bottom strip presented")?;
    let surface_h = layout.pane.h as f32;
    check(bottom.top > top.top, &format!("the band moved down: {}", bottom.top))?;
    let panes_bottom = surface_h - renderer.bottom_inset();
    check(
        bottom.top >= panes_bottom - 0.5,
        &format!("panes end at {panes_bottom} above the band at {}", bottom.top),
    )?;
    check(
        renderer.top_inset() < top_end,
        &format!("the top inset {} gave the strip's rows back", renderer.top_inset()),
    )?;
    renderer.set_status_line_visible(false);
    renderer.set_status_line_position(StatusPosition::Top);
    present(renderer, &mut scene)?;
    Ok(())
}

/// The strip tiles `renderer`'s atlas holds, each as `(glyph, width, height)` of its atlas rect.
pub(super) fn strip_tiles(renderer: &GpuRenderer) -> Vec<(sonicterm_types::GlyphKey, u32, u32)> {
    renderer.__test_resident_tile_sizes(sonicterm_types::GlyphRasterVariant::StatusLine)
}

/// The strip showing `view`, presented by a cold renderer built at `family`, `size_pt`,
/// `scale_factor` and `font_scale`: the tiles a renderer that never drew another size holds.
pub(super) fn cold_strip_tiles(
    active: &ActiveEventLoop,
    view: &sonicterm_render_model::boundary::ui::status_line::StatusLineView,
    (family, size_pt, scale_factor, font_scale): (&str, f32, f32, f32),
) -> Result<Vec<(sonicterm_types::GlyphKey, u32, u32)>, String> {
    let (window, mut cold) = wgpu_renderer(active)?;
    cold.set_font(family, size_pt, 1.0, 1.0);
    cold.set_scale_factor(scale_factor);
    cold.set_status_line_font_scale(font_scale);
    cold.set_status_line_view(view.clone());
    cold.set_status_line_visible(true);
    let mut scene = single(&layout(&cold, &window));
    present(&mut cold, &mut scene)?;
    check(cold.last_missing_chrome().is_empty(), "the cold strip drew every glyph")?;
    Ok(strip_tiles(&cold))
}

/// Through the real renderer: a warm strip, then a zoom, a DPI change and a `font_scale` change.
/// After each, every strip tile resident in the atlas has the atlas-rect size a cold renderer
/// built at that final size draws for the same text, and no other strip tile is resident, so a
/// tile of an earlier size can neither stay resident nor be drawn.
///
/// It runs on its own renderer and window: the font, scale and strip changes and the cold
/// renderers it builds must not reach the shared renderer the failure cases after it rely on.
pub(super) fn strip_rasterizes_anew_after_size_changes(
    _shared: &mut GpuRenderer,
    _shared_layout: &Layout,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    use sonicterm_render_model::boundary::cfg::status_line::{StatusSegmentId, StatusSide};
    use sonicterm_render_model::boundary::ui::status_line::{
        SpokenSegment, StatusLineView, StatusPart, StatusSegmentView, StatusTone,
    };
    let (window, mut owned) = wgpu_renderer(active)?;
    let renderer = &mut owned;
    let own_layout = layout(renderer, &window);
    let default_scale = sonicterm_gpu::status_strip::default_font_scale();
    let (family, size_pt, scale_factor) =
        ("Rec Mono St.Helens", renderer.font_size(), renderer.scale_factor());
    let view = StatusLineView::new(vec![StatusSegmentView {
        id: StatusSegmentId::Mode,
        side: StatusSide::Left,
        priority: 500,
        protected: true,
        parts: vec![StatusPart::new("HELLO", StatusTone::Value)],
        compact: None,
        click: None,
        spoken: SpokenSegment::default(),
    }]);
    let mut scene = single(&own_layout);
    renderer.set_status_line_view(view.clone());
    renderer.set_status_line_visible(true);
    let mut previous: Option<Vec<(sonicterm_types::GlyphKey, u32, u32)>> = None;
    let mut step = |renderer: &mut GpuRenderer, label: &str, font_scale: f32| {
        present(renderer, &mut scene)?;
        check(
            renderer.last_missing_chrome().is_empty(),
            &format!("{label}: the strip drew a glyph as missing"),
        )?;
        let mut live = strip_tiles(renderer);
        check(!live.is_empty(), &format!("{label}: the strip holds tiles"))?;
        let target = (family, renderer.font_size(), renderer.scale_factor(), font_scale);
        let cold = cold_strip_tiles(active, &view, target)?;
        sonicterm_gpu::status_strip::compare_strip_tile_sizes(&live, &cold)
            .map_err(|error| format!("{label}: {error}"))?;
        // Each change must reach the tiles, or the comparison above proved nothing about it.
        live.sort_by_key(|(key, width, height)| (key.ch, key.weight_bold, *width, *height));
        check(previous.as_ref() != Some(&live), &format!("{label}: the tiles kept their size"))?;
        previous = Some(live);
        Ok::<(), String>(())
    };
    step(renderer, "warm", default_scale)?;
    renderer.set_font(family, size_pt + 4.0, 1.0, 1.0);
    step(renderer, "zoom", default_scale)?;
    renderer.set_scale_factor(scale_factor * 2.0);
    step(renderer, "DPI", default_scale)?;
    check(renderer.set_status_line_font_scale(0.5), "a new font_scale changes the strip")?;
    step(renderer, "font_scale", 0.5)?;
    Ok(())
}
