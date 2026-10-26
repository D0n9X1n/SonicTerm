#![cfg(target_os = "windows")]
//! Where Cascadia Mono's digits land in a real Windows software frame. Placement is checked
//! absolutely: a digit whose outline rests on the baseline (yMin 0) has its last ink row just above
//! the renderer's own baseline row. Tops are checked relative to each other: two digits' first ink
//! rows differ by what their outlines predict, so the round 2, which overshoots the flat 7 by 20 of
//! 2048 units, shows one row higher at 21.75 px. Absolute tops are not asserted: DirectWrite inks a
//! top row the outline barely touches at some strikes (every digit at 14.5 px), which no
//! outline-coverage threshold models.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sonicterm_app::app::App;
use sonicterm_cfg::config::{Config, ScrollbarMode, SoftwareRenderMode};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_gpu::outline_ink::{
    glyph_extent, ink_threshold, predicted_ink_top, DECISION_MARGIN, INK_DIFFERENCE,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Window, WindowId};

/// The shipping default face on Windows, measured only where the host has it.
const FAMILY: &str = "Cascadia Mono";
/// The raster scales the font-weight layout test checks.
const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

struct Probe {
    outcome: Option<Result<(), String>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        // winit allows one event loop per process, so every scale runs inside this one.
        self.outcome = Some(run(active));
        active.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

/// Every scale in turn, collecting mismatches; a host that does not resolve the family skips all.
fn run(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for scale in SCALES {
        let Some(mut mismatches) = check_scale(active, scale)? else {
            return Ok(());
        };
        failures.append(&mut mismatches);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// Redraw until a frame is presented after this call starts.
fn render(app: &mut App, active: &ActiveEventLoop, id: WindowId) -> Result<(), String> {
    let started = Instant::now();
    loop {
        app.__test_set_window_last_render(id, started - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        if app.__test_window_last_render(id).is_some_and(|time| time >= started) {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("no frame was presented".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `#rrggbb` as sRGB bytes.
fn hex_rgb(hex: &str) -> Result<[u8; 3], String> {
    let digits = hex.trim_start_matches('#');
    let channel = |index: usize| {
        digits.get(index * 2..index * 2 + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    match (channel(0), channel(1), channel(2)) {
        (Some(red), Some(green), Some(blue)) => Ok([red, green, blue]),
        _ => Err(format!("not a #rrggbb colour: {hex}")),
    }
}

/// One digit's outline prediction and the ink the frame drew for it.
struct DigitInk {
    /// The digit drawn.
    digit: char,
    /// Whether the outline's lowest point is on the baseline, so its last ink row is just above it.
    flat_bottom: bool,
    /// Rows above the baseline the outline predicts ink in.
    rows_above_baseline: u32,
    /// Whether the predicted top row's coverage is clear of the ink threshold.
    decided: bool,
    /// The first row with ink.
    top_row: Option<u32>,
    /// The last row with ink.
    bottom_row: Option<u32>,
}

/// Draw `0123456789` at `scale` and compare each digit's measured ink top with the top its outline
/// predicts. `Ok(None)` when the host does not resolve the family, `Ok(Some(mismatches))` otherwise.
fn check_scale(active: &ActiveEventLoop, scale: f32) -> Result<Option<Vec<String>>, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 200))
                    .with_visible(true)
                    .with_title("SonicTerm digit tops"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.font.family = FAMILY.into();
    config.font.size = 14.5;
    config.font.line_height = 1.1;
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.window.padding_left = 0.0;
    config.window.padding_right = 0.0;
    config.window.padding_top = 0.0;
    config.window.padding_bottom = 0.0;
    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &theme,
        RendererSettings {
            font_family: &config.font.family,
            font_dirs: &[],
            font_size: config.font.size,
            line_height_mult: config.font.line_height,
            font_weight_scale: config.font.effective_weight_scale(),
            subpixel_aa: config.font.subpixel_aa,
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: config.appearance.backdrop,
                opacity: 1.0,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "digit-tops-test",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    let resolved = renderer.resolved_font_family();
    if resolved.as_deref() != Some(FAMILY) {
        // When: the host resolves another face, its outlines are unknown here, so report and skip.
        println!("capability=HOST_INCAPABLE case=digit-tops reason=\"{FAMILY} resolved to {resolved:?}\"");
        return Ok(None);
    }
    renderer.set_scale_factor(scale);
    renderer.set_tab_bar_visible(false);
    renderer.set_cursor_blink(false);
    let foreground = hex_rgb(&theme.colors.foreground.0)?;
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let pane = app.__test_seed_tab("digit-tops");
    let id = app.__test_main_window_id().ok_or("no main window")?;
    if !app.__test_advance_pane_parser(pane, b"\x1b[?25l0123456789") {
        return Err("the digits were not written".into());
    }
    if !app.__test_attach_window_renderer(id, window, renderer) {
        return Err("the renderer was not attached".into());
    }
    app.__test_set_frontmost_window(Some(id));
    render(&mut app, active, id)?;
    // The face file and strike the renderer drew the 7 from, so the outline is the one it drew.
    let renderer = app.__test_window_renderer_mut(id).ok_or("the window has a renderer")?;
    let tiles = renderer.__test_resident_body_tiles();
    let baseline_in_cell = renderer.baseline_y_in_cell();
    let (face_path, strike_px) = tiles
        .iter()
        .find_map(|(key, _, identity)| {
            let identity = identity.as_ref().filter(|_| key.ch == '7')?;
            let strike_px = identity.source.strike_px_milli as f32 / 1000.0;
            Some((identity.source.face.source.clone(), strike_px))
        })
        .ok_or("no resident tile for 7")?;
    let font = std::fs::read(&face_path).map_err(|error| format!("{face_path}: {error}"))?;
    let (cell_w, cell_h, top) = app.__test_window_cell_geometry(id).ok_or("no cell geometry")?;
    let pixel = |x_px: u32, y_px: u32| app.__test_window_software_frame_pixel_bgra(id, x_px, y_px);
    let row_top = top.ceil() as u32;
    let row_bottom = (top + cell_h).floor() as u32;
    let background = pixel((cell_w * 20.5) as u32, row_top + 1).ok_or("no frame pixel")?;
    // The rows of `column` with ink, read as the font-weight test reads a digit's top.
    let ink_rows = |column: u32| -> Vec<u32> {
        let left = (column as f32 * cell_w).round() as u32;
        let right = ((column + 1) as f32 * cell_w).round() as u32;
        (row_top..row_bottom)
            .filter(|y_px| {
                (left..right).any(|x_px| {
                    pixel(x_px, *y_px).is_some_and(|bgra| {
                        bgra[..3]
                            .iter()
                            .zip(&background[..3])
                            .any(|(ink, base)| ink.abs_diff(*base) > INK_DIFFERENCE)
                    })
                })
            })
            .collect()
    };
    // The baseline comes from the renderer's own geometry, the cell's top plus the offset every
    // row's glyphs are placed from, never from the drawn ink.
    let baseline = (top + baseline_in_cell).round() as u32;
    let threshold = ink_threshold(foreground, [background[2], background[1], background[0]]);
    let mut digits = Vec::new();
    for (column, digit) in ('0'..='9').enumerate() {
        let extent = glyph_extent(&font, digit).ok_or_else(|| format!("no outline for {digit}"))?;
        let predicted = predicted_ink_top(extent, strike_px, threshold);
        let decided = (predicted.top_row_coverage - threshold).abs() >= DECISION_MARGIN;
        let ink = ink_rows(column as u32);
        let (top_row, bottom_row) = (ink.first().copied(), ink.last().copied());
        println!(
            "digit_top scale={scale} strike_px={strike_px} digit={digit} y_min={} y_max={} \
             top_row_coverage={:.3} threshold={threshold:.3} baseline={baseline} \
             predicted={:?} measured={top_row:?} bottom={bottom_row:?} decided={decided}",
            extent.y_min,
            extent.y_max,
            predicted.top_row_coverage,
            baseline.checked_sub(predicted.rows_above_baseline)
        );
        digits.push(DigitInk {
            digit,
            flat_bottom: extent.y_min == 0,
            rows_above_baseline: predicted.rows_above_baseline,
            decided,
            top_row,
            bottom_row,
        });
    }
    let flat: Vec<char> =
        digits.iter().filter(|ink| ink.flat_bottom).map(|ink| ink.digit).collect();
    let mut failures = Vec::new();
    if !['1', '2', '7'].iter().all(|digit| flat.contains(digit)) {
        // When: 1, 2 or 7 is not flat-bottomed, this is not the face the placement check expects.
        failures.push(format!("{FAMILY}'s flat-bottomed digits are {flat:?}, not 1, 2 and 7"));
    }
    // Each flat-bottomed digit whose last ink row is not just above `baseline_row`.
    let misplaced = |baseline_row: u32| -> Vec<String> {
        digits
            .iter()
            .filter(|ink| ink.flat_bottom && ink.bottom_row != baseline_row.checked_sub(1))
            .map(|ink| {
                format!(
                    "{} at {scale}x: last ink row {:?}, baseline row {baseline_row}",
                    ink.digit, ink.bottom_row
                )
            })
            .collect()
    };
    failures.extend(misplaced(baseline));
    if misplaced(baseline + 1).is_empty() {
        // When: a baseline 1 px low still matches, the bottom check cannot see a misplaced baseline.
        failures.push(format!("at {scale}x a baseline 1 px below the renderer's still matched"));
    }
    let decided: Vec<&DigitInk> = digits.iter().filter(|ink| ink.decided).collect();
    for (index, first) in decided.iter().enumerate() {
        for second in &decided[index + 1..] {
            let (Some(first_top), Some(second_top)) = (first.top_row, second.top_row) else {
                // When: either decided digit drew no ink, its top cannot be compared.
                failures
                    .push(format!("{} or {} drew no ink at {scale}x", first.digit, second.digit));
                continue;
            };
            let measured = i64::from(first_top) - i64::from(second_top);
            let predicted =
                i64::from(second.rows_above_baseline) - i64::from(first.rows_above_baseline);
            if measured != predicted {
                // When: `measured` differs from `predicted`, the two tops are not where the outlines put them.
                failures.push(format!(
                    "{} and {} at {scale}x: tops differ by {measured} rows, outlines by {predicted}",
                    first.digit, second.digit
                ));
            }
        }
    }
    Ok(Some(failures))
}

/// At every scale the font-weight test checks, each flat-bottomed Cascadia Mono digit's last ink row
/// is the renderer's baseline row less one, and a baseline 1 px lower fails that check. Each pair of
/// decided digits' tops differ by what their outlines predict; a top within `DECISION_MARGIN` of the
/// ink threshold is printed, not compared. A host without the face skips with a printed reason.
#[test]
fn cascadia_mono_digit_tops_follow_their_outlines() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().expect("an event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("the event loop runs");
    probe
        .outcome
        .expect("the probe ran")
        .expect("digits sit on the baseline and their tops follow their outlines");
}
