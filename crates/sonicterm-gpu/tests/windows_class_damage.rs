//! Narrow class damage on a real wgpu renderer, read back from its retained frame.
//!
//! Setters keep the key: a cursor-shape, cursor-blink or window-focus change leaves the retained
//! frame key set, so the next frame is not a first frame and damages only its class.
//!
//! Retained-pixel parity: each narrowed case presents with damage smaller than the surface, and
//! its retained pixels equal a full repaint of the same state, over a translucent background.
//!
//! Only the event-loop entry point is Windows-only (winit allows a test-thread event loop there);
//! the case logic compiles on every host, so a non-Windows lint pass type-checks it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::{path::PathBuf, sync::Arc};

use sonicterm_gpu::core::{
    unpad_readback_rows, GlyphAtlasStart, GpuRenderer, PresentOutcome, PresentedDamage,
    RendererSettings, SurfaceAppearance,
};
use sonicterm_render_model::{
    boundary::{
        cfg::{
            config::{CursorShape, ScrollbarMode, SoftwareRenderMode},
            theme::Theme,
        },
        grid::grid::{CellFlags, Color, Grid},
        ui::{
            selection::Selection,
            tabs::{Tab, TabBar},
        },
    },
    CursorStyle, PaneRender, PixelRect,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

/// The one pane every case draws.
const PANE_ID: u64 = 1;
/// The terminal background opacity: below 1, so every case composes over a translucent ground.
const OPACITY: f32 = 0.6;
/// Row pitch as a share of the font height, so glyph ink overhangs into the next row.
const LINE_HEIGHT: f32 = 0.6;
/// Right padding in logical pixels, so the scrollbar track sits inside it, not at the edge.
const RIGHT_PADDING: f32 = 12.0;
/// The cursor's row: low enough that a glyph 10/3 rows tall still fits above it.
const CURSOR_ROW: u16 = 4;
/// The cursor's column: past the two glyphs of its row, so it rests on a blank cell.
const CURSOR_COL: u16 = 4;
/// The row holding wide CJK characters and combining marks for the selection case.
const WIDE_ROW: u16 = 2;
/// The injected glyph's color: opaque, and unlike any theme color, so stale ink is visible.
const INJECTED_COLOR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so every case runs inside this one.
        self.outcome = Some(run_cases(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// Whether this host enumerates no wgpu adapter at all: the only reason to skip.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// Pixel geometry every case shares: the pane, its text grid's origin and the cell size.
#[derive(Clone, Copy)]
struct Layout {
    pane: PixelRect,
    grid_x: f32,
    grid_y: f32,
    cell_w: f32,
    cell_h: f32,
    cols: u16,
    rows: u16,
    /// The tab bar's top edge; the bar is pinned to the surface bottom.
    tab_bar_top: f32,
    /// The right edge of the pane's padded chrome, where the scrollbar track ends.
    chrome_right: f32,
    scale: f32,
}

impl Layout {
    /// The drawn cursor cell `(x, y, w, h)` in surface pixels.
    fn cursor_rect(&self) -> (f32, f32, f32, f32) {
        (
            self.grid_x + f32::from(CURSOR_COL) * self.cell_w,
            self.grid_y + f32::from(CURSOR_ROW) * self.cell_h,
            self.cell_w,
            self.cell_h,
        )
    }

    /// A glyph rectangle under the cursor, 10/3 rows tall and bottom-aligned with it: the cursor
    /// covers 30% of it, so a block cursor recolors all of it, and its top reaches more than one
    /// font height above the cursor row at the compressed pitch.
    fn tall_glyph(&self) -> (f32, f32, f32, f32) {
        let (left, top, width, height) = self.cursor_rect();
        let glyph_h = height * 10.0 / 3.0;
        (left, top + height - glyph_h, width, glyph_h)
    }
}

/// What a case draws, besides the renderer's own state.
struct Scene {
    grid: Grid,
    cursor_visible: bool,
    selection: Option<Selection>,
    tabs: TabBar,
    scrollbar_alpha: f32,
}

/// A visible window and a wgpu renderer on it with a translucent background, a compressed row
/// pitch, right padding, an auto scrollbar and the tab bar shown; `Err` names a host with no
/// adapter as `HOST_INCAPABLE`.
fn wgpu_renderer(active: &ActiveEventLoop) -> Result<(Arc<Window>, GpuRenderer), String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(480, 320))
                    .with_visible(true)
                    .with_title("class-damage"),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs = [PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: 16.0,
        line_height_mult: LINE_HEIGHT,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0, RIGHT_PADDING, 0.0, 0.0],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: OPACITY,
            scrollbar: ScrollbarMode::Auto,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Off,
        },
        role: "class-damage",
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
    check(!renderer.is_software_render_degraded(), "the renderer presents through wgpu")?;
    renderer.set_tab_bar_visible(true);
    renderer.set_cursor_blink(false);
    renderer.set_cursor_shape(CursorShape::Block);
    renderer.set_window_focused(true);
    renderer.__enable_retained_frame_readback();
    renderer.__enable_presented_damage();
    Ok((window, renderer))
}

/// The pane above the tab bar, and where its rows and scrollbar draw.
fn layout(renderer: &GpuRenderer, window: &Window) -> Layout {
    let size = window.inner_size();
    let (cell_w, cell_h) = renderer.cell_size();
    let tab_bar_top = renderer.tab_bar_y_offset();
    let pane = PixelRect { x: 0, y: 0, w: size.width, h: tab_bar_top.floor() as u32 };
    let padding = [
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.padding_top_px(),
        renderer.padding_bottom_px(),
    ];
    let content_w = pane.w as f32 - padding[0] - padding[1];
    let content_h = pane.h as f32 - padding[2] - padding[3];
    let cols = ((content_w / cell_w).floor() as u16).max(1);
    let rows = ((content_h / cell_h).floor() as u16).max(1);
    let geometry = sonicterm_render_model::pane_content_geometry(pane, padding, cell_h, rows);
    Layout {
        pane,
        grid_x: geometry.grid.x,
        grid_y: geometry.grid.y,
        cell_w,
        cell_h,
        cols,
        rows,
        tab_bar_top,
        chrome_right: geometry.content.x + geometry.content.w,
        scale: renderer.scale_factor(),
    }
}

/// A grid with scrollback (so the scrollbar can draw), every row full of dense glyphs except the
/// cursor row, which holds two, a row of wide CJK characters with combining marks, and the
/// cursor parked on a blank cell of its row. Its dirt is cleared by the first presented frame.
fn scene(layout: &Layout) -> Scene {
    let mut grid = Grid::new(layout.cols, layout.rows);
    for _ in 0..u32::from(layout.rows) * 3 {
        grid.linefeed();
    }
    for row in 0..layout.rows {
        grid.goto(row, 0);
        let text: String = if row == CURSOR_ROW {
            "MW".to_owned()
        } else if row == WIDE_ROW {
            "中文e\u{301}字a\u{308}漢M".to_owned()
        } else {
            "MW".repeat(usize::from(layout.cols) / 2)
        };
        for character in text.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    grid.goto(CURSOR_ROW, CURSOR_COL);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    Scene { grid, cursor_visible: true, selection: None, tabs, scrollbar_alpha: 0.0 }
}

/// Draw `scene` once through the compatibility path, which applies its receipts to the grid.
fn frame(renderer: &mut GpuRenderer, layout: &Layout, scene: &mut Scene) -> PresentOutcome {
    let theme = Theme::default();
    let fonts = renderer.begin_frame_fonts();
    let mut panes = [PaneRender {
        id: PANE_ID,
        rect_px: layout.pane,
        grid: &mut scene.grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: scene.scrollbar_alpha,
        inline_images: Vec::new(),
    }];
    renderer.render_with_outcome(
        &fonts,
        &mut panes,
        &theme,
        scene.cursor_visible,
        scene.selection.as_ref(),
        None,
        &scene.tabs,
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// Draw `scene` until a frame presents, at most four tries, and return that frame's damage. A
/// retry plans against the same presented key, so it carries the same damage.
fn present(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    scene: &mut Scene,
) -> Result<PresentedDamage, String> {
    for _ in 0..4 {
        if matches!(frame(renderer, layout, scene), PresentOutcome::Presented) {
            return renderer.__take_presented_damage().ok_or_else(|| {
                String::from("a presented frame records its damage beside the frame key")
            });
        }
    }
    Err(String::from("the frame never presented"))
}

/// Start a case from a full repaint of `scene`, so the next frame is narrowed against it.
fn baseline(renderer: &mut GpuRenderer, layout: &Layout, scene: &mut Scene) -> Result<(), String> {
    renderer.invalidate_retained_frame();
    let damage = present(renderer, layout, scene)?;
    check(damage.first_frame, "the baseline is a whole-surface first frame")
}

/// The retained GPU frame's tightly packed BGRA pixels and its width. This test maps and polls;
/// the renderer never does.
fn retained_pixels(renderer: &mut GpuRenderer) -> Result<(Vec<u8>, u32), String> {
    let readback =
        renderer.__copy_retained_frame().ok_or("readback is enabled and the device works")?;
    let slice = readback.buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    readback
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll the retained-frame readback: {error}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|error| format!("map the retained-frame readback: {error}"))?;
    let pixels = unpad_readback_rows(
        &mapped,
        readback.width_px,
        readback.height_px,
        readback.padded_row_bytes,
    );
    drop(mapped);
    readback.buffer.unmap();
    Ok((pixels, readback.width_px))
}

/// The first pixel `(x, y)` where two equally sized frames differ, if any.
fn first_difference(first: &[u8], second: &[u8], width_px: u32) -> Option<(usize, usize)> {
    let width = width_px as usize;
    first
        .chunks(4)
        .zip(second.chunks(4))
        .position(|(left, right)| left != right)
        .map(|index| (index % width, index / width))
}

/// Present `scene` with narrow damage, then repaint the same state in full, and require the two
/// retained frames equal byte for byte. Returns the narrow frame's damage and the full pixels.
fn narrow_matches_full(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    scene: &mut Scene,
    case: &str,
) -> Result<(PresentedDamage, Vec<u8>, u32), String> {
    let narrow = present(renderer, layout, scene)?;
    check(
        narrow.is_narrow(),
        &format!("{case}: the frame damages less than the surface: {narrow:?}"),
    )?;
    let (narrow_pixels, width_px) = retained_pixels(renderer)?;
    renderer.invalidate_retained_frame();
    let full = present(renderer, layout, scene)?;
    check(full.first_frame, &format!("{case}: the comparison frame repaints in full"))?;
    let (full_pixels, _) = retained_pixels(renderer)?;
    check(
        narrow_pixels.len() == full_pixels.len(),
        &format!("{case}: both frames read back at one size"),
    )?;
    let difference = first_difference(&narrow_pixels, &full_pixels, width_px);
    check(
        difference.is_none(),
        &format!(
            "{case}: narrow damage {:?} equals a full repaint; first differing pixel {difference:?}",
            narrow.damage
        ),
    )?;
    // The background is translucent, so the cleared ground keeps an alpha below opaque.
    let corner = &full_pixels[..4];
    check(corner[3] < u8::MAX, &format!("{case}: the background is translucent: {corner:?}"))?;
    Ok((narrow, full_pixels, width_px))
}

/// Whether `damage` covers rows `top..bottom` (surface pixels, `top` rounded down).
fn covers_rows(damage: PixelRect, top: f32, bottom: f32) -> bool {
    damage.y <= top.floor() as i32 && damage.bottom() >= bottom.ceil() as i32
}

/// Whether two full frames differ anywhere in rows `top..bottom`.
fn rows_differ(first: &[u8], second: &[u8], width_px: u32, top: f32, bottom: f32) -> bool {
    let row_bytes = width_px as usize * 4;
    let start = (top.max(0.0).floor() as usize) * row_bytes;
    let end = ((bottom.max(0.0).ceil() as usize) * row_bytes).min(first.len()).min(second.len());
    start < end && first[start..end] != second[start..end]
}

/// Test 6: the cursor-shape, cursor-blink and focus setters keep the frame key, so the next
/// frame is not a first frame, and its damage is only the setter's class: the cursor row for
/// shape and blink, and the cursor row through the bottom tab band for focus.
fn setters_keep_the_key(renderer: &mut GpuRenderer, layout: &Layout) -> Result<(), String> {
    let mut scene = scene(layout);
    baseline(renderer, layout, &mut scene)?;
    let (_, cursor_top, _, cursor_h) = layout.cursor_rect();
    type Setter = fn(&mut GpuRenderer);
    let cases: [(&str, Setter, bool); 6] = [
        ("cursor shape", |renderer| renderer.set_cursor_shape(CursorShape::Bar), false),
        ("cursor shape back", |renderer| renderer.set_cursor_shape(CursorShape::Block), false),
        ("cursor blink", |renderer| renderer.set_cursor_blink(true), false),
        ("cursor blink back", |renderer| renderer.set_cursor_blink(false), false),
        ("window unfocused", |renderer| renderer.set_window_focused(false), true),
        ("window focused", |renderer| renderer.set_window_focused(true), true),
    ];
    for (name, setter, reaches_tab_band) in cases {
        setter(renderer);
        check(renderer.__test_has_frame_key(), &format!("{name}: the setter keeps the frame key"))?;
        let damage = present(renderer, layout, &mut scene)?;
        check(!damage.first_frame, &format!("{name}: the next frame is not a first frame"))?;
        check(damage.is_narrow(), &format!("{name}: the damage is narrow: {damage:?}"))?;
        check(
            covers_rows(damage.damage, cursor_top, cursor_top + cursor_h),
            &format!("{name}: the damage covers the cursor row: {damage:?}"),
        )?;
        if reaches_tab_band {
            // When: focus is the class, its damage runs from the cursor row through the tab band.
            check(
                damage.damage.bottom() == damage.surface.bottom(),
                &format!("{name}: focus damages the tab band: {damage:?}"),
            )?;
        } else {
            // When: shape or blink is the class, its damage stays above the tab band.
            check(
                (damage.damage.bottom() as f32) <= layout.tab_bar_top,
                &format!("{name}: the cursor class leaves the tab band alone: {damage:?}"),
            )?;
        }
    }
    Ok(())
}

/// Test 8: every narrowed class repaints the same pixels as a full repaint of its state.
fn retained_pixel_parity(renderer: &mut GpuRenderer, layout: &Layout) -> Result<(), String> {
    let (_, cursor_top, _, cursor_h) = layout.cursor_rect();
    let tall = layout.tall_glyph();

    // A cursor toggle under glyph ink that overhangs from the row above at the compressed pitch.
    let mut toggle = scene(layout);
    baseline(renderer, layout, &mut toggle)?;
    for visible in [false, true] {
        toggle.cursor_visible = visible;
        narrow_matches_full(renderer, layout, &mut toggle, "cursor toggle")?;
    }

    // A focus change repaints the cursor and the tab bar's active marker.
    let mut focus = scene(layout);
    baseline(renderer, layout, &mut focus)?;
    for focused in [false, true] {
        renderer.set_window_focused(focused);
        narrow_matches_full(renderer, layout, &mut focus, "focus change")?;
    }

    // A tab-title change repaints the band.
    let mut retitle = scene(layout);
    baseline(renderer, layout, &mut retitle)?;
    let tab_id = retitle.tabs.tabs()[0].id;
    retitle.tabs.set_title(tab_id, "a much longer title");
    narrow_matches_full(renderer, layout, &mut retitle, "tab title")?;

    // A scrollbar fade over text, in a pane with right padding, then a fade below the emit floor:
    // the damage is the drawn track inside the padding, not a strip at the surface edge.
    let mut fade = scene(layout);
    fade.scrollbar_alpha = 1.0;
    baseline(renderer, layout, &mut fade)?;
    let track_w = (8.0 * layout.scale).max(1.0);
    for alpha in [0.5, 0.0] {
        fade.scrollbar_alpha = alpha;
        let (damage, ..) = narrow_matches_full(renderer, layout, &mut fade, "scrollbar fade")?;
        let damage = damage.damage;
        check(
            (damage.right() - layout.chrome_right.ceil() as i32).abs() <= 1
                && (damage.x - (layout.chrome_right - track_w).floor() as i32).abs() <= 1,
            &format!(
                "scrollbar fade to {alpha}: damage {damage:?} is the track ending at {}",
                layout.chrome_right
            ),
        )?;
    }

    // The compressed-row cursor case: a recolored glyph reaching more than one font height above
    // the cursor row. Toggling the cursor must repaint those rows.
    let mut compressed = scene(layout);
    renderer.__inject_test_glyph(Some((tall, INJECTED_COLOR)));
    baseline(renderer, layout, &mut compressed)?;
    let mut full_frames = Vec::new();
    for visible in [false, true] {
        compressed.cursor_visible = visible;
        let (damage, pixels, width_px) =
            narrow_matches_full(renderer, layout, &mut compressed, "compressed-row cursor")?;
        check(
            covers_rows(damage.damage, tall.1, cursor_top + cursor_h),
            &format!("compressed-row cursor: damage {damage:?} reaches the glyph top {}", tall.1),
        )?;
        full_frames.push((pixels, width_px));
    }
    // The case is only meaningful if those rows really change with the cursor.
    let (off, width_px) = &full_frames[0];
    let (on, _) = &full_frames[1];
    check(
        rows_differ(off, on, *width_px, tall.1, tall.1 + cursor_h * 0.5),
        "compressed-row cursor: the rows above the pad hold recolored ink",
    )?;

    // The recolored glyph changes through its row's dirt with no class change, the cursor
    // staying put. Each step starts from its own baseline of the state it changes from: a tall
    // glyph shrinking to the cursor's size, a tall glyph removed, and a tall glyph written where
    // there was none. The tall glyph's top must be damaged only when one side draws it.
    let tall_glyph = Some(tall);
    for (step, before, after) in [
        ("tall to short", tall_glyph, Some(layout.cursor_rect())),
        ("tall to absent", tall_glyph, None),
        ("absent to tall", None, tall_glyph),
    ] {
        let mut shrink = scene(layout);
        renderer.__inject_test_glyph(before.map(|rect| (rect, INJECTED_COLOR)));
        baseline(renderer, layout, &mut shrink)?;
        renderer.__inject_test_glyph(after.map(|rect| (rect, INJECTED_COLOR)));
        // Rewrite a cell of the cursor's row so the row is dirty, then park the cursor again.
        shrink.grid.goto(CURSOR_ROW, 0);
        shrink.grid.put_char('M', Color::Default, Color::Default, CellFlags::empty());
        shrink.grid.goto(CURSOR_ROW, CURSOR_COL);
        let (damage, ..) = narrow_matches_full(renderer, layout, &mut shrink, step)?;
        if before == tall_glyph || after == tall_glyph {
            // When: either frame draws the tall glyph, its top above the row pad must repaint.
            check(
                covers_rows(damage.damage, tall.1, cursor_top + cursor_h),
                &format!("{step}: damage {damage:?} reaches the tall glyph's top {}", tall.1),
            )?;
        }
    }
    renderer.__inject_test_glyph(None);

    // A selection extension across wide CJK characters and combining marks.
    let mut select = scene(layout);
    let wide_row = select.grid.scrollback_len() as u64 + u64::from(WIDE_ROW);
    let mut selection = Selection::new(wide_row, 0);
    selection.extend(wide_row, 1);
    select.selection = Some(selection);
    baseline(renderer, layout, &mut select)?;
    for end_col in [3, 6] {
        if let Some(selection) = select.selection.as_mut() {
            selection.extend(wide_row, end_col);
        }
        narrow_matches_full(renderer, layout, &mut select, "wide selection")?;
    }
    Ok(())
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let (window, mut renderer) = match wgpu_renderer(active) {
        Ok(built) => built,
        Err(reason) if reason.starts_with("HOST_INCAPABLE") => {
            // When: the host has no adapter, there is no retained wgpu frame to compare.
            println!("capability={reason}");
            return Ok(());
        }
        Err(reason) => return Err(reason),
    };
    let layout = layout(&renderer, &window);
    // WIDE_ROW is above CURSOR_ROW, so a pane holding the cursor row holds both case rows.
    check(layout.rows > CURSOR_ROW, "the pane holds the case rows")?;
    let mut failures = Vec::new();
    for (name, case) in [
        ("setters keep the key", setters_keep_the_key as fn(&mut GpuRenderer, &Layout) -> _),
        ("retained-pixel parity", retained_pixel_parity),
    ] {
        if let Err(error) = case(&mut renderer, &layout) {
            failures.push(format!("{name}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Cursor, focus, tab-band, scrollbar and selection changes present narrow damage on a real wgpu
/// renderer, the setters keep the frame key, and every narrowed frame equals a full repaint.
#[cfg(target_os = "windows")]
#[test]
fn windows_class_damage_on_a_real_renderer() {
    use winit::{event_loop::EventLoop, platform::windows::EventLoopBuilderExtWindows};
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("class damage event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
