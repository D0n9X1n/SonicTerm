use super::*;
use crate::i18n::test_translator as translator;
use sonicterm_cfg::keymap::{ActionWrapper, Binding, Keymap, Meta};
use PaletteEntry::Command;

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

/// Pointer highlighting validates display indices without bypassing disabled commands or other picker modes.
#[test]
fn pointer_selection_is_bounded_and_preserves_command_availability() {
    let mut palette = CommandPalette::new();
    palette.open();
    palette.set_query("Copy to Clipboard");
    assert!(palette.select_visible_index(0));
    assert_eq!(palette.highlighted(), Some(&Command(Action::CopyToClipboard)));
    assert!(palette.current().is_none());
    assert!(!palette.select_visible_index(usize::MAX));
    assert_eq!(palette.selected(), 0);
    palette.set_query("");
    palette.set_visible_rows(2);
    let last = palette.len() - 1;
    assert!(palette.select_visible_index(last));
    assert_eq!(palette.scroll_offset(), last - 1);
    palette.start_rename_tab("literal title");
    assert!(!palette.select_visible_index(0));
    assert_eq!(palette.query(), "literal title");
    palette.start_tab_color_picker(
        "title",
        vec![TabColorChoice { name: "default".into(), hex: None }],
    );
    assert!(!palette.select_visible_index(0));
    assert_eq!(palette.selected(), 0);
}

/// Entry identity distinguishes action parameters and tab IDs while ignoring tab presentation changes.
#[test]
fn pointer_entry_identity_ignores_only_tab_presentation() {
    let mut tabs = TabBar::new();
    let id = tabs.push(crate::tabs::Tab::new("original"));
    let other = tabs.push(crate::tabs::Tab::new("original"));
    let entry = PaletteEntry::Tab { id, title: "original".into(), position: 0 };
    assert!(entry.same_identity(&PaletteEntry::Tab { id, title: "renamed".into(), position: 1 }));
    assert!(!entry.same_identity(&PaletteEntry::Tab {
        id: other,
        title: "original".into(),
        position: 0
    }));
    assert!(!entry.same_identity(&Command(Action::ActivateTab(0))));
    assert!(!Command(Action::ActivateTab(0)).same_identity(&Command(Action::ActivateTab(1))));
}

/// The overflow selector exposes every live tab in order and never includes commands or a closed target.
#[test]
fn tab_selector_keeps_only_live_targets_reachable_by_arrows() {
    let mut tabs = TabBar::new();
    for index in 0..24 {
        tabs.push(crate::tabs::Tab::new(format!("terminal {index}")));
    }
    let mut palette = CommandPalette::new();
    palette.set_tabs(&tabs, &translator("en"));
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: tabs.len(),
        ..CommandContext::default()
    });
    palette.open_tabs();
    palette.set_visible_rows(3);
    assert_eq!(palette.len(), tabs.len());
    for tab in tabs.tabs() {
        assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == tab.id));
        assert!(palette.selected() >= palette.scroll_offset());
        assert!(palette.selected() < palette.scroll_offset() + 3);
        palette.move_selection_down();
    }
    assert_eq!(palette.selected(), 0);
    palette.set_query("new window");
    assert!(palette.is_empty());
    palette.set_query("terminal 23");
    let target = tabs.tabs()[23].id;
    let position = palette
        .visible()
        .iter()
        .position(|entry| matches!(entry, PaletteEntry::Tab { id, .. } if *id == target))
        .unwrap();
    assert!(palette.select_visible_index(position));
    assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == target));
    tabs.close(target);
    palette.set_tabs(&tabs, &translator("en"));
    assert!(palette.current().is_none());
    palette.close();
    palette.open();
    palette.set_query("new window");
    assert_eq!(palette.current(), Some(&Command(Action::NewWindow)));
}

/// Go to Tab rows use runtime identity across duplicate titles, reorder, rename, and replacement.
#[test]
fn go_to_tab_tracks_identity_instead_of_title_or_position() {
    use crate::tabs::{Tab, TabBar};
    let mut tabs = TabBar::new();
    let first = tabs.push(Tab::new("duplicate"));
    let second = tabs.push(Tab::new("duplicate"));
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 2,
        ..CommandContext::default()
    });
    palette.set_tabs(&tabs, &translator("en"));
    palette.open();
    palette.set_query("Go to Tab");
    palette.set_visible_rows(1);
    assert_eq!(
        palette.visible().iter().filter(|entry| matches!(entry, PaletteEntry::Tab { .. })).count(),
        2
    );
    assert!(
        matches!(palette.current(), Some(PaletteEntry::Tab { id, position: 0, .. }) if *id == first)
    );
    palette.move_selection_down();
    assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == second));
    let hash = palette.presentation_hash();
    palette.set_tabs(&tabs, &translator("en"));
    assert_eq!(palette.presentation_hash(), hash);
    tabs.reorder(1, 0);
    tabs.set_title(second, "renamed");
    palette.set_tabs(&tabs, &translator("en"));
    assert!(
        matches!(palette.current(), Some(PaletteEntry::Tab { id, position: 0, title }) if *id == second && title == "renamed")
    );
    tabs.close(second);
    let replacement = tabs.push(Tab::new("renamed"));
    palette.set_tabs(&tabs, &translator("en"));
    assert!(
        palette.current().is_none(),
        "a replacement cannot inherit selection from a closed tab"
    );
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 2,
        read_only: true,
        ..CommandContext::default()
    });
    assert!(palette.current().is_none(), "later refresh cannot select an unchosen replacement");
    palette.move_selection_down();
    assert!(palette.current().is_some());
    palette.set_query("renamed");
    assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == replacement));
    assert_eq!(palette.shortcut_hint_for_visible_index(palette.selected()), None);
}

#[test]
fn changing_one_tab_title_retains_other_presentations() {
    // Unchanged tab entries keep their owned label/search allocations during a peer's title refresh.
    let mut tabs = TabBar::new();
    let first = tabs.push(crate::tabs::Tab::new("first"));
    let second = tabs.push(crate::tabs::Tab::new("second"));
    let i18n = translator("en");
    let mut palette = CommandPalette::new();
    palette.set_tabs(&tabs, &i18n);
    let index = palette
        .all
        .iter()
        .position(|entry| matches!(entry, PaletteEntry::Tab { id, .. } if *id == first))
        .unwrap();
    let label = palette.presentation[index].label.as_ptr();
    let search = palette.presentation[index].search.as_ptr();
    tabs.set_title(second, "renamed");
    palette.set_tabs(&tabs, &i18n);
    assert_eq!(palette.presentation[index].label.as_ptr(), label);
    assert_eq!(palette.presentation[index].search.as_ptr(), search);
    assert!(palette.presentation[index + 1].label.contains("renamed"));
}

/// Same-title replacement invalidates retained frame identity even when rendered strings are identical.
#[test]
fn go_to_tab_same_title_replacement_changes_identity_and_keeps_refresh_coherent() {
    let mut tabs = TabBar::new();
    let original = tabs.push(crate::tabs::Tab::new("same"));
    let i18n = translator("en");
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 1,
        ..CommandContext::default()
    });
    palette.set_tabs(&tabs, &i18n);
    palette.open();
    palette.set_query("Go to Tab");
    let old_hash = palette.presentation_hash();
    let old_label = palette.label_for_visible_index(palette.selected()).unwrap().to_string();
    tabs.close(original);
    let replacement = tabs.push(crate::tabs::Tab::new("same"));
    palette.set_tabs(&tabs, &i18n);
    assert_ne!(palette.presentation_hash(), old_hash);
    assert!(palette.current().is_none());
    palette.set_keymap(&Keymap::default(), &i18n);
    palette.set_locale(&translator("ja"));
    assert!(palette.current().is_none());
    palette.set_locale(&i18n);
    palette.move_selection_up();
    assert!(palette.current().is_some());
    palette.set_query("Go to Tab");
    assert_eq!(palette.label_for_visible_index(0), Some(old_label.as_str()));
    assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == replacement));
    palette.start_rename_tab("unchanged");
    tabs.set_title(replacement, "changed");
    palette.set_tabs(&tabs, &i18n);
    assert_eq!(palette.query(), "unchanged");
    palette.open();
    palette.set_query("changed");
    assert!(matches!(palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == replacement));
}

/// Disabled rows stay searchable and highlighted while only live context can make them executable.
#[test]
fn disabled_commands_preserve_identity_and_never_become_current() {
    let mut palette = CommandPalette::new();
    palette.open();
    palette.set_query("Copy to Clipboard");
    assert_eq!(palette.highlighted(), Some(&Command(Action::CopyToClipboard)));
    assert_eq!(palette.current(), None);
    assert_eq!(
        palette.disabled_reason_for_visible_index(palette.selected()),
        Some(DisabledReason::NoWindow)
    );
    let mut context = CommandContext {
        window_available: true,
        tab_count: 2,
        pane_available: true,
        ..CommandContext::default()
    };
    palette.move_cursor_left();
    let caret = palette.cursor();
    let before = palette.presentation_hash();
    palette.set_context(context);
    assert_ne!(palette.presentation_hash(), before);
    assert_eq!(
        palette.disabled_reason_for_visible_index(palette.selected()),
        Some(DisabledReason::NoSelection)
    );
    let unchanged = palette.presentation_hash();
    palette.set_context(context);
    assert_eq!(palette.presentation_hash(), unchanged);
    context.selection_available = true;
    palette.set_context(context);
    assert_eq!(palette.current(), Some(&Command(Action::CopyToClipboard)));
    context.selection_available = false;
    palette.set_context(context);
    assert_eq!(palette.highlighted(), Some(&Command(Action::CopyToClipboard)));
    assert_eq!(palette.current(), None);
    assert_eq!(palette.query(), "Copy to Clipboard");
    assert_eq!(palette.cursor(), caret);
    palette.set_locale(&translator("ja"));
    assert_eq!(palette.highlighted(), Some(&Command(Action::CopyToClipboard)));
    assert_eq!(palette.current(), None);
}

/// Empty results are grouped stably without changing canonical fuzzy-score ties or command reachability.
#[test]
fn categories_group_empty_query_and_keep_search_order() {
    let mut expected = palette_actions();
    expected.sort_by_key(|action| crate::command_label::descriptor(action).category);
    let mut palette = CommandPalette::new();
    assert_eq!(
        palette.visible().into_iter().cloned().collect::<Vec<_>>(),
        expected.into_iter().map(Command).collect::<Vec<_>>()
    );
    let pattern = Pattern::parse("create", CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut scratch = Vec::new();
    let canonical = palette_actions();
    let mut scored: Vec<_> = canonical
        .iter()
        .enumerate()
        .filter_map(|(index, action)| {
            let label = search_haystack(action);
            let score = pattern.score(Utf32Str::new(&label, &mut scratch), &mut matcher)?;
            Some((index, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let expected: Vec<_> =
        scored.iter().map(|(index, _)| Command(canonical[*index].clone())).collect();
    palette.set_query("create");
    assert_eq!(palette.visible().into_iter().cloned().collect::<Vec<_>>(), expected);
    assert_eq!(
        palette.visible().get(..2),
        Some([&Command(Action::NewTab), &Command(Action::NewWindow)].as_slice())
    );
}

/// Context refresh cannot replace color-choice indices or reset a noncommand selection.
#[test]
fn context_refresh_keeps_color_picker_indices_and_selection() {
    let mut palette = CommandPalette::new();
    palette.start_tab_color_picker(
        "title",
        vec![
            TabColorChoice { name: "one".into(), hex: None },
            TabColorChoice { name: "two".into(), hex: Some("#123456".into()) },
        ],
    );
    palette.move_selection_down();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 4,
        pane_available: true,
        ..CommandContext::default()
    });
    assert_eq!(palette.len(), 2);
    assert_eq!(palette.selected(), 1);
    assert_eq!(palette.selected_tab_color().unwrap().name, "two");
    assert!(palette.current().is_none());
    assert!(palette.highlighted().is_none());
    palette.move_selection_down();
    assert_eq!(palette.selected(), 0);
}

#[test]
fn palette_defaults_do_not_expose_placeholder_parameter_actions() {
    let actions = palette_actions();
    assert!(!actions.iter().any(|a| matches!(a, Action::ApplyTheme(_))));
    assert!(!actions.iter().any(|a| matches!(a, Action::OpenSshPane(_))));
    assert!(actions.iter().any(|a| matches!(a, Action::OpenCommandPalette)));
    assert!(actions.iter().any(|a| matches!(a, Action::UpdateTabColor)));
    assert!(actions.iter().any(|a| matches!(a, Action::MoveTabToNewWindow)));
    assert!(actions
        .iter()
        .any(|a| { matches!(a, Action::ResizePane { dir: Direction::Left, amount: 5 }) }));
    assert!(covers_every_variant_kind());
}

#[test]
fn move_tab_to_new_window_is_searchable_and_unbound_by_default() {
    let mut palette = CommandPalette::new();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    palette.set_query("detach");

    let visible = palette.visible();
    let index = visible
        .iter()
        .position(|action| matches!(action, Command(Action::MoveTabToNewWindow)))
        .expect("detach should find Move Tab to New Window");
    assert_eq!(palette.label_for_visible_index(index), Some("Move Tab to New Window"));
    assert_eq!(palette.shortcut_hint_for_visible_index(index), None);
}

#[test]
fn save_current_settings_is_present_exhaustively_labeled_and_unbound_by_default() {
    // Contract: the save action is fully represented in the palette but has no default shortcut.
    let actions = palette_actions();
    assert!(actions.contains(&Action::SaveCurrentSettings));
    assert!(covers_every_variant_kind());
    assert_eq!(crate::command_label::label(&Action::SaveCurrentSettings), "Save Current Settings");
    assert_eq!(action_display_name(&Action::SaveCurrentSettings), "SaveCurrentSettings");

    let mut palette = CommandPalette::new();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    let index = palette
        .visible()
        .iter()
        .position(|action| matches!(action, Command(Action::SaveCurrentSettings)))
        .expect("Save Current Settings should be in the default palette");
    assert_eq!(palette.shortcut_hint_for_visible_index(index), None);
}

#[test]
fn save_current_settings_is_searchable_by_expected_terms() {
    // Contract: common persistence and font-setting terms all discover the save action.
    for query in ["save", "persist", "save font", "current settings"] {
        let mut palette = CommandPalette::new();
        palette.set_keymap(&Keymap::default(), &translator("en"));
        palette.set_query(query);
        assert!(
            palette
                .visible()
                .iter()
                .any(|action| matches!(action, Command(Action::SaveCurrentSettings))),
            "{query:?} should find Save Current Settings"
        );
    }
}

/// Concrete user-bound actions expose the native hint while unsupported placeholder actions stay hidden.
#[test]
fn palette_imports_concrete_keymap_theme_actions_and_shortcuts() {
    let keymap = Keymap {
        meta: Meta { name: "test".into(), version: "1.0".into() },
        bindings: vec![
            Binding {
                keys: "super+shift+y".into(),
                action: ActionWrapper(Action::ApplyTheme("wezterm".into())),
            },
            Binding {
                keys: "super+shift+s".into(),
                action: ActionWrapper(Action::OpenSshPane("alice@example.com".into())),
            },
        ],
    };
    let mut palette = CommandPalette::new();
    palette.set_keymap(&keymap, &translator("en"));
    let visible = palette.visible();
    let theme_idx = visible
        .iter()
        .position(|a| matches!(a, Command(Action::ApplyTheme(name)) if name == "wezterm"))
        .expect("concrete keymap theme action should be visible");
    let expected = if cfg!(target_os = "macos") {
        "⌘⇧Y"
    } else if cfg!(target_os = "windows") {
        "Win+Shift+Y"
    } else {
        "Super+Shift+Y"
    };
    assert_eq!(palette.shortcut_hint_for_visible_index(theme_idx), Some(expected));
    assert!(!visible.iter().any(|a| matches!(a, Command(Action::OpenSshPane(_)))));
}

/// Cached phrases preserve catalog word order and insert literal values without rescanning them.
#[test]
fn palette_text_slots_preserve_order_and_literal_marker_values() {
    let literal = format!("user {{name}} {TEXT_SLOT} 中文");
    for (before, after) in [("prefix ", ""), ("", " suffix"), ("prefix ", " suffix")] {
        let slot = TextSlot::new(format!("{before}{TEXT_SLOT}{after}"));
        assert_eq!(slot.render(&literal), format!("{before}{literal}{after}"));
    }
    for locale in crate::i18n::SHIPPED_LOCALES {
        let i18n = translator(locale);
        for kind in ["one", "other"] {
            let rendered = i18n
                .try_t_args(
                    "palette-command-footer",
                    Some(&[("count", TEXT_SLOT), ("count-kind", kind)]),
                )
                .unwrap();
            assert_eq!(rendered.matches(TEXT_SLOT).count(), 1);
        }
        let title = i18n.try_t_args("palette-color-title", Some(&[("title", TEXT_SLOT)])).unwrap();
        assert_eq!(title.matches(TEXT_SLOT).count(), 1);
        assert!(!title.contains('▏'), "the caret is owned by layout, not translations");
        let text = PaletteText::new(Some(&i18n));
        assert_eq!(text.color_title(&literal).matches(TEXT_SLOT).count(), 1);
        assert!(text.color_title(&literal).ends_with('▏'));
    }
}

/// Search and display follow the first live binding while existing semantic aliases remain searchable.
#[test]
fn palette_searches_the_first_live_native_shortcut_hint() {
    let action = Action::ApplyTheme("custom-user-theme".into());
    let keymap = Keymap {
        meta: Meta { name: "live-shortcut".into(), version: "1.0".into() },
        bindings: vec![
            Binding { keys: "ctrl+alt+y".into(), action: ActionWrapper(action.clone()) },
            Binding { keys: "super+shift+x".into(), action: ActionWrapper(action.clone()) },
        ],
    };
    let expected = if cfg!(target_os = "macos") { "⌃⌥Y" } else { "Ctrl+Alt+Y" };
    let mut palette = CommandPalette::new();
    palette.set_keymap(&keymap, &translator("en"));
    palette.set_query(expected);
    let visible = palette.visible();
    let index = visible
        .iter()
        .position(|candidate| **candidate == Command(action.clone()))
        .expect("the live shortcut must find its concrete action");
    assert_eq!(palette.shortcut_hint_for_visible_index(index), Some(expected));
    assert_eq!(palette.current(), Some(&Command(action.clone())));
    palette.open();
    palette.set_query(expected);
    let layout = crate::overlays::PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0)
        .expect("open palette produces the renderer's display model");
    let row = layout
        .row_labels
        .iter()
        .position(|label| label == "Apply Theme: custom-user-theme")
        .expect("the live action remains in the displayed rows");
    assert_eq!(layout.row_shortcuts[row].as_deref(), Some(expected));
    palette.set_query("cmd+q");
    assert!(palette
        .visible()
        .iter()
        .any(|candidate| matches!(candidate, Command(Action::QuitApp))));
}

#[test]
fn palette_disables_activate_tab_entries_beyond_current_tab_count() {
    // Missing tab targets remain searchable with a reason, but are never executable.
    let keymap = Keymap {
        meta: Meta { name: "test".into(), version: "1.0".into() },
        bindings: vec![
            Binding { keys: "super+1".into(), action: ActionWrapper(Action::ActivateTab(0)) },
            Binding { keys: "super+2".into(), action: ActionWrapper(Action::ActivateTab(1)) },
            Binding { keys: "super+3".into(), action: ActionWrapper(Action::ActivateTab(2)) },
        ],
    };
    let mut palette = CommandPalette::new();
    palette.set_keymap(&keymap, &translator("en"));
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 2,
        ..CommandContext::default()
    });
    let visible = palette.visible();
    assert!(visible.iter().any(|a| matches!(a, Command(Action::ActivateTab(0)))));
    assert!(visible.iter().any(|a| matches!(a, Command(Action::ActivateTab(1)))));
    let missing =
        visible.iter().position(|a| matches!(a, Command(Action::ActivateTab(2)))).unwrap();
    assert_eq!(
        palette.disabled_reason_for_visible_index(missing),
        Some(DisabledReason::MissingTab)
    );
    palette.set_query("Activate Tab 3");
    assert_eq!(palette.highlighted(), Some(&Command(Action::ActivateTab(2))));
    assert!(palette.current().is_none());
}

#[test]
fn palette_query_height_scales_on_large_window() {
    use crate::overlays::PaletteLayout;
    // Huge window so the window-relative clamps never bind; only the
    // SIZE terms drive the layout, so a pure SIZE field (the query-row
    // height) must double at 2x. panel_padding is held at 0 so it does
    // not enter this assertion.
    let mut palette = CommandPalette::new();
    palette.open();
    let one = PaletteLayout::compute(&mut palette, 4000.0, 2400.0, 0.0, 1.0)
        .expect("open palette yields a layout");
    let two = PaletteLayout::compute(&mut palette, 4000.0, 2400.0, 0.0, 2.0)
        .expect("open palette yields a layout");
    assert_eq!(two.query_row.h, one.query_row.h * 2.0);
}

#[test]
fn palette_text_editing_supports_space_cjk_and_caret_movement() {
    let mut palette = CommandPalette::new();
    palette.open();
    for ch in "rename".chars() {
        palette.input_char(ch);
    }
    palette.input_char(' ');
    palette.input_char('标');
    palette.input_char('题');
    assert_eq!(palette.query(), "rename 标题");
    assert_eq!(palette.cursor(), "rename 标题".len());

    palette.move_cursor_left();
    palette.move_cursor_left();
    palette.input_char('-');
    assert_eq!(palette.query(), "rename -标题");
    palette.backspace();
    assert_eq!(palette.query(), "rename 标题");
}

#[test]
fn palette_core_text_edits_refilter_and_preserve_unicode_suffixes() {
    let mut palette = CommandPalette::new();
    palette.open();
    palette.set_query("rename 标题 tail");
    let filtered_len = palette.len();

    palette.apply_text_edit(crate::text_edit::TextEdit::DeletePreviousWord);

    assert_eq!(palette.query(), "rename 标题 ");
    assert_eq!(palette.cursor(), "rename 标题 ".len());
    assert!(palette.len() >= filtered_len, "editing the command query must refilter results");
}

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
    assert_eq!(palette.selected_tab_color().map(|c| c.hex.as_deref()), Some(None));
    palette.move_selection_down();
    assert_eq!(palette.selected_tab_color().and_then(|c| c.hex.as_deref()), Some("#fb4934"));
    palette.move_selection_down();
    assert_eq!(palette.selected_tab_color().map(|c| c.name.as_str()), Some("ANSI Blue"));
}

/// The weight commands exist so `weight_scale` is reachable without editing
/// `sonicterm.toml`. Searching the words a user would actually type has to
/// surface them, or the feature is invisible.
#[test]
fn font_weight_commands_are_searchable_by_bolder_and_thinner() {
    for (query, expected, label) in [
        ("bolder", Action::IncreaseFontWeight, "Increase Font Weight (Bolder)"),
        ("thinner", Action::DecreaseFontWeight, "Decrease Font Weight (Thinner)"),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_keymap(&Keymap::default(), &translator("en"));
        palette.set_query(query);

        let visible = palette.visible();
        let index = visible
            .iter()
            .position(|action| **action == Command(expected.clone()))
            .unwrap_or_else(|| panic!("{query:?} should find {label}"));
        assert_eq!(palette.label_for_visible_index(index), Some(label));
    }
}

/// Alternate wordings people reach for, so the commands are not findable only
/// by their exact label.
#[test]
fn font_weight_commands_are_searchable_by_synonyms() {
    for (query, expected) in [
        ("heavier", Action::IncreaseFontWeight),
        ("thicker", Action::IncreaseFontWeight),
        ("bolder font", Action::IncreaseFontWeight),
        ("bold font", Action::IncreaseFontWeight),
        ("lighter", Action::DecreaseFontWeight),
        ("slimmer", Action::DecreaseFontWeight),
        ("thinner font", Action::DecreaseFontWeight),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_keymap(&Keymap::default(), &translator("en"));
        palette.set_query(query);
        assert!(
            palette.visible().iter().any(|action| **action == Command(expected.clone())),
            "{query:?} should surface {expected:?}"
        );
    }
}

#[test]
fn reset_font_weight_is_in_the_palette() {
    let mut palette = CommandPalette::new();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    palette.set_query("reset font weight");
    let visible = palette.visible();
    let index = visible
        .iter()
        .position(|action| matches!(action, Command(Action::ResetFontWeight)))
        .expect("reset font weight should be searchable");
    assert_eq!(palette.label_for_visible_index(index), Some("Reset Font Weight to Config"));
}
