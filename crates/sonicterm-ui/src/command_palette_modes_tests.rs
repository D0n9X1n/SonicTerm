//! Typed palette modes preserve selection, validation and live row identities.

use super::*;

#[test]
fn window_name_validation_preserves_unicode_and_rejects_malformed_names() {
    // Limits count trimmed scalars, while controls and Unicode line separators are rejected before trimming.
    assert_eq!(validate_window_name("  工作 e\u{301}  "), Ok("工作 e\u{301}"));
    assert_eq!(validate_window_name("   "), Ok(""));
    let limit = "界".repeat(128);
    assert_eq!(validate_window_name(&limit), Ok(limit.as_str()));
    assert_eq!(validate_window_name(&"界".repeat(129)), Err(WindowNameError::TooLong));
    for invalid in ["Work\n", "\tWork", "a\0b", "a\u{7f}b", "a\u{2028}b", "a\u{2029}b"] {
        assert_eq!(validate_window_name(invalid), Err(WindowNameError::ControlCharacter));
    }
}

#[test]
fn window_rename_rejects_whole_input_and_keeps_feedback_until_corrected() {
    // A malformed IME/paste chunk must never silently become a different saved name.
    let mut palette = CommandPalette::new();
    palette.start_rename_window("Work");
    palette.input_window_name("\nprivate");
    assert_eq!(palette.query(), "Work");
    assert_eq!(palette.window_name_error(), Some(WindowNameError::ControlCharacter));
    palette.input_window_name("");
    assert_eq!(palette.window_name_error(), Some(WindowNameError::ControlCharacter));
    palette.input_window_name(" space");
    assert_eq!(palette.query(), "Work space");
    assert_eq!(palette.window_name_error(), None);
    palette.set_query("界".repeat(128));
    palette.input_window_name("x");
    assert_eq!(palette.query().chars().count(), 128);
    assert_eq!(palette.window_name_error(), Some(WindowNameError::TooLong));
    palette.backspace();
    assert_eq!(palette.window_name_error(), None);
    assert_eq!(palette.mode(), CommandPaletteMode::RenameWindow);
    assert!(palette.visible().is_empty());
    palette.close();
    palette.open();
    assert_eq!(palette.window_name_error(), None);
}

/// Rename edits use the current caret without re-entering command filtering.
#[test]
fn rename_mode_core_text_edits_use_the_current_caret() {
    let mut palette = CommandPalette::new();
    palette.start_rename_tab("alpha🙂omega");
    palette.apply_text_edit(crate::text_edit::TextEdit::MoveStart);
    palette.apply_text_edit(crate::text_edit::TextEdit::MoveForward);
    palette.apply_text_edit(crate::text_edit::TextEdit::DeleteToEnd);

    assert_eq!(palette.query(), "a");
    assert_eq!(palette.cursor(), 1);
    assert!(palette.visible().is_empty(), "rename mode must not refilter command actions");
}

/// Color navigation preserves the explicit reset row and returns the selected choice.
#[test]
fn tab_color_picker_exposes_selected_choice() {
    let mut palette = CommandPalette::new();
    palette.start_tab_color_picker(
        "#1 work",
        vec![
            TabColorChoice { name: "Reset to Default".into(), hex: None },
            TabColorChoice { name: "ANSI Red".into(), hex: Some("#fb4934".into()) },
            TabColorChoice { name: "ANSI Blue".into(), hex: Some("#83a598".into()) },
        ],
    );

    assert_eq!(palette.mode(), CommandPaletteMode::TabColor);
    assert_eq!(palette.tab_color_title(), "#1 work");
    assert_eq!(palette.len(), 3);
    assert_eq!(palette.selected_tab_color().map(|choice| choice.hex.as_deref()), Some(None));
    palette.move_selection_down();
    assert_eq!(
        palette.selected_tab_color().and_then(|choice| choice.hex.as_deref()),
        Some("#fb4934")
    );
    palette.move_selection_down();
    assert_eq!(palette.selected_tab_color().map(|choice| choice.name.as_str()), Some("ANSI Blue"));
}

/// Window renames validate the whole replacement before committing, preserving caret and anchor on rejection.
#[test]
fn window_rename_replaces_the_selection_atomically() {
    let mut palette = CommandPalette::new();
    let full = "界".repeat(128);
    palette.start_rename_window(full.clone());
    palette.select_all();
    palette.input_window_name(&"新".repeat(128));
    assert_eq!(palette.query(), "新".repeat(128), "a 128-scalar replacement of everything fits");
    assert_eq!(palette.window_name_error(), None);

    palette.set_query(full.clone());
    palette.set_cursor("界".len() * 2);
    palette.extend_to("界".len());
    let (caret, range) = (palette.cursor(), palette.selected_range());
    for (text, error) in [
        ("ab", WindowNameError::TooLong),
        ("a\u{2028}", WindowNameError::ControlCharacter),
        ("a\n", WindowNameError::ControlCharacter),
    ] {
        palette.input_window_name(text);
        assert_eq!(palette.window_name_error(), Some(error), "{text:?}");
        assert_eq!(palette.query(), full, "{text:?} leaves the query untouched");
        assert_eq!((palette.cursor(), palette.selected_range()), (caret, range.clone()));
    }
    // Whole-string input in rename mode shares the atomic validation instead of stripping controls.
    palette.input_str("\u{7}");
    assert_eq!(palette.window_name_error(), Some(WindowNameError::ControlCharacter));
    assert_eq!(palette.query(), full);

    palette.input_window_name("x");
    assert_eq!(palette.query(), format!("界x{}", "界".repeat(126)));
    assert_eq!(palette.window_name_error(), None);
    assert_eq!((palette.cursor(), palette.selected_range()), ("界x".len(), None));
}

/// Contract: a confirmation page lists Apply, Cancel, then the items; it opens on Cancel, arrow
/// keys move only between Apply and Cancel, and closing clears the page.
#[test]
fn confirmation_page_selects_only_apply_or_cancel() {
    let mut palette = CommandPalette::new();
    let session = palette.session();
    palette.start_confirm("Title", "Apply", "Cancel", vec!["one".into(), "two".into()]);
    assert_ne!(palette.session(), session);
    assert_eq!(palette.mode(), CommandPaletteMode::Confirm);
    assert_eq!(palette.confirm_title(), "Title");
    assert_eq!(
        palette.confirm_rows().iter().map(ConfirmRow::label).collect::<Vec<_>>(),
        vec!["Apply", "Cancel", "one", "two"]
    );
    assert!(palette.visible().is_empty(), "no command entry is selectable");
    assert!(!palette.confirm_applies());
    palette.move_selection_down();
    assert!(palette.confirm_applies());
    palette.move_selection_down();
    assert!(!palette.confirm_applies());
    palette.move_selection_up();
    assert!(palette.confirm_applies());
    assert!(!palette.select_visible_index(3), "item rows cannot be selected");
    palette.close();
    assert!(palette.confirm_rows().is_empty());
    assert!(!palette.confirm_applies());
}

/// A status-segment checklist: toggles, one heading with two segments, and Reset.
fn status_rows() -> Vec<StatusSegmentRow> {
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    let row = |kind, label: &str, name: &str| StatusSegmentRow {
        kind,
        label: label.to_owned(),
        name: name.to_owned(),
    };
    vec![
        row(StatusSegmentRowKind::ShowStrip, "✓ Show status line", ""),
        row(StatusSegmentRowKind::HideSensitive, "  Hide sensitive segments", ""),
        row(StatusSegmentRowKind::Heading, "Core", ""),
        row(StatusSegmentRowKind::Segment(Seg::Mode), "✓ Mode — left", "Mode"),
        row(StatusSegmentRowKind::Segment(Seg::Mem), "  Speicher", "Speicher"),
        row(StatusSegmentRowKind::Reset, "Reset to configuration", ""),
    ]
}

#[test]
fn status_segment_headings_are_never_selected() {
    // Up and Down step over the dimmed group headings and wrap at both ends.
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    let mut palette = CommandPalette::new();
    palette.start_status_segments("Segments", "footer", status_rows(), None);
    assert_eq!(palette.mode(), CommandPaletteMode::StatusSegments);
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::ShowStrip));
    palette.move_selection_down();
    palette.move_selection_down();
    assert_eq!(
        palette.selected_status_row(),
        Some(StatusSegmentRowKind::Segment(Seg::Mode)),
        "the heading is skipped"
    );
    palette.move_selection_up();
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::HideSensitive));
    palette.move_selection_up();
    palette.move_selection_up();
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::Reset), "it wraps");
    palette.start_status_segments("Segments", "footer", status_rows(), Some(Seg::Mem));
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::Segment(Seg::Mem)));
}

#[test]
fn status_segment_typing_filters_segments_by_id_and_localized_name() {
    // A query keeps only segment rows whose id or localized name contains it, case-insensitively.
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    let mut palette = CommandPalette::new();
    palette.start_status_segments("Segments", "footer", status_rows(), None);
    for character in "SPEI".chars() {
        palette.input_char(character);
    }
    let kinds: Vec<_> = palette.status_segment_rows().iter().map(|row| row.kind).collect();
    assert_eq!(kinds, [StatusSegmentRowKind::Segment(Seg::Mem)], "by localized name");
    palette.set_query("mem");
    let kinds: Vec<_> = palette.status_segment_rows().iter().map(|row| row.kind).collect();
    assert_eq!(kinds, [StatusSegmentRowKind::Segment(Seg::Mem)], "by id");
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::Segment(Seg::Mem)));
    palette.set_query("");
    assert_eq!(
        palette.status_segment_rows().len(),
        status_rows().len(),
        "an empty query lists all"
    );
}

#[test]
fn status_segment_rows_refresh_in_place_and_close_clean() {
    // A change rebuilds the rows without moving the highlight or the query; closing drops them.
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    let mut palette = CommandPalette::new();
    palette.start_status_segments("Segments", "footer", status_rows(), Some(Seg::Mem));
    let mut rows = status_rows();
    rows[4].label = "✓ Speicher — right".to_owned();
    palette.update_status_segments(rows);
    assert_eq!(palette.selected_status_row(), Some(StatusSegmentRowKind::Segment(Seg::Mem)));
    assert_eq!(palette.status_segment_rows()[4].label, "✓ Speicher — right");
    palette.close();
    assert!(palette.status_segment_rows().is_empty());
    assert_eq!(palette.selected_status_row(), None);
}

#[test]
fn a_checklist_row_change_changes_the_presentation_hash() {
    // The renderer keys the retained frame on this hash: a toggle that only changes a row's check
    // mark or list must change it, or the open checklist keeps showing the old state.
    let mut palette = CommandPalette::new();
    palette.start_status_segments("Segments", "footer", status_rows(), None);
    let before = palette.presentation_hash();
    let mut rows = status_rows();
    rows[0].label = "  Show status line".to_owned();
    palette.update_status_segments(rows);
    assert_ne!(palette.presentation_hash(), before, "a check mark changed");
    let toggled = palette.presentation_hash();
    palette.update_status_segments(palette.status_segment_rows().into_iter().cloned().collect());
    assert_eq!(palette.presentation_hash(), toggled, "unchanged rows keep the hash");
}

/// The font picker highlights the live family, filters by the typed query without case, and
/// closes back to the command list with its rows gone.
#[test]
fn font_family_picker_filters_and_highlights_the_live_family() {
    let mut palette = CommandPalette::new();
    let rows: Vec<PickerRow> = ["Consolas", "Menlo", "Monaco"]
        .iter()
        .map(|family| PickerRow { value: (*family).into(), label: format!("  {family}") })
        .collect();
    palette.start_picker(PickerKind::FontFamily, "Type to filter fonts", "footer", rows, "Monaco");
    assert_eq!(palette.mode(), CommandPaletteMode::Picker);
    assert_eq!(palette.picker_kind(), Some(PickerKind::FontFamily));
    assert_eq!(palette.selected_picker_value(), Some("Monaco"));
    palette.input_str("MEN");
    let kept: Vec<&str> = palette.picker_rows().iter().map(|row| row.value.as_str()).collect();
    assert_eq!(kept, ["Menlo"]);
    assert_eq!(palette.selected_picker_value(), Some("Menlo"));
    palette.close();
    assert_eq!(palette.mode(), CommandPaletteMode::Commands);
    assert_eq!(palette.picker_kind(), None);
    assert!(palette.picker_rows().is_empty());
}

/// A theme picker filters on the row label, so typing `light` keeps the rows tagged light, and a
/// late font catalog refresh cannot replace its rows: `update_picker` only touches its own kind.
#[test]
fn theme_picker_filters_by_label_and_ignores_another_kinds_refresh() {
    let mut palette = CommandPalette::new();
    let rows: Vec<PickerRow> =
        [("github-light", "light"), ("nord", "dark"), ("rose-pine-dawn", "light")]
            .iter()
            .map(|(name, tone)| PickerRow {
                value: (*name).into(),
                label: format!("  {name} · {tone}"),
            })
            .collect();
    palette.start_picker(PickerKind::Theme, "Type to filter themes", "footer", rows, "nord");
    assert_eq!(palette.picker_kind(), Some(PickerKind::Theme));
    assert_eq!(palette.selected_picker_value(), Some("nord"));
    // Theme names match exactly: `Nord` is another file, so the live `nord` row stays highlighted.
    let mut cased = palette.clone();
    let rows = vec![
        PickerRow { value: "Nord".into(), label: "  Nord · light".into() },
        PickerRow { value: "nord".into(), label: "✓ nord · dark".into() },
    ];
    cased.start_picker(PickerKind::Theme, "themes", "footer", rows, "nord");
    assert_eq!(cased.selected_picker_value(), Some("nord"));
    let font_rows = vec![PickerRow { value: "Menlo".into(), label: "  Menlo".into() }];
    palette.update_picker(PickerKind::FontFamily, "fonts", font_rows);
    assert_eq!(
        palette.picker_title(),
        "Type to filter themes",
        "another kind's refresh is ignored"
    );
    palette.input_str("LIGHT");
    let kept: Vec<&str> = palette.picker_rows().iter().map(|row| row.value.as_str()).collect();
    assert_eq!(kept, ["github-light", "rose-pine-dawn"]);
}
