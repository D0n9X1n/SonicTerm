//! A new window's configured inner size: rows and columns, the tab bar, the status line and the
//! integrated titlebar band.

use super::*;

/// Parentless requests use configured startup dimensions rather than the warm pool's internal size.
#[test]
fn new_window_size_fallback_tracks_configured_rows_and_columns() {
    let mut config = Config::default();
    config.window.titlebar = sonicterm_cfg::config::TitlebarMode::Native;
    config.window.cols = 100;
    config.window.rows = 40;
    config.font.size = 16.0;
    config.font.line_height = 1.25;
    config.window.padding_left = 8.0;
    config.window.padding_right = 12.0;
    config.window.padding_top = 4.0;
    config.window.padding_bottom = 6.0;
    let size = configured_window_size(&config, false);
    assert_eq!(size, winit::dpi::LogicalSize::new(920.0, 810.0));
    assert_eq!(
        configured_window_size(&config, true).height,
        size.height + f64::from(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT)
    );
}

/// An enabled status line adds its own height to the initial window, with or without the tab
/// bar, so a window opened with `[window]` rows keeps that many terminal rows.
#[test]
fn configured_window_size_adds_the_status_line_when_enabled() {
    let mut config = Config::default();
    config.window.titlebar = sonicterm_cfg::config::TitlebarMode::Native;
    let without = configured_window_size(&config, true);
    config.status_line.enabled = true;
    let strip = f64::from(sonicterm_gpu::status_strip::status_line_height(config.font.size, 1.0));
    assert!(strip >= 20.0);
    assert_eq!(configured_window_size(&config, true).height, without.height + strip);
    assert_eq!(configured_window_size(&config, true).width, without.width);
    config.status_line.enabled = false;
    let bare = configured_window_size(&config, false);
    config.status_line.enabled = true;
    assert_eq!(configured_window_size(&config, false).height, bare.height + strip);
}

/// With an integrated titlebar the band's height comes out of the inner size once: a top strip
/// shares it (`max`), and a bottom strip or none sits apart from a band at least the strip's text
/// height. So a window opened with `[window]` rows keeps that many terminal rows in every layout.
#[test]
fn configured_window_size_reserves_the_integrated_band_once() {
    use sonicterm_cfg::config::TitlebarMode;
    use sonicterm_cfg::status_line::StatusPosition;
    let mut config = Config::default();
    config.window.titlebar = TitlebarMode::Native;
    let native = configured_window_size(&config, true).height;
    config.window.titlebar = TitlebarMode::Integrated;
    let band = f64::from(titlebar::creation_band_estimate(TitlebarMode::Integrated));
    let strip = f64::from(sonicterm_gpu::status_strip::status_line_height(config.font.size, 1.0));
    let name_band = band.max(strip);
    if band == 0.0 {
        // When: this platform has no integrated titlebar, the mode changes nothing.
        assert_eq!(configured_window_size(&config, true).height, native);
        return;
    }
    assert_eq!(configured_window_size(&config, true).height, native + name_band, "strip off");
    config.status_line.enabled = true;
    config.status_line.position = StatusPosition::Top;
    assert_eq!(configured_window_size(&config, true).height, native + name_band, "merged");
    config.status_line.position = StatusPosition::Bottom;
    assert_eq!(
        configured_window_size(&config, true).height,
        native + name_band + strip,
        "a bottom strip sits apart from the band"
    );
}
