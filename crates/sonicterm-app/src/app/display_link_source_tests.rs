//! Structural contracts for native display-link pacing and callback ownership.

use super::*;

/// The native link is macOS-only and never a CVDisplayLink; its tick target only reads the shared
/// generation and posts through the proxy, never naming the App; registration installs the link
/// after reading the monitor period; the wait fold reconciles links between collecting deadlines
/// and arming the wake; every suppression writer invalidates link pacing at the writer; due
/// service and the reconcile never write the pacing mode; and neither handler passes the real
/// clock to admission. CRLF-normalized, comments blanked.
#[test]
fn display_link_sources_keep_their_contracts() {
    let link = code_of(include_str!("display_link.rs"));
    assert!(link.contains("#[cfg(target_os = \"macos\")]\npub(super) mod native {"), "native gate");
    assert!(!link.contains("CVDisplayLink"), "only NSView.displayLink is used");
    let native = body_of(&link, "pub(super) mod native {");
    assert!(native.contains("available!(macos = 14.0)"), "the timer path stays below macOS 14");
    let fired = body_of(native, "fn display_link_fired(");
    assert!(fired.contains("send_event("), "{fired}");
    assert!(fired.contains("generation.load(Ordering::Acquire)"), "{fired}");
    assert!(!fired.contains("App"), "the tick target never touches the App: {fired}");

    let registry = code_of(concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    ));
    let insert = body_of(&registry, "pub(super) fn insert_window_registered(");
    let refresh = insert.find("refresh_monitor_period()").expect("the period is read");
    let install = insert.find("install_native_display_link(id)").expect("the link is installed");
    assert!(refresh < install, "the link takes the period just read: {insert}");

    let event_loop =
        code_of(concat!(include_str!("event_loop.rs"), "\n", include_str!("event_loop_wait.rs")));
    let wait = body_of(&event_loop, "pub(in crate::app) fn do_about_to_wait(");
    let collect = wait.find("refresh_frame_due_work_at(").expect("deadlines are collected");
    let sync = wait.find("sync_display_links(").expect("links are reconciled");
    let arm = wait.find("set_control_flow(").expect("the wake is armed");
    assert!(collect < sync && sync < arm, "{wait}");

    let redraw = code_of(REDRAW_SOURCES);
    let occlusion = body_of(&redraw, "fn observe_native_occlusion(");
    let settle = body_of(&redraw, "fn settle(");
    let stopped = body_of(settle, "FrameSettlement::Stopped(generation) => {");
    let backend = body_of(settle, "SurfaceRetryReason::Occluded => {");
    let park = body_of(&redraw, "fn park(");
    let child_tabs = code_of(include_str!("child_tabs.rs"));
    let hide = body_of(&child_tabs, "pub(super) fn hide_main_window(");
    let degrade = body_of(&link, "pub(super) fn set_software_render_degrade(");
    for (writer, body) in [
        ("native occlusion", occlusion),
        ("backend occlusion", backend),
        ("device stop", stopped),
        ("park", park),
        ("hide", hide),
        ("software degrade", degrade),
    ] {
        assert!(body.contains("invalidate_link_pacing()"), "{writer}: {body}");
    }

    let service = body_of(&redraw, "pub(super) fn service_redraw_due(");
    let reconcile = body_of(&link, "pub(super) fn sync_display_link(");
    let reconcile_all = body_of(&link, "pub(super) fn sync_display_links(");
    for (name, body) in
        [("service", service), ("reconcile", reconcile), ("reconcile all", reconcile_all)]
    {
        assert!(!assigns(body, "pacing"), "{name} must not write the pacing mode: {body}");
        assert!(!body.contains("invalidate_link_pacing("), "{name} must not cancel link pacing");
    }

    for (name, source) in [
        (
            "window_event.rs",
            code_of(concat!(
                include_str!("window_event.rs"),
                "\n",
                include_str!("window_event_pointer.rs"),
                "\n",
                include_str!("window_event_redraw.rs")
            )),
        ),
        ("child_window.rs", code_of(include_str!("child_window.rs"))),
    ] {
        for (offset, _) in source.match_indices("begin_window_redraw(") {
            let call = &source[offset..offset + source[offset..].find(')').unwrap()];
            assert!(!call.contains("Instant::now"), "{name}: {call}");
        }
    }
}

/// The scan helper's assignment check tells a write from a comparison or a longer name.
#[test]
fn the_assignment_scan_tells_a_write_from_a_comparison() {
    assert!(assigns("self.redraw.pacing = None;", "pacing"));
    assert!(!assigns("if self.redraw.pacing == Some(mode) {", "pacing"));
    assert!(!assigns("self.link_pacing = None;", "pacing"));
}
