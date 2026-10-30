//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// A visible window and a counting wgpu renderer on it with a translucent background, line
/// height 1 (so the vertical ink pad is about one row), no scrollbar and no tab bar; `Err` names a
/// host with no adapter as `HOST_INCAPABLE`.
pub(super) fn wgpu_renderer(
    active: &ActiveEventLoop,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    let (window, renderer) = renderer_in_mode(active, SoftwareRenderMode::Off)?;
    check(!renderer.is_software_render_degraded(), "the renderer presents through wgpu")?;
    Ok((window, renderer))
}

/// A visible window and a counting renderer on it in software render `mode`, configured as
/// [`wgpu_renderer`] configures one; `Err` names a host with no adapter as `HOST_INCAPABLE`.
pub(super) fn renderer_in_mode(
    active: &ActiveEventLoop,
    mode: SoftwareRenderMode,
) -> Result<(Arc<Window>, GpuRenderer), String> {
    // Logical size: the 16 pt font rasterizes at the window's scale, so the pane keeps the same
    // row count (at least the 20 the cases need) on every host DPI.
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(LogicalSize::new(640.0, 480.0))
                    .with_visible(true)
                    .with_title("partial-assembly"),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs =
        [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono")];
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: 16.0,
        line_height_mult: 1.0,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: OPACITY,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: mode,
        },
        role: "partial-assembly",
        glyph_atlas_start: GlyphAtlasStart::Normal,
    };
    let mut renderer = match GpuRenderer::new(window.clone(), active, &Theme::default(), settings) {
        Ok(renderer) => renderer,
        Err(error) if host_has_no_adapter() => {
            // When: host_has_no_adapter holds, no wgpu renderer can exist on this host.
            return Err(format!("HOST_INCAPABLE: no wgpu adapter: {error}"));
        }
        Err(error) => return Err(format!("wgpu renderer construction failed: {error}")),
    };
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    renderer.set_cursor_shape(CursorShape::Block);
    renderer.set_window_focused(true);
    renderer.set_frame_counting(true);
    renderer.__enable_retained_frame_readback();
    renderer.__enable_presented_damage();
    Ok((window, renderer))
}
