#![cfg(target_os = "windows")]
//! Kitty images reach a Windows pane's presented frame in each form a program sends: a direct PNG,
//! the same PNG chunked, and a virtual placement drawn through U+10EEEE placeholder cells. Each
//! stream is shaped as the bundled console runtime forwards it (its start-up queries, `\r\r\n` line
//! ends, the APCs and placeholder cells unchanged), fed through the pane worker's decode path, and
//! presented by the GDI presenter, whose frame pixels are read back. A text-only stream is the
//! control: its frame holds neither image colour.

use base64::Engine;
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, ScrollbarMode, SoftwareRenderMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

/// The image's left half, RGB.
const LEFT_RGB: [u8; 3] = [230, 40, 40];
/// The image's right half, RGB.
const RIGHT_RGB: [u8; 3] = [40, 80, 230];
/// What the bundled runtime writes before the program's first byte: window and cursor queries,
/// focus and win32-input modes, then the home position.
const RUNTIME_PREFIX: &[u8] = b"\x1b[1t\x1b[6n\x1b[c\x1b[?1004h\x1b[?9001h\x1b[1;1H";
/// The combining marks that number placeholder rows and columns 0 through 7.
const DIACRITICS: [char; 8] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
];

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

/// A 64x32 PNG whose left half is `LEFT_RGB` and right half `RIGHT_RGB`.
fn two_colour_png() -> Vec<u8> {
    let image = image::RgbImage::from_fn(64, 32, |pixel_x, _| {
        image::Rgb(if pixel_x < 32 { LEFT_RGB } else { RIGHT_RGB })
    });
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
        .expect("encoding a PNG in memory cannot fail");
    encoded
}

fn payload() -> String {
    base64::engine::general_purpose::STANDARD.encode(two_colour_png())
}

/// Direct placement in one APC, 8 columns by 2 rows at the cursor.
fn direct_stream() -> Vec<u8> {
    format!("direct:\r\r\n\x1b_Ga=T,f=100,t=d,q=2,c=8,r=2;{}\x1b\\\r\r\nend\r\r\n", payload())
        .into_bytes()
}

/// The same placement split into 64-character chunks; only the first carries the control keys.
fn chunked_stream() -> Vec<u8> {
    let encoded = payload();
    let pieces: Vec<&str> = encoded
        .as_bytes()
        .chunks(64)
        .map(|piece| std::str::from_utf8(piece).expect("base64 is ASCII"))
        .collect();
    let mut stream = String::from("chunked:\r\r\n");
    for (index, piece) in pieces.iter().enumerate() {
        let more = u8::from(index + 1 < pieces.len());
        if index == 0 {
            // When: index is 0, the first chunk carries the whole control section.
            stream.push_str(&format!("\x1b_Ga=T,f=100,t=d,q=2,c=8,r=2,m={more};{piece}\x1b\\"));
        } else {
            stream.push_str(&format!("\x1b_Gm={more};{piece}\x1b\\"));
        }
    }
    stream.push_str("\r\r\nend\r\r\n");
    stream.into_bytes()
}

/// Transmit image 42, place it virtually over 8x2 cells, then write those cells as U+10EEEE
/// placeholders whose truecolor foreground carries the id and whose marks carry row and column.
fn placeholder_stream() -> Vec<u8> {
    let mut stream = format!(
        "placeholder:\r\r\n\x1b_Ga=t,f=100,t=d,q=2,i=42;{}\x1b\\\x1b_Ga=p,U=1,q=2,i=42,c=8,r=2\x1b\\",
        payload()
    );
    for row_mark in &DIACRITICS[..2] {
        stream.push_str("\x1b[38;2;0;0;42m");
        for column_mark in DIACRITICS {
            stream.push('\u{10EEEE}');
            stream.push(*row_mark);
            stream.push(column_mark);
        }
        stream.push_str("\x1b[39m\r\r\n");
    }
    stream.push_str("end\r\r\n");
    stream.into_bytes()
}

/// One app with a real window presented through GDI and one seeded pane.
struct Fixture {
    app: App,
    window: Arc<Window>,
    id: WindowId,
    pane: u64,
}

fn fixture(active: &ActiveEventLoop, role: &'static str) -> Result<Fixture, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_inner_size(LogicalSize::new(640.0, 360.0))
                    .with_visible(true)
                    .with_title("SonicTerm kitty images through the app"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.appearance.scrollbar = ScrollbarMode::Never;
    config.terminal.cursor_blink = false;
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
                scrollbar: config.appearance.scrollbar,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role,
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, Keymap::default());
    app.__test_set_software_render_degrade(true);
    let pane = app.__test_seed_tab("kitty");
    let id = app.__test_main_window_id().ok_or("no main window")?;
    check(app.__test_attach_window_renderer(id, window.clone(), renderer), "renderer attached")?;
    app.__test_set_frontmost_window(Some(id));
    Ok(Fixture { app, window, id, pane })
}

impl Fixture {
    /// Dispatch redraws until the renderer counts one more successful presentation, retrying
    /// briefly since a frame can be skipped or retried while the presenter settles. The render
    /// clock alone also moves for failed and retried attempts, so it cannot prove a presented frame.
    fn present(&mut self, active: &ActiveEventLoop) -> Result<(), String> {
        let started = Instant::now();
        let presented_before = self.successful_frames()?;
        loop {
            self.app.__test_set_window_last_render(self.id, started - Duration::from_secs(1));
            ApplicationHandler::window_event(
                &mut self.app,
                active,
                self.id,
                WindowEvent::RedrawRequested,
            );
            if self.successful_frames()? > presented_before {
                // When: the successful-frame count rose, the GDI presenter showed this frame.
                return Ok(());
            }
            check(started.elapsed() < Duration::from_secs(3), "the frame was presented")?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn successful_frames(&self) -> Result<u64, String> {
        self.app.__test_window_successful_frames(self.id).ok_or_else(|| String::from("no renderer"))
    }

    /// Count frame pixels near each image half's colour, and the leftmost x of each.
    fn image_pixels(&self) -> Result<ImagePixels, String> {
        let size = self.window.inner_size();
        let mut found = ImagePixels::default();
        for pixel_y in 0..size.height {
            for pixel_x in 0..size.width {
                let [blue, green, red, _] = self
                    .app
                    .__test_window_software_frame_pixel_bgra(self.id, pixel_x, pixel_y)
                    .ok_or("software frame pixel unavailable")?;
                // GDI frames are BGRA; bilinear scaling softens only the edges, so a small
                // tolerance keeps the interior of each half and drops blended seams.
                let near = |rgb: [u8; 3]| {
                    [red, green, blue].iter().zip(rgb).all(|(have, want)| have.abs_diff(want) <= 12)
                };
                if near(LEFT_RGB) {
                    found.left += 1;
                    found.left_min_x = found.left_min_x.min(pixel_x);
                } else if near(RIGHT_RGB) {
                    found.right += 1;
                    found.right_min_x = found.right_min_x.min(pixel_x);
                }
            }
        }
        Ok(found)
    }
}

#[derive(Debug)]
struct ImagePixels {
    left: usize,
    right: usize,
    left_min_x: u32,
    right_min_x: u32,
}

impl Default for ImagePixels {
    fn default() -> Self {
        Self { left: 0, right: 0, left_min_x: u32::MAX, right_min_x: u32::MAX }
    }
}

/// The same lines with the image commands and placeholder cells left out: the control that shows
/// the pixel check finds no image colour in an ordinary text frame.
fn text_only_stream() -> Vec<u8> {
    b"direct:\r\r\n\r\r\nplaceholder:\r\r\n\r\r\nend\r\r\n".to_vec()
}

/// Feed `stream` after the runtime's prefix through the pane worker and present. With `image`,
/// require both image halves on screen, the left half drawn left of the right half; without it,
/// require neither colour anywhere in the frame.
fn presents(
    active: &ActiveEventLoop,
    role: &'static str,
    stream: Vec<u8>,
    image: bool,
) -> Result<(), String> {
    let mut fixture = fixture(active, role)?;
    fixture.present(active)?;
    let mut bytes = RUNTIME_PREFIX.to_vec();
    bytes.extend(stream);
    fixture.app.__test_publish_pane_output(fixture.id, fixture.pane, &bytes);
    fixture.present(active)?;
    let found = fixture.image_pixels()?;
    if image {
        // When: the stream carries an image, both halves must be drawn in order.
        check(
            found.left > 0 && found.right > 0 && found.left_min_x < found.right_min_x,
            &format!("both image halves are drawn in order: {found:?}"),
        )
    } else {
        check(found.left == 0 && found.right == 0, &format!("no image colour is drawn: {found:?}"))
    }
}

#[test]
fn windows_kitty_images_through_the_app() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("kitty images event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for (name, role, stream, image) in [
        ("text only", "kitty-control", text_only_stream(), false),
        ("direct", "kitty-direct", direct_stream(), true),
        ("chunked", "kitty-chunked", chunked_stream(), true),
        ("placeholder", "kitty-placeholder", placeholder_stream(), true),
    ] {
        if let Err(error) = presents(active, role, stream, image) {
            failures.push(format!("{name}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
