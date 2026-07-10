//! no-GPU degrade decision + frame-period clamp.
use super::{
    should_degrade_for_software_render, software_render_frame_period,
    SOFTWARE_RENDER_FRAME_PERIOD,
};
use sonicterm_cfg::config::SoftwareRenderMode;
use std::time::Duration;

#[test]
fn degrade_mode_combines_config_with_detection() {
    // Auto follows detection.
    assert!(should_degrade_for_software_render(SoftwareRenderMode::Auto, true));
    assert!(!should_degrade_for_software_render(SoftwareRenderMode::Auto, false));
    // Force always degrades; Off never does — regardless of detection.
    assert!(should_degrade_for_software_render(SoftwareRenderMode::Force, false));
    assert!(!should_degrade_for_software_render(SoftwareRenderMode::Off, true));
}

#[test]
fn frame_period_clamps_only_when_degrading() {
    let sixty = Duration::from_micros(16_667); // 60 Hz
                                               // Hardware path: untouched.
    assert_eq!(software_render_frame_period(false, sixty), sixty);
    // Software path: a fast monitor period is slowed to the software cap.
    assert_eq!(software_render_frame_period(true, sixty), SOFTWARE_RENDER_FRAME_PERIOD);
}

#[test]
fn frame_period_uses_software_cap_when_degrading() {
    let slow = Duration::from_millis(50);
    assert_eq!(software_render_frame_period(true, slow), SOFTWARE_RENDER_FRAME_PERIOD);
}

#[test]
fn effective_frame_period_lowers_cap_while_composing() {
    use super::{effective_frame_period, SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD};
    let sixty = Duration::from_micros(16_667);
    // Hardware path: untouched whatever the flags.
    assert_eq!(effective_frame_period(false, false, sixty), sixty);
    assert_eq!(effective_frame_period(false, true, sixty), sixty);
    // Software path, idle: software cap.
    assert_eq!(effective_frame_period(true, false, sixty), SOFTWARE_RENDER_FRAME_PERIOD);
    // Software path, composing: lower cap — composing wins.
    assert_eq!(effective_frame_period(true, true, sixty), SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD);
}

