//! no-GPU degrade decision + frame-period clamp.
use super::{
    effective_frame_period, should_degrade_for_software_render, software_render_frame_period,
    SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD, SOFTWARE_RENDER_FRAME_PERIOD,
};
use sonicterm_cfg::config::SoftwareRenderMode;
use std::time::Duration;

/// Monitor period derived the way the window-ready path derives it, from
/// winit's `refresh_rate_millihertz`: `period_us = 1_000_000_000 / mhz`.
/// Building the test inputs through the same arithmetic keeps them honest
/// against the truncation the real path performs.
fn period_from_millihertz(mhz: u32) -> Duration {
    Duration::from_micros(1_000_000_000u64 / u64::from(mhz))
}

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
    let sixty = Duration::from_micros(16_667);
    // Hardware path: untouched whatever the flags.
    assert_eq!(effective_frame_period(false, false, sixty), sixty);
    assert_eq!(effective_frame_period(false, true, sixty), sixty);
    // Software path, idle: software cap.
    assert_eq!(effective_frame_period(true, false, sixty), SOFTWARE_RENDER_FRAME_PERIOD);
    // Software path, composing: lower cap — composing wins.
    assert_eq!(effective_frame_period(true, true, sixty), SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD);
}

#[test]
fn software_cap_applies_across_high_refresh_monitors() {
    // The rates a software-render user is most likely to be sitting in front
    // of. Every one is FASTER than the 40 fps software cap, so the cap must
    // fire and the monitor period must not pass through.
    for (mhz, label) in [
        (120_000u32, "120 Hz"),
        (144_000, "144 Hz"),
        (240_000, "240 Hz"),
        (165_000, "165 Hz"),
    ] {
        let monitor = period_from_millihertz(mhz);
        assert!(
            monitor < SOFTWARE_RENDER_FRAME_PERIOD,
            "{label} period {monitor:?} should be faster than the software cap, \
             otherwise this case proves nothing",
        );
        assert_eq!(
            software_render_frame_period(true, monitor),
            SOFTWARE_RENDER_FRAME_PERIOD,
            "{label}: the software cap must replace the monitor period",
        );
        // Hardware path on the same monitor keeps the native period.
        assert_eq!(
            software_render_frame_period(false, monitor),
            monitor,
            "{label}: the hardware path must pass the monitor period through",
        );
    }
}

#[test]
fn software_cap_overrides_rather_than_floors_a_slower_monitor() {
    // A 30 Hz panel is SLOWER than the software cap. The resolver is an
    // unconditional override, not a max(): degrading replaces 33.3 ms with
    // 25 ms, so the CPU is asked for MORE frames than the panel presents.
    // Encoded here as the behavior that exists, not the behavior the word
    // "clamp" in the doc comment implies.
    let thirty = period_from_millihertz(30_000);
    assert!(thirty > SOFTWARE_RENDER_FRAME_PERIOD);
    assert_eq!(software_render_frame_period(true, thirty), SOFTWARE_RENDER_FRAME_PERIOD);
    assert!(
        software_render_frame_period(true, thirty) < thirty,
        "a slower-than-cap monitor is sped up to the cap, not left alone",
    );
}

#[test]
fn effective_frame_period_covers_the_full_state_matrix() {
    let monitor = period_from_millihertz(144_000);
    // (software_render, composing) -> expected period.
    let cases = [
        (false, false, monitor, "hardware, idle: monitor period"),
        (false, true, monitor, "hardware, composing: monitor period, drop is software-only"),
        (true, false, SOFTWARE_RENDER_FRAME_PERIOD, "software, idle: 40 fps cap"),
        (true, true, SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD, "software, composing: ~12 fps cap"),
    ];
    for (software_render, composing, expected, label) in cases {
        assert_eq!(effective_frame_period(software_render, composing, monitor), expected, "{label}");
    }
}

#[test]
fn compose_cap_releases_when_composition_ends() {
    // The failure worth hunting is a compose cap that engages and never lets
    // go. The resolver is a pure function of its arguments with no retained
    // state, so flipping `composing` back to false restores the 40 fps cap
    // immediately, and repeated cycles are stable.
    let monitor = period_from_millihertz(120_000);
    for _ in 0..3 {
        assert_eq!(
            effective_frame_period(true, true, monitor),
            SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD,
        );
        assert_eq!(
            effective_frame_period(true, false, monitor),
            SOFTWARE_RENDER_FRAME_PERIOD,
            "composition ending must restore the 40 fps cap",
        );
    }
}

#[test]
fn the_two_resolvers_agree_on_the_non_composing_software_path() {
    // `software_render_frame_period` decides the stored period; the stored
    // period is then fed to `effective_frame_period` as its monitor period.
    // On the non-composing software path the two must land on the same value,
    // otherwise the deferral gate and the wake deadline would pace to
    // different clocks.
    for mhz in [60_000u32, 120_000, 144_000, 240_000] {
        let monitor = period_from_millihertz(mhz);
        let stored = software_render_frame_period(true, monitor);
        assert_eq!(stored, effective_frame_period(true, false, stored));
    }
}

#[test]
fn degrade_write_is_not_reversible_from_the_stored_period() {
    // The window-ready path writes the monitor period into the frame-period
    // field, then overwrites that same field with the software cap when
    // degrading. The original monitor period is retained nowhere, so a later
    // transition back to the hardware path resolves against the already-capped
    // value and the monitor's true period cannot be recovered.
    let monitor = period_from_millihertz(144_000);
    let stored = software_render_frame_period(true, monitor);
    assert_eq!(stored, SOFTWARE_RENDER_FRAME_PERIOD);
    // Degrade cleared, resolving against what the field now holds.
    let after_degrade_cleared = software_render_frame_period(false, stored);
    assert_eq!(after_degrade_cleared, SOFTWARE_RENDER_FRAME_PERIOD);
    assert_ne!(
        after_degrade_cleared, monitor,
        "the monitor period is not restored by clearing degrade",
    );
    assert_eq!(
        effective_frame_period(false, false, stored),
        SOFTWARE_RENDER_FRAME_PERIOD,
        "the hardware branch returns the stored value, which is the cap",
    );
}
