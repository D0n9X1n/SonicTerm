//! The one native pointer cursor decision per window: the owner precedence, the shape mapping,
//! and the sync that follows the hovered pane, writing only when the result changes.

use super::*;
use crate::app::{App, SplitterDragState};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::pane::{Rect, SplitAxis};
use sonicterm_vt::vt::PointerShape;
use winit::{dpi::PhysicalPosition, window::CursorIcon, window::WindowId};

/// Every owner claims the pointer; removing them one at a time, highest first, shows each one's
/// icon in the issue's order: overlay, splitter, scrollbar, tab bar, titlebar band, link,
/// selection, then the pane's shape, and the arrow over no pane.
#[test]
fn ui_owners_take_the_cursor_in_precedence_order() {
    let mut owners = CursorOwners {
        overlay: true,
        splitter: Some(SplitAxis::Vertical),
        scrollbar: true,
        tab_bar: true,
        band_edge: Some(CursorIcon::NsResize),
        link: true,
        selection: true,
        pane_shape: Some(PointerShape::Crosshair),
    };
    assert_eq!(decide_cursor(owners), CursorIcon::Default, "overlay");
    owners.overlay = false;
    assert_eq!(decide_cursor(owners), CursorIcon::ColResize, "vertical splitter");
    owners.splitter = Some(SplitAxis::Horizontal);
    assert_eq!(decide_cursor(owners), CursorIcon::RowResize, "horizontal splitter");
    owners.splitter = None;
    // The arrow here, not the band's edge icon, proves the scrollbar outranks the band.
    assert_eq!(decide_cursor(owners), CursorIcon::Default, "scrollbar");
    owners.scrollbar = false;
    assert_eq!(decide_cursor(owners), CursorIcon::Default, "tab bar");
    owners.tab_bar = false;
    assert_eq!(decide_cursor(owners), CursorIcon::NsResize, "band edge");
    owners.band_edge = None;
    assert_eq!(decide_cursor(owners), CursorIcon::Pointer, "link");
    owners.link = false;
    assert_eq!(decide_cursor(owners), CursorIcon::Text, "selection");
    owners.selection = false;
    assert_eq!(decide_cursor(owners), CursorIcon::Crosshair, "pane shape");
    owners.pane_shape = None;
    assert_eq!(decide_cursor(owners), CursorIcon::Default, "no pane");
}

/// Each of the 34 shapes maps 1:1 to the winit icon whose W3C name is the shape's CSS name.
#[test]
fn every_shape_maps_to_the_winit_icon_of_the_same_css_name() {
    for shape in PointerShape::ALL {
        assert_eq!(cursor_icon_for(shape).name(), shape.css_name());
    }
}

/// A headless app whose main window shows two panes side by side on an 800x240 viewport of
/// 10-pixel cells; returns it with the left and right pane ids.
fn split_main() -> (App, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let left = app.__test_seed_tab("main");
    app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0);
    app.__test_split_active_right();
    let right = app
        .compute_active_pane_rects()
        .into_iter()
        .map(|(pane_id, _)| pane_id)
        .find(|pane_id| *pane_id != left)
        .expect("a second pane");
    (app, left, right)
}

/// Advance a pane's parser with `bytes` and publish its modes and pointer shape, as the VT worker
/// does after each batch.
fn publish(app: &App, window_id: WindowId, pane_id: u64, bytes: &[u8]) {
    let pane = &app.registry.windows[&window_id].panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(bytes);
    pane.__test_publish_input_modes(&parser);
}

/// The icon a window last applied and how many times it applied a change.
fn applied(app: &App, window_id: WindowId) -> (Option<CursorIcon>, u32) {
    let cursor = &app.registry.windows[&window_id].native_cursor;
    (cursor.applied, cursor.applied_writes)
}

/// The main window shows the hovered pane's shape: crossing to the other pane changes it, a
/// repeated sync with nothing changed writes nothing, and leaving the window returns the arrow.
#[test]
fn the_main_cursor_follows_the_hovered_pane_and_writes_only_on_change() {
    let (mut app, left, _right) = split_main();
    let main = app.registry.main_window_id.unwrap();
    publish(&app, main, left, b"\x1b]22;crosshair\x07");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.sync_window_cursors();
    assert_eq!(applied(&app, main), (Some(CursorIcon::Crosshair), 1));
    app.sync_window_cursors();
    assert_eq!(applied(&app, main), (Some(CursorIcon::Crosshair), 1), "no write when unchanged");
    app.handle_main_cursor_moved(PhysicalPosition::new(655.0, 85.0));
    app.sync_window_cursors();
    assert_eq!(applied(&app, main), (Some(CursorIcon::Text), 2), "the right pane's fallback");
    app.handle_main_cursor_left();
    app.sync_window_cursors();
    assert_eq!(applied(&app, main), (Some(CursorIcon::Default), 3));
}

/// A child window decides from its own hovered pane, and leaving it returns the arrow.
#[test]
fn a_child_cursor_follows_its_own_hovered_pane() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0
    ));
    let pane_id = app.__test_child_active_pane(child).unwrap();
    publish(&app, child, pane_id, b"\x1b]22;wait\x07");
    let config = app.config.clone();
    app.handle_child_cursor_moved(child, PhysicalPosition::new(55.0, 85.0), &config);
    app.sync_window_cursors();
    assert_eq!(applied(&app, child).0, Some(CursorIcon::Wait));
    // The child's CursorLeft handler parks the pointer off the window the same way.
    app.registry.windows.get_mut(&child).unwrap().cursor_pos = (-1.0, -1.0);
    app.sync_window_cursors();
    assert_eq!(applied(&app, child).0, Some(CursorIcon::Default));
}

/// With the pointer stationary, the hovered pane's published shape changes the cursor: an OSC 22
/// set, a switch to the alternate screen's empty stack, and a tracking mode's fallback.
#[test]
fn a_shape_change_under_a_stationary_pointer_updates_the_cursor() {
    let (mut app, left, _right) = split_main();
    let main = app.registry.main_window_id.unwrap();
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Text));
    publish(&app, main, left, b"\x1b]22;pointer\x07");
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Pointer));
    publish(&app, main, left, b"\x1b[?1049h");
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Text));
    publish(&app, main, left, b"\x1b[?1000h");
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Default));
}

/// Releasing a UI owner with the pointer stationary hands the cursor back to the pane: ending a
/// splitter drag, closing the palette and a path-hover invalidation each restore its shape.
#[test]
fn releasing_an_owner_restores_the_pane_shape_without_motion() {
    let (mut app, left, right) = split_main();
    let main = app.registry.main_window_id.unwrap();
    publish(&app, main, left, b"\x1b]22;crosshair\x07");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Crosshair));

    let rects = app.compute_active_pane_rects();
    let rect_of = |pane_id: u64| rects.iter().find(|(id, _)| *id == pane_id).unwrap().1;
    let seam_x = (rect_of(left).x + rect_of(left).w + rect_of(right).x) / 2.0;
    let hit = {
        let window = app.main().unwrap();
        window.tab_states[window.tabs.active_index()]
            .tree
            .hit_splitter(Rect::new(0.0, 0.0, 800.0, 240.0), 8.0, seam_x, 100.0)
            .expect("the seam between the panes")
    };
    app.main_mut().unwrap().splitter_drag =
        Some(SplitterDragState { splitter: hit.id, axis: hit.axis, last_pos: (seam_x, 100.0) });
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::ColResize));
    app.main_mut().unwrap().splitter_drag = None;
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Crosshair), "after the drag");

    app.interaction.command_palette.open();
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Default));
    app.interaction.command_palette.close();
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Crosshair), "after the palette");

    app.main_mut().unwrap().hover_link = true;
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Pointer));
    app.main_mut().unwrap().invalidate_path_hover();
    app.sync_window_cursors();
    assert_eq!(applied(&app, main).0, Some(CursorIcon::Crosshair), "after the hover");
}

/// A report window (About, Memory Usage) is not a terminal: over its pane the pointer keeps the
/// arrow, not the I-beam a terminal pane shows by default.
#[test]
fn a_report_window_keeps_the_arrow_over_its_pane() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    for report in [app.__test_open_about_window(), app.__test_open_memory_report_window()] {
        assert!(app.__test_set_child_pane_viewport(
            report,
            Rect::new(0.0, 0.0, 800.0, 240.0),
            10.0,
            10.0
        ));
        app.registry.windows.get_mut(&report).unwrap().cursor_pos = (55.0, 85.0);
        assert!(
            crate::app::child_window_pointer::child_pane_cell_at(
                &app.registry.windows[&report],
                55.0,
                85.0
            )
            .is_some(),
            "precondition: the pointer is over the report's pane"
        );
        app.sync_window_cursors();
        assert_eq!(applied(&app, report).0, Some(CursorIcon::Default));
    }
}

/// Collect every first-party `.rs` file under `dir`.
fn rust_files(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            // When: `path` is Rust source, it is scanned.
            found.push(path);
        }
    }
}

/// Only this module writes the native cursor: every other `.set_cursor(` in the crate is a text
/// caret of the palette, a search field or a selection, so each writer goes through one decision.
#[test]
fn only_the_cursor_decision_writes_the_native_cursor() {
    let mut files = Vec::new();
    rust_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut native_writes = 0;
    for path in files {
        let text = std::fs::read_to_string(&path).expect("readable source");
        for (offset, _) in text.match_indices(".set_cursor(") {
            let receiver: String = text[..offset]
                .chars()
                .rev()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if path.ends_with("app/pointer_cursor.rs") {
                native_writes += 1;
                continue;
            }
            assert!(
                ["command_palette", "search", "selection", ""].contains(&receiver.as_str()),
                "{}: `{receiver}.set_cursor(` writes the native cursor outside pointer_cursor.rs",
                path.display()
            );
        }
    }
    assert_eq!(native_writes, 1, "pointer_cursor.rs holds the one native write");
}
