use super::*;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

#[test]
fn failure_codes_are_distinct_and_stable() {
    // Protect CI diagnostics so display, GPU, PTY, marker, and presentation failures remain distinguishable.
    assert_eq!(RuntimeSmokeFailure::EventLoop.exit_code(), 10);
    assert_eq!(RuntimeSmokeFailure::Display.exit_code(), 11);
    assert_eq!(RuntimeSmokeFailure::Gpu.exit_code(), 12);
    assert_eq!(RuntimeSmokeFailure::Pty.exit_code(), 13);
    assert_eq!(RuntimeSmokeFailure::Marker.exit_code(), 14);
    assert_eq!(RuntimeSmokeFailure::Present.exit_code(), 15);
}

#[test]
fn marker_command_cannot_pass_from_terminal_echo() {
    // Protect the round trip: the complete expected marker may exist only in shell output, never in typed input.
    let state = RuntimeSmokeState::new(42);
    let command = String::from_utf8(state.command().to_vec()).expect("ASCII smoke command");
    assert_eq!(state.marker(), "__SONICTERM_SMOKE_42__");
    assert!(!command.contains(state.marker()));
    assert!(command.contains("printf"));
    assert!(command.contains("%s"));
    assert!(command.contains("42"));
}

#[test]
fn marker_detection_reads_the_live_grid() {
    // Protect the production observation seam rather than accepting a PTY write or process launch as success.
    let mut parser = Parser::new(Grid::new(80, 4));
    parser.advance(b"prompt$ printf '__SONICTERM_SMOKE_%s__' '42'\r\n");
    assert!(!grid_contains_marker(parser.grid(), "__SONICTERM_SMOKE_42__"));
    parser.advance(b"__SONICTERM_SMOKE_42__\r\n");
    assert!(grid_contains_marker(parser.grid(), "__SONICTERM_SMOKE_42__"));
}

#[test]
fn success_requires_a_presentation_after_marker_observation() {
    // Protect against treating parsed-but-never-presented output as a runnable terminal.
    let mut state = RuntimeSmokeState::new(7);
    state.begin_marker_wait();
    state.begin_present_wait(3);
    assert_eq!(state.observe_presented_frame(3), None);
    assert_eq!(state.observe_presented_frame(4), Some(Ok(())));
}

#[test]
fn timeout_maps_to_the_boundary_currently_under_test() {
    // Protect actionable exit codes when the watchdog fires during each startup phase.
    let mut state = RuntimeSmokeState::new(1);
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Display);
    state.begin_gpu();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Gpu);
    state.begin_pty();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Pty);
    state.begin_marker_wait();
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Marker);
    state.begin_present_wait(0);
    assert_eq!(state.timeout_failure(), RuntimeSmokeFailure::Present);
}
