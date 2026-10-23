use super::*;
use crate::app::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::status_line::{StatusSegmentId as Seg, StatusSide};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::StatusSegmentRowKind as Kind;
use winit::keyboard::{Key, NamedKey};

/// A headless App with the strip on and one child window, the checklist's usual host.
fn checklist_app() -> (App, winit::window::WindowId) {
    let mut config = Config::default();
    config.status_line.enabled = true;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let child = app.__test_seed_child_window(&["work"]);
    (app, child)
}

/// The kinds of the checklist's rows, in display order.
fn kinds(app: &App) -> Vec<Kind> {
    app.command_palette.status_segment_rows().iter().map(|row| row.kind).collect()
}

/// The label of `id`'s row.
fn label(app: &App, id: Seg) -> String {
    let rows = app.command_palette.status_segment_rows();
    rows.iter().find(|row| row.kind == Kind::Segment(id)).map(|row| row.label.clone()).unwrap()
}

#[test]
fn the_checklist_lists_every_implemented_segment_grouped_in_catalog_order() {
    // Toggles first, then one heading per group that has an implemented segment, in catalog
    // order, then Reset; every implemented segment appears exactly once, shown ones checked.
    let (mut app, child) = checklist_app();
    assert!(app.run_action_for_window(&Action::ChooseStatusLineSegments, child));
    let rows = kinds(&app);
    assert_eq!(rows[..2], [Kind::ShowStrip, Kind::HideSensitive]);
    assert_eq!(rows.last(), Some(&Kind::Reset));
    let shown: Vec<Seg> = rows
        .iter()
        .filter_map(|kind| match kind {
            Kind::Segment(id) => Some(*id),
            _ => None,
        })
        .collect();
    let implemented: Vec<Seg> = GROUPS
        .iter()
        .flat_map(|(group, _)| {
            Seg::ALL
                .into_iter()
                .filter(move |id| id.meta().implemented && id.meta().group == *group)
        })
        .collect();
    assert_eq!(shown, implemented, "every implemented segment, grouped in catalog order");
    let headings = rows.iter().filter(|kind| **kind == Kind::Heading).count();
    let groups_with_segments = GROUPS
        .iter()
        .filter(|(group, _)| {
            Seg::ALL.iter().any(|id| id.meta().implemented && id.meta().group == *group)
        })
        .count();
    assert_eq!(
        headings, groups_with_segments,
        "a group with no implemented segment has no heading"
    );
    let mem_side = app.current_status_line().segment(Seg::Mem).map(|segment| segment.side);
    assert_eq!(mem_side, Some(StatusSide::Right), "the developer preset shows mem on the right");
    assert_eq!(label(&app, Seg::Mem), "✓ Mem — right");
    assert_eq!(app.palette_attached_window, Some(child), "attached to the window it came from");
}

#[test]
fn enter_toggles_and_keeps_the_page_open_and_arrows_move_a_checked_segment() {
    // Enter enables `cpu` at its home list without closing the page; Left moves it right to
    // center to left; Enter disables it; Left on an unchecked row moves nothing; Escape closes.
    let (mut app, child) = checklist_app();
    app.open_status_segments(Some(child), Some(Seg::Cpu));
    let side =
        |app: &mut App| app.current_status_line().segment(Seg::Cpu).map(|segment| segment.side);
    assert_eq!(side(&mut app), None);
    assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter)));
    assert!(app.command_palette.is_open(), "Enter keeps the checklist open");
    assert_eq!(side(&mut app), Some(StatusSide::Right));
    assert_eq!(app.command_palette.selected_status_row(), Some(Kind::Segment(Seg::Cpu)));
    assert!(label(&app, Seg::Cpu).starts_with('✓'), "the row is redrawn checked");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowLeft));
    assert_eq!(side(&mut app), Some(StatusSide::Center));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowLeft));
    assert_eq!(side(&mut app), Some(StatusSide::Left));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowRight));
    assert_eq!(side(&mut app), Some(StatusSide::Center));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert_eq!(side(&mut app), None, "Enter on a checked row disables it");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowLeft));
    assert_eq!(side(&mut app), None, "an unchecked row does not move");
    assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape)));
    assert!(!app.command_palette.is_open());
    assert_eq!(app.palette_attached_window, None);
}

#[test]
fn the_toggle_rows_and_reset_change_the_session_only() {
    // Show and Hide sensitive mirror the toggle actions; Reset drops every choice.
    let (mut app, child) = checklist_app();
    app.open_status_segments(Some(child), Some(Seg::Cpu));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    app.open_status_segments(Some(child), None);
    assert_eq!(app.command_palette.selected_status_row(), Some(Kind::ShowStrip));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert!(app.current_status_line().hide_sensitive);
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert!(!app.current_status_line().enabled, "Show status line turns the strip off");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    assert_eq!(app.command_palette.selected_status_row(), Some(Kind::Reset), "Up wraps to Reset");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    let after = app.current_status_line();
    assert!(after.enabled && !after.hide_sensitive && after.segment(Seg::Cpu).is_none());
    assert!(app.command_palette.is_open());
}

#[test]
fn the_checklist_command_is_allowed_in_readonly_panes() {
    // It changes no terminal input, so a READONLY pane may open it.
    let descriptor = sonicterm_ui::command_label::descriptor(&Action::ChooseStatusLineSegments);
    assert!(descriptor.read_only_allowed);
}

#[test]
fn every_checklist_label_exists_in_every_locale() {
    // Command, page, toggle, group, list, tag and segment-name keys are in en, ja and zh-CN.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/i18n");
    for locale in ["en", "ja", "zh-CN"] {
        let text = std::fs::read_to_string(root.join(locale).join("messages.ftl")).unwrap();
        for key in label_keys() {
            assert!(
                text.lines().any(|line| line.starts_with(&format!("{key} ="))),
                "{locale} lacks {key}"
            );
        }
    }
}

#[test]
fn sensitive_rows_stay_listed_and_toggleable_while_hidden() {
    // With sensitive segments hidden, a sensitive row is still listed, tagged with the localized
    // `sensitive · hidden`, and Enter still toggles it.
    for (locale, tag) in
        [("en", "sensitive · hidden"), ("ja", "機密 · 非表示"), ("zh-CN", "敏感 · 已隐藏")]
    {
        let (mut app, child) = checklist_app();
        app.set_locale(locale);
        app.toggle_status_line_sensitive();
        let sensitive = Seg::ALL
            .into_iter()
            .find(|id| {
                id.meta().implemented
                    && id.meta().sensitivity != sonicterm_cfg::status_line::Sensitivity::None
            })
            .expect("the catalog has a sensitive segment");
        app.open_status_segments(Some(child), Some(sensitive));
        assert!(label(&app, sensitive).ends_with(tag), "{locale}: {}", label(&app, sensitive));
        let shown = |app: &mut App| app.current_status_line().segment(sensitive).is_some();
        let before = shown(&mut app);
        app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
        assert_ne!(shown(&mut app), before, "{locale}: the hidden row still toggles");
        assert!(label(&app, sensitive).ends_with(tag), "{locale}");
    }
}
