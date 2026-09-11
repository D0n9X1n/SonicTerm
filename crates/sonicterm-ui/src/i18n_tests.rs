use super::*;
use crate::command_label::CommandContext;
use crate::command_palette::PaletteEntry::Command;

/// Build a fixture without consulting process-global locale overrides.
pub(crate) fn translator(locale: &str) -> I18n {
    I18n {
        active: locale.parse().unwrap(),
        active_bundle: build_bundle(locale),
        fallback: build_bundle("en"),
    }
}

/// Every shipped locale parses, negotiates to itself, and serves a known message.
#[test]
fn shipped_locales_are_parseable_and_translatable() {
    for locale in SHIPPED_LOCALES {
        assert_eq!(negotiate(locale), *locale);
        let value = translator(locale).t("menu-file-new-tab");
        assert!(!value.is_empty());
        assert_ne!(value, "menu-file-new-tab");
    }
}

/// Shipped command messages must translate while preserving English aliases and concrete numeric arguments.
#[test]
fn command_catalogs_supply_localized_templates() {
    for (locale, new_tab, resize) in [
        ("en", "New Tab", "Resize Pane Left by 37"),
        ("zh-CN", "新建标签页", "向左调整窗格 37 步"),
        ("ja", "新しいタブ", "ペインを左に 37 ステップ調整"),
    ] {
        let i18n = translator(locale);
        assert_eq!(i18n.t("command-new-tab"), new_tab);
        assert_eq!(
            i18n.t_args("command-resize-pane", Some(&[("direction", "left"), ("amount", "37")])),
            resize
        );
        let literal = "my-theme {literal} 中文";
        assert!(i18n.t_args("command-apply-theme", Some(&[("name", literal)])).contains(literal));
    }
}

/// Every command identity has translated text while English labels and aliases remain searchable.
#[test]
fn command_catalogs_cover_existing_action_labels() {
    use crate::command_label::{descriptor, label, localized_label, localized_search_haystack};
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        for action in crate::command_palette::all_actions() {
            let entry = descriptor(&action);
            assert!(
                i18n.active_bundle.get_message(entry.label_key).is_some(),
                "{locale}: {}",
                entry.label_key
            );
            let localized = localized_label(&action, &i18n);
            assert!(!localized.is_empty());
            assert_ne!(localized, entry.label_key);
            let haystack = localized_search_haystack(&action, &i18n);
            assert!(haystack.contains(&label(&action)));
            assert!(haystack.contains(&localized));
            for alias in entry.aliases {
                assert!(haystack.contains(alias));
            }
            if *locale == "en" {
                assert_eq!(localized, label(&action));
            }
        }
    }
}

/// Missing action templates fall back wholly to English, even if the locale has translated enum words.
#[test]
fn command_labels_fall_back_to_existing_english_text() {
    use crate::command_label::{label, localized_label};
    let mut i18n = translator("ja");
    i18n.active_bundle = Bundle::new_concurrent(vec!["ja".parse().unwrap()]);
    i18n.active_bundle
        .add_resource(FluentResource::try_new("command-direction-left = 左\n".into()).unwrap())
        .unwrap();
    for action in crate::command_palette::all_actions() {
        assert_eq!(localized_label(&action, &i18n), label(&action));
    }
    i18n.fallback = Bundle::new_concurrent(vec!["en".parse().unwrap()]);
    for action in crate::command_palette::all_actions() {
        assert_eq!(localized_label(&action, &i18n), label(&action));
    }
    assert_eq!(i18n.t("unknown-contract-key"), "unknown-contract-key");
}

/// Localized templates preserve concrete command arguments and their original units.
#[test]
fn localized_command_parameters_keep_their_values() {
    use crate::command_label::localized_label;
    use sonicterm_cfg::keymap::{Action, Direction};
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        assert!(localized_label(&Action::ActivateTab(16), &i18n).contains("17"));
        assert!(localized_label(&Action::ResizePane { dir: Direction::Left, amount: 37 }, &i18n)
            .contains("37"));
        let theme = "my-theme {literal} 中文";
        assert!(localized_label(&Action::ApplyTheme(theme.into()), &i18n).contains(theme));
        let target = "example-user@example-host:2222";
        assert!(localized_label(&Action::OpenSshPane(target.into()), &i18n).contains(target));
    }
}

/// Every category has a translated label and the fixed semantic order survives localization.
#[test]
fn command_categories_have_stable_order_and_localized_labels() {
    use crate::command_label::CommandCategory;
    assert!(CommandCategory::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        for category in CommandCategory::ALL {
            let label = category.label(&i18n);
            assert!(!label.is_empty());
            assert!(!label.starts_with("command-category-"));
        }
    }
}

/// Palette rows and matching consume translated catalog text while English aliases remain usable.
#[test]
fn palette_localized_rows_and_queries_follow_the_active_catalog() {
    use crate::command_palette::CommandPalette;
    use crate::overlays::PaletteLayout;
    use sonicterm_cfg::keymap::{Action, Keymap};
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.open();
    palette.set_keymap(&Keymap::default(), &translator("en"));
    let canonical: Vec<_> = palette.visible().into_iter().cloned().collect();
    for (locale, query, expected) in [
        ("en", "New Tab", "New Tab"),
        ("zh-CN", "新建标签页", "新建标签页"),
        ("ja", "新しいタブ", "新しいタブ"),
    ] {
        palette.set_keymap(&Keymap::default(), &translator(locale));
        palette.set_query(query);
        assert_eq!(palette.current(), Some(&Command(Action::NewTab)), "{locale}");
        let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0).unwrap();
        assert_eq!(layout.row_labels.first().map(String::as_str), Some(expected));
        palette.set_query("create");
        assert!(palette.visible().contains(&&Command(Action::NewTab)));
        palette.set_query("cmd+q");
        assert!(palette.visible().contains(&&Command(Action::QuitApp)));
        palette.set_query("");
        let actions: Vec<_> = palette.visible().into_iter().cloned().collect();
        assert_eq!(actions, canonical);
    }
}

/// Refresh keeps command identity across changed fuzzy ranks without moving the query caret.
#[test]
fn palette_locale_refresh_preserves_selected_action_and_valid_scroll() {
    use crate::command_palette::CommandPalette;
    use sonicterm_cfg::keymap::Action;
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.open();
    palette.set_visible_rows(4);
    for _ in 0..12 {
        palette.move_selection_down();
    }
    let action = palette.current().cloned();
    let scroll = palette.scroll_offset();
    palette.set_locale(&translator("ja"));
    assert_eq!(palette.current(), action.as_ref());
    assert_eq!(palette.scroll_offset(), scroll);
    let mut translated = translator("ja");
    translated.active_bundle = Bundle::new_concurrent(vec!["ja".parse().unwrap()]);
    translated
        .active_bundle
        .add_resource(
            FluentResource::try_new(
                "command-new-window = 自定义条目\ncommand-new-tab = 自定义条目\ncommand-reload-config = 自定义条目\n".into(),
            )
            .unwrap(),
        )
        .unwrap();
    palette.set_locale(&translated);
    palette.set_query("自定义条目");
    palette.move_cursor_left();
    let cursor = palette.cursor();
    let window_index = palette
        .visible()
        .iter()
        .position(|action| **action == Command(Action::NewWindow))
        .expect("translated query finds the window action");
    for _ in 0..window_index {
        palette.move_selection_down();
    }
    translated.active_bundle = Bundle::new_concurrent(vec!["ja".parse().unwrap()]);
    translated
        .active_bundle
        .add_resource(
            FluentResource::try_new(
                "command-new-window = 自定义条目\ncommand-new-tab = other\ncommand-reload-config = 自定义条目\n".into(),
            )
            .unwrap(),
        )
        .unwrap();
    let previous = palette.selected();
    palette.set_locale(&translated);
    assert_eq!(palette.current(), Some(&Command(Action::NewWindow)));
    assert_ne!(
        palette.selected(),
        previous,
        "removing the preceding localized match changes the index"
    );
    assert_eq!(palette.query(), "自定义条目");
    assert_eq!(palette.cursor(), cursor);
    palette.set_query("新しいタブ");
    assert!(palette.is_empty());
    palette.set_locale(&translator("ja"));
    assert_eq!(palette.current(), Some(&Command(Action::NewTab)));
}

/// Keymap refresh owns labels and hints together, removing stale bound actions and preserving live selection.
#[test]
fn palette_locale_keymap_refresh_and_clone_keep_concrete_records_aligned() {
    use crate::command_palette::CommandPalette;
    use sonicterm_cfg::keymap::{Action, ActionWrapper, Binding, Keymap, Meta};
    let theme = Action::ApplyTheme("literal-theme {name}".into());
    let mut keymap = Keymap {
        meta: Meta { name: "localized".into(), version: "1.0".into() },
        bindings: vec![Binding { keys: "ctrl+alt+y".into(), action: ActionWrapper(theme.clone()) }],
    };
    let i18n = translator("zh-CN");
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.open();
    palette.set_keymap(&keymap, &i18n);
    palette.set_query("literal-theme");
    assert_eq!(palette.current(), Some(&Command(theme.clone())));
    assert_eq!(
        palette.label_for_visible_index(0),
        Some(crate::command_label::localized_label(&theme, &i18n).as_str())
    );
    let cloned = palette.clone();
    assert_eq!(cloned.current(), Some(&Command(theme.clone())));
    assert_eq!(cloned.presentation_hash(), palette.presentation_hash());
    keymap.bindings.insert(
        0,
        Binding {
            keys: "alt+x".into(),
            action: ActionWrapper(Action::ApplyTheme("other-theme".into())),
        },
    );
    palette.set_keymap(&keymap, &i18n);
    assert_eq!(palette.current(), Some(&Command(theme.clone())));
    let expected = crate::command_label::pretty_keys("ctrl+alt+y");
    palette.set_query(&expected);
    assert_eq!(palette.current(), Some(&Command(theme.clone())));
    assert_eq!(palette.shortcut_hint_for_visible_index(0), Some(expected.as_str()));
    palette.set_keymap(&Keymap::default(), &i18n);
    assert!(palette.is_empty());
    palette.set_query("literal-theme");
    assert!(palette.is_empty());
    assert_eq!(
        cloned.current(),
        Some(&Command(theme.clone())),
        "clone owns its presentation records"
    );
}

/// Noncommand rows keep their mode-specific indices while later Commands opens use refreshed locale and bindings.
#[test]
fn palette_locale_refresh_in_other_modes_preserves_rows_but_refreshes_commands() {
    use crate::command_palette::{CommandPalette, CommandPaletteMode, TabColorChoice};
    use sonicterm_cfg::keymap::{Action, ActionWrapper, Binding, Keymap, Meta};
    let action = Action::ApplyTheme("new-binding".into());
    let keymap = Keymap {
        meta: Meta { name: "mode-refresh".into(), version: "1.0".into() },
        bindings: vec![Binding {
            keys: "ctrl+alt+y".into(),
            action: ActionWrapper(action.clone()),
        }],
    };
    for mode in [
        CommandPaletteMode::RenameTab,
        CommandPaletteMode::RenameWindow,
        CommandPaletteMode::TabColor,
    ] {
        let mut palette = CommandPalette::new();
        palette.set_context(CommandContext {
            window_available: true,
            tab_count: 8,
            pane_available: true,
            ..CommandContext::default()
        });
        match mode {
            CommandPaletteMode::RenameTab => {
                palette.start_rename_tab("title 中文");
                palette.move_cursor_left();
            }
            CommandPaletteMode::TabColor => {
                palette.start_tab_color_picker(
                    "title 中文",
                    vec![
                        TabColorChoice { name: "reset".into(), hex: None },
                        TabColorChoice { name: "red".into(), hex: Some("#ff0000".into()) },
                        TabColorChoice { name: "blue".into(), hex: Some("#0000ff".into()) },
                    ],
                );
                palette.set_visible_rows(1);
                palette.move_selection_up();
            }
            CommandPaletteMode::RenameWindow => {
                palette.start_rename_window("title 中文");
                palette.move_cursor_left();
            }
            CommandPaletteMode::Commands => unreachable!(),
        }
        let before = (
            palette.query().to_string(),
            palette.cursor(),
            palette.selected(),
            palette.scroll_offset(),
            palette.len(),
            palette.selected_tab_color().cloned(),
        );
        palette.set_locale(&translator("ja"));
        palette.set_keymap(&keymap, &translator("ja"));
        assert_eq!(palette.mode(), mode);
        assert_eq!(
            (
                palette.query().to_string(),
                palette.cursor(),
                palette.selected(),
                palette.scroll_offset(),
                palette.len(),
                palette.selected_tab_color().cloned()
            ),
            before
        );
        assert!(palette.current().is_none());
        assert!(palette.visible().is_empty());
        if mode == CommandPaletteMode::TabColor {
            palette.move_selection_down();
            assert_eq!(palette.selected(), 0, "color navigation still wraps at the color count");
        }
        palette.open();
        palette.set_query("新しいタブ");
        assert_eq!(palette.current(), Some(&Command(Action::NewTab)));
        palette.set_query("new-binding");
        assert_eq!(palette.current(), Some(&Command(action.clone())));
        assert_eq!(palette.label_for_visible_index(0), Some("テーマを適用：new-binding"));
        palette.close();
        assert_eq!(palette.mode(), CommandPaletteMode::Commands);
        assert!(palette.visible().contains(&&Command(action.clone())));
    }
}

/// Cached display identity changes on locale or binding edits but stays stable for equivalent refreshes.
#[test]
fn palette_presentation_identity_tracks_cached_text_not_refresh_count() {
    use crate::command_palette::CommandPalette;
    use sonicterm_cfg::keymap::{Action, ActionWrapper, Binding, Keymap, Meta};
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.set_locale(&translator("en"));
    let english = palette.presentation_hash();
    palette.set_locale(&translator("en"));
    assert_eq!(palette.presentation_hash(), english);
    palette.set_locale(&translator("ja"));
    let japanese = palette.presentation_hash();
    assert_ne!(japanese, english);
    palette.set_locale(&translator("ja"));
    assert_eq!(palette.presentation_hash(), japanese);
    let keymap = Keymap {
        meta: Meta { name: "identity".into(), version: "1.0".into() },
        bindings: vec![Binding {
            keys: "ctrl+alt+y".into(),
            action: ActionWrapper(Action::ToggleTabBar),
        }],
    };
    palette.set_keymap(&keymap, &translator("ja"));
    let bound = palette.presentation_hash();
    assert_ne!(bound, japanese);
    palette.set_keymap(&keymap, &translator("ja"));
    assert_eq!(palette.presentation_hash(), bound);
}

/// Actual layout chrome uses the shipped locale while retaining literal query and tab-title text.
#[test]
fn palette_chrome_localizes_every_mode_without_changing_geometry() {
    use crate::command_palette::{CommandPalette, TabColorChoice};
    use crate::overlays::PaletteLayout;
    let mut baseline = None;
    for (
        locale,
        placeholder,
        none,
        hint,
        noun,
        navigation,
        rename,
        rename_hint,
        color,
        color_hint,
    ) in [
        (
            "en",
            "Search commands, settings, shortcuts…",
            "No commands found",
            "Try settings, split, font, shortcut",
            "commands",
            "↑↓ navigate · ↵ run · esc close",
            "New tab title…",
            "↵ rename · esc cancel",
            "Color for literal {name} 中文▏",
            "↑↓ choose color · ↵ apply · esc cancel",
        ),
        (
            "zh-CN",
            "搜索命令、设置、快捷键…",
            "未找到命令",
            "试试设置、分屏、字体、快捷键",
            "个命令",
            "↑↓ 导航 · ↵ 执行 · esc 关闭",
            "新标签页标题…",
            "↵ 重命名 · esc 取消",
            "literal {name} 中文 的颜色▏",
            "↑↓ 选择颜色 · ↵ 应用 · esc 取消",
        ),
        (
            "ja",
            "コマンド、設定、ショートカットを検索…",
            "コマンドが見つかりません",
            "設定、分割、フォント、ショートカットを試してください",
            "件のコマンド",
            "↑↓ 移動 · ↵ 実行 · esc 閉じる",
            "新しいタブ名…",
            "↵ 名前を変更 · esc キャンセル",
            "literal {name} 中文 の色▏",
            "↑↓ 色を選択 · ↵ 適用 · esc キャンセル",
        ),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_context(CommandContext {
            window_available: true,
            tab_count: 8,
            pane_available: true,
            ..CommandContext::default()
        });
        palette.set_locale(&translator(locale));
        palette.open();
        let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 3.0, 1.5).unwrap();
        assert_eq!(layout.query_placeholder.as_deref(), Some(placeholder), "{locale}");
        let spacing = if locale == "ja" { "" } else { " " };
        assert_eq!(layout.footer_label, format!("{}{spacing}{noun} · {navigation}", palette.len()));
        let geometry = (layout.border, layout.query_row, layout.footer);
        if let Some(expected) = baseline {
            assert_eq!(geometry, expected);
        } else {
            baseline = Some(geometry);
        }
        palette.set_query("zzzzzzzz");
        let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 3.0, 1.5).unwrap();
        assert_eq!(layout.empty_label.as_deref(), Some(none));
        assert_eq!(layout.empty_hint.as_deref(), Some(hint));
        assert_eq!(layout.footer_label, format!("0{spacing}{noun} · {navigation}"));
        palette.start_rename_tab("");
        let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 3.0, 1.5).unwrap();
        assert_eq!(layout.query_placeholder.as_deref(), Some(rename));
        assert_eq!(layout.footer_label, rename_hint);
        palette.set_query("literal {name} 中文");
        palette.move_cursor_left();
        let query = palette.query().to_string();
        let caret = palette.cursor();
        palette.set_locale(&translator(locale));
        assert_eq!(palette.query(), query);
        assert_eq!(palette.cursor(), caret);
        palette.start_tab_color_picker(
            "literal {name} 中文",
            vec![TabColorChoice { name: "user color".into(), hex: Some("#123456".into()) }],
        );
        let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 3.0, 1.5).unwrap();
        assert_eq!(layout.query_label, color);
        assert_eq!(layout.footer_label, color_hint);
        assert_eq!(layout.row_labels, ["user color — literal {name} 中文"]);
    }
}

/// Missing chrome translations remain English and the single-command footer keeps its original grammar.
#[test]
fn palette_chrome_falls_back_to_english_without_raw_keys() {
    use crate::command_palette::CommandPalette;
    use crate::overlays::PaletteLayout;
    let mut i18n = translator("ja");
    i18n.active_bundle = Bundle::new_concurrent(vec!["ja".parse().unwrap()]);
    i18n.fallback = Bundle::new_concurrent(vec!["en".parse().unwrap()]);
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.open();
    palette.set_locale(&i18n);
    let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0).unwrap();
    assert_eq!(layout.query_placeholder.as_deref(), Some("Search commands, settings, shortcuts…"));
    palette.set_query("cmd+q");
    let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0).unwrap();
    assert_eq!(layout.footer_label, "1 command · ↑↓ navigate · ↵ run · esc close");
}

/// Chrome-only catalog changes invalidate retained display identity even with unchanged action labels.
#[test]
fn palette_chrome_identity_includes_whole_phrase_ordering() {
    use crate::command_palette::{CommandPalette, TabColorChoice};
    use crate::overlays::PaletteLayout;
    let mut palette = CommandPalette::new();
    palette.set_context(CommandContext {
        window_available: true,
        tab_count: 8,
        pane_available: true,
        ..CommandContext::default()
    });
    palette.open();
    palette.set_query("cmd+q");
    let original = palette.presentation_hash();
    let mut i18n = translator("en");
    i18n.active_bundle = Bundle::new_concurrent(vec!["en".parse().unwrap()]);
    i18n.active_bundle.add_resource(FluentResource::try_new(
        "palette-command-footer = Results: { $count } end\npalette-color-title = [{ $title }] chosen\n".into()
    ).unwrap()).unwrap();
    palette.set_locale(&i18n);
    assert_ne!(palette.presentation_hash(), original);
    let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0).unwrap();
    assert_eq!(layout.footer_label, "Results: 1 end");
    palette.start_tab_color_picker(
        "literal {title}",
        vec![TabColorChoice { name: "one".into(), hex: None }],
    );
    let layout = PaletteLayout::compute(&mut palette, 1200.0, 800.0, 0.0, 1.0).unwrap();
    assert_eq!(layout.query_label, "[literal {title}] chosen▏");
}

/// Disabled reasons and categories share localized cached text while all commands remain discoverable.
#[test]
fn palette_details_localize_disabled_reasons_and_keep_row_alignment() {
    use crate::command_palette::CommandPalette;
    use crate::overlays::PaletteLayout;
    for (locale, expected) in [
        ("en", "Clipboard · No text selected"),
        ("zh-CN", "剪贴板 · 未选择文本"),
        ("ja", "クリップボード · テキストが選択されていません"),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_context(CommandContext {
            window_available: true,
            tab_count: 1,
            pane_available: true,
            ..CommandContext::default()
        });
        palette.set_locale(&translator(locale));
        palette.open();
        palette.set_query("Copy to Clipboard");
        let layout = PaletteLayout::compute(&mut palette, 1400.0, 1000.0, 0.0, 1.5).unwrap();
        assert_eq!(layout.row_details.first().and_then(|text| text.as_deref()), Some(expected));
        assert_eq!(layout.row_disabled.first(), Some(&true));
        assert_eq!(layout.row_details.len(), layout.rows.len());
        assert_eq!(layout.row_disabled.len(), layout.rows.len());
        assert!(palette.current().is_none());
    }
}

/// Live tab labels localize around literal titles and keep English target queries usable.
#[test]
fn go_to_tab_labels_preserve_titles_and_english_queries() {
    use crate::command_palette::{CommandPalette, PaletteEntry};
    use crate::tabs::{Tab, TabBar};
    let mut tabs = TabBar::new();
    let title = "literal {title} 中文";
    let id = tabs.push(Tab::new(title));
    for (locale, expected) in [
        ("en", "Go to Tab 1: literal {title} 中文"),
        ("zh-CN", "切换到标签页 1：literal {title} 中文"),
        ("ja", "タブ 1 に移動：literal {title} 中文"),
    ] {
        let mut palette = CommandPalette::new();
        palette.set_context(CommandContext {
            window_available: true,
            tab_count: 1,
            ..CommandContext::default()
        });
        let i18n = translator(locale);
        palette.set_locale(&i18n);
        palette.set_tabs(&tabs, &i18n);
        palette.open();
        palette.set_query(expected);
        assert!(
            matches!(palette.current(), Some(PaletteEntry::Tab { id: selected, .. }) if *selected == id)
        );
        assert_eq!(palette.label_for_visible_index(0), Some(expected));
        palette.set_query("Go to Tab");
        assert!(
            matches!(palette.current(), Some(PaletteEntry::Tab { id: selected, .. }) if *selected == id)
        );
        assert_eq!(palette.shortcut_hint_for_visible_index(0), None);
    }
}

/// Invalid locale tags fall back to English rather than leaking an unsupported tag.
#[test]
fn invalid_locale_negotiates_to_english() {
    assert_eq!(negotiate("not a locale"), "en");
}

/// Missing message ids remain visible as their key after active and English lookup fail.
#[test]
fn missing_message_returns_its_key() {
    assert_eq!(translator("ja").t("missing-contract-key"), "missing-contract-key");
}

/// Reload replaces future translations without retaining the previous bundle.
#[test]
fn reload_switches_the_active_locale() {
    let expected = pick_locale(Some("ja"));
    let expected_label = translator(&expected).t("menu-file-new-tab");
    let mut i18n = translator("en");

    i18n.reload_locale(Some("ja"));

    assert_eq!(i18n.locale(), expected);
    assert_eq!(i18n.t("menu-file-new-tab"), expected_label);
}
