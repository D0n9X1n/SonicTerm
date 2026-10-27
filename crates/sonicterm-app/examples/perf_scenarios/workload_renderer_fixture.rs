#![cfg(test)]
//! Windows renderer working-set coverage fixture with one native event loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sonicterm_app::app::App;
use sonicterm_cfg::config::{Config, ScrollbarMode, SoftwareRenderMode};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::{GlyphAtlasStart, GpuRenderer, RendererSettings, SurfaceAppearance};
use sonicterm_gpu::glyph_working_set::{
    measure_glyph_working_set, representative_gaps, representative_identities, TileIdentity,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Window, WindowId};

use super::super::{fixture_bytes, FONT_FAMILY, FONT_SIZE};
use crate::scenarios::Fixture;

/// The tab titles the harness measures with.
const TITLES: [&str; 3] = ["zsh", "perf_scenarios", "S12 covered window"];

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

/// Whether this host enumerates no wgpu adapter at all, established apart from renderer
/// construction. That is the only limitation that turns a failed wgpu renderer into a skip.
fn host_has_no_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).is_empty()
}

/// The app's font directory, so the renderer and the helper load the same faces.
fn font_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")]
}

/// A wgpu renderer (WARP on the hosted runner) at `scale` on its own window, with the tab bar
/// on. `Ok(None)` only when the host enumerates no adapter; any other error fails.
fn renderer(
    active: &ActiveEventLoop,
    scale: f32,
    size_pt: f32,
) -> Result<Option<(Arc<Window>, GpuRenderer)>, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(1000, 640))
                    .with_title("SonicTerm working-set coverage"),
            )
            .map_err(|error| error.to_string())?,
    );
    let dirs = font_dirs();
    let created = GpuRenderer::new(
        window.clone(),
        active,
        &Theme::default(),
        RendererSettings {
            font_family: FONT_FAMILY,
            font_dirs: &dirs,
            font_size: size_pt,
            line_height_mult: 1.2,
            font_weight_scale: 1.0,
            subpixel_aa: Default::default(),
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: Default::default(),
                opacity: 1.0,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Off,
            },
            role: "working-set-coverage",
            glyph_atlas_start: GlyphAtlasStart::Normal,
        },
    );
    let mut renderer = match created {
        Ok(renderer) => renderer,
        Err(error) if host_has_no_adapter() => {
            // When: the host enumerates no adapter, report the capability, not a pass.
            println!("capability=HOST_INCAPABLE case=working-set-coverage reason={error}");
            return Ok(None);
        }
        Err(error) => {
            // When: an adapter exists, so the construction error is a defect.
            return Err(format!("wgpu renderer construction failed with an adapter: {error}"));
        }
    };
    renderer.set_scale_factor(scale);
    renderer.set_tab_bar_visible(true);
    renderer.set_cursor_blink(false);
    Ok(Some((window, renderer)))
}

/// How long a case may wait for a presented frame whose rows and chrome draw no tofu.
const FALLBACK_DEADLINE: Duration = Duration::from_secs(15);

/// Dispatch the real main-window redraw with pacing open until a dispatch presents a frame
/// (the renderer's successful-frame count advances) and that frame drew no tofu in its terminal
/// rows (`last_missing_tofu()`) or its chrome (`last_missing_chrome()`: titles, palette rows,
/// query and footer), so every fallback face has landed. Fails at the deadline with what was
/// still missing.
fn settle(app: &mut App, active: &ActiveEventLoop, id: WindowId, case: &str) -> Result<(), String> {
    let started = Instant::now();
    let mut presented_any = false;
    let mut last_missing = Vec::new();
    let mut last_chrome_missing = Vec::new();
    while started.elapsed() < FALLBACK_DEADLINE {
        let before = app
            .__test_window_renderer_mut(id)
            .map(|renderer| renderer.successful_frame_count())
            .ok_or("the window has a renderer")?;
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
        let renderer = app.__test_window_renderer_mut(id).ok_or("the window has a renderer")?;
        let presented = renderer.successful_frame_count() > before;
        presented_any |= presented;
        last_missing = renderer.last_missing_tofu().to_vec();
        last_chrome_missing = renderer.last_missing_chrome().to_vec();
        if super::fallback_settled(presented, &last_missing, &last_chrome_missing) {
            // When: the frame presented with no row or chrome tofu, fallback is complete.
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!(
        "{case}: no presented frame without tofu within {FALLBACK_DEADLINE:?} \
         (any frame presented: {presented_any}; terminal rows still missing: \
         {last_missing:?}; chrome still missing: {last_chrome_missing:?})"
    ))
}

/// Test 17 for one fixture at one scale: the renderer draws the fixture, three titles, the
/// cursor and the open palette until a presented frame draws no tofu in its rows or chrome.
/// It then holds every required footer symbol and title character as a real tile, prints its
/// atlas as a `source=real_renderer` row, every resident tile resolves to an identity (face,
/// glyph, strike, variant and flags), each identity is in the helper's set with the same
/// raster size, and for S9 a resident body tile draws each representative codepoint with pixels.
fn covered(
    active: &ActiveEventLoop,
    name: &str,
    fixture: Fixture,
    scale: f32,
) -> Result<(), String> {
    let case = format!("{name}@{scale}");
    let size_pt: f32 = FONT_SIZE.parse().map_err(|_| "the scenario font size")?;
    let Some((window, renderer)) = renderer(active, scale, size_pt)? else {
        return Ok(());
    };
    let mut config = Config::default();
    config.font.family = FONT_FAMILY.into();
    config.font.size = size_pt;
    config.appearance.software_render_mode = SoftwareRenderMode::Off;
    config.locale = "en".into();
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let bytes = fixture_bytes(fixture);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    // A pty's ONLCR turns each LF into CR LF before the parser sees it.
    let terminal_bytes = text.replace('\n', "\r\n").into_bytes();
    for title in TITLES {
        let pane = app.__test_seed_tab(title);
        check(app.__test_advance_pane_parser(pane, &terminal_bytes), "fixture written")?;
    }
    let id = app.__test_main_window_id().ok_or("no main window")?;
    check(app.__test_attach_window_renderer(id, window, renderer), "renderer attached")?;
    super::open_palette_on_main_frame(&mut app).map_err(|error| format!("{case}: {error}"))?;
    settle(&mut app, active, id, &case)?;
    let renderer = app.__test_window_renderer_mut(id).ok_or("the window has a renderer")?;
    let resident = renderer.__test_resident_tile_keys();
    let absent = super::missing_required_chrome(&resident);
    check(absent.is_empty(), &format!("{case}: required chrome not resident: {absent:?}"))?;
    let facts = renderer.glyph_atlas_facts();
    // The renderer's own missing-glyph reports, which include shaped glyphs that drew nothing.
    let incomplete_glyphs =
        renderer.last_missing_tofu().len() + renderer.last_missing_chrome().len();
    print_row(&super::working_set_row(
        super::platform_name(),
        scale as u32,
        name,
        "real_renderer",
        &facts.fit,
        facts.max_tile,
        facts.packed_pixels,
        incomplete_glyphs,
    ));
    let (identities, unresolved) = renderer.__test_resident_tile_identities();
    check(
        unresolved.is_empty(),
        &format!("{case}: resident keys with no face identity: {unresolved:?}"),
    )?;
    let lines: Vec<&str> = text.lines().collect();
    let dpi = (72.0 * scale).round() as usize;
    if name == "S9" {
        // When: the fixture is S9, its representative emoji and CJK must be real resident tiles.
        let body_tiles = renderer.__test_resident_body_tiles();
        let sizes: Vec<_> = body_tiles.iter().map(|(key, size, _)| (*key, *size)).collect();
        let gaps = representative_gaps(&super::S9_REPRESENTATIVE_CODEPOINTS, &sizes);
        // Diagnostics only: shaping a codepoint alone may pick another fallback face than the frame.
        let alone = representative_identities(
            &super::S9_REPRESENTATIVE_CODEPOINTS,
            FONT_FAMILY,
            size_pt,
            dpi,
            &font_dirs(),
        );
        let label = |identity: Option<&TileIdentity>| {
            identity.map_or_else(
                || "unresolved".to_owned(),
                |identity| {
                    format!("{} glyph {}", identity.source.face.source, identity.source.glyph_id)
                },
            )
        };
        for (codepoint, isolated) in &alone {
            let drawn: Vec<String> = body_tiles
                .iter()
                .filter(|(key, _, _)| key.ch == *codepoint)
                .map(|(_, _, identity)| label(identity.as_ref()))
                .collect();
            println!(
                "glyph_atlas_representative fixture=S9 scale={scale} codepoint={codepoint} \
                 drawn={drawn:?} alone={}",
                label(isolated.as_ref())
            );
        }
        check(gaps.is_empty(), &format!("{case}: representative codepoints not drawn: {gaps:?}"))?;
    }
    // Only helper work is shared; every suite still proves its own resident identities and sizes.
    let helper = match crate::workload::glyph_oracle::load(name, dpi)? {
        Some(tiles) => tiles,
        None => {
            let measured =
                measure_glyph_working_set(&lines, &TITLES, FONT_FAMILY, size_pt, dpi, &font_dirs())
                    .map_err(|error| {
                        format!("{case}: the helper measurement is incomplete: {error}")
                    })?;
            crate::workload::glyph_oracle::identities(&measured.tile_identities)?
        }
    };
    let identities = crate::workload::glyph_oracle::identities(&identities)?;
    let missing: Vec<&String> =
        identities.keys().filter(|identity| !helper.contains_key(*identity)).take(20).collect();
    check(missing.is_empty(), &format!("{case}: resident tiles the helper missed: {missing:?}"))?;
    let resized: Vec<(&String, [u32; 2], [u32; 2])> = identities
        .iter()
        .filter_map(|(identity, size)| {
            let measured = helper[identity];
            (measured != *size).then_some((identity, *size, measured))
        })
        .take(20)
        .collect();
    check(
        resized.is_empty(),
        &format!("{case}: raster sizes differ (identity, renderer, helper): {resized:?}"),
    )
}

/// Write one measurement row straight to stdout. CI runs this test without `--nocapture`, and
/// libtest captures only the print macros, so a direct write keeps the row in the job's log.
fn print_row(row: &str) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    // A row that cannot be written loses only evidence; the coverage assertions still run.
    let _ = writeln!(stdout, "{row}").and_then(|()| stdout.flush());
}

fn run_cases(active: &ActiveEventLoop) -> Result<(), String> {
    let mut failures = Vec::new();
    for (name, fixture) in [("S9", Fixture::EmojiCjk), ("S12", Fixture::HistoryScreen)] {
        for scale in [1.0, 2.0] {
            if let Err(error) = covered(active, name, fixture, scale) {
                failures.push(error);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
#[path = "workload_renderer_fixture_tests.rs"]
mod workload_renderer_fixture_tests;
