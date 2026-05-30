//! Integration tests for the command palette state.

use sonic_core::keymap::{Action, Direction, ScrollAction};
use sonic_shared::command_palette::{action_display_name, all_actions, CommandPalette};

#[test]
fn starts_closed_and_empty_query() {
    let p = CommandPalette::new();
    assert!(!p.is_open());
    assert_eq!(p.query(), "");
    // Even when closed the full universe is available — refilter on open
    // is the canonical reset.
    assert!(!p.is_empty());
}

#[test]
fn open_clears_query_and_resets_selection() {
    let mut p = CommandPalette::new();
    p.set_query("zzznevermatches");
    assert!(p.is_empty());
    p.open();
    assert!(p.is_open());
    assert_eq!(p.query(), "");
    assert_eq!(p.selected(), 0);
    assert_eq!(p.len(), all_actions().len());
}

#[test]
fn set_query_ne_filters_to_actions_containing_subsequence_ne() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    let visible: Vec<String> = p.visible().iter().map(|a| action_display_name(a)).collect();
    // NewTab, NewWindow, NextTab — every action whose lowercased name
    // contains the subsequence "ne".
    assert!(visible.iter().any(|n| n == "NewTab"), "visible = {visible:?}");
    assert!(visible.iter().any(|n| n == "NextTab"), "visible = {visible:?}");
    assert!(visible.iter().any(|n| n == "NewWindow"), "visible = {visible:?}");
    // Sanity: a totally unrelated action should be filtered out.
    assert!(!visible.iter().any(|n| n == "CopyToClipboard"));
}

#[test]
fn input_char_appends_and_refilters_incrementally() {
    let mut p = CommandPalette::new();
    p.open();
    p.input_char('n');
    p.input_char('e');
    p.input_char('w');
    assert_eq!(p.query(), "new");
    let visible: Vec<String> = p.visible().iter().map(|a| action_display_name(a)).collect();
    assert!(visible.iter().any(|n| n == "NewTab"));
    assert!(visible.iter().any(|n| n == "NewWindow"));
    // "CopyToClipboard" does not contain n,e,w as a subsequence.
    assert!(!visible.iter().any(|n| n == "CopyToClipboard"));
}

#[test]
fn backspace_widens_filter() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("newt");
    let narrow = p.len();
    p.backspace();
    assert_eq!(p.query(), "new");
    assert!(p.len() >= narrow);
}

#[test]
fn enter_returns_currently_selected_action() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    // "current" gives us what Enter would dispatch.
    let current = p.current().cloned().expect("at least one match for 'ne'");
    // Must be one of the actions whose display name contains "ne" as a
    // subsequence — specifically the first hit in canonical order.
    let name = action_display_name(&current);
    assert!(name.to_lowercase().contains('n'));
    assert!(matches!(current, Action::NewTab | Action::NewWindow | Action::NextTab));
}

#[test]
fn esc_closes_and_clears() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("ne");
    p.close();
    assert!(!p.is_open());
    assert_eq!(p.query(), "");
    assert_eq!(p.selected(), 0);
}

#[test]
fn move_selection_wraps_around_bounds() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query(""); // full list
    let n = p.len();
    assert!(n >= 3);
    assert_eq!(p.selected(), 0);
    p.move_selection_up(); // wraps to last
    assert_eq!(p.selected(), n - 1);
    p.move_selection_down(); // wraps back to first
    assert_eq!(p.selected(), 0);
    for _ in 0..n {
        p.move_selection_down();
    }
    assert_eq!(p.selected(), 0, "full loop returns to start");
}

#[test]
fn move_selection_on_empty_is_noop() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("zzzzznevermatchesanything");
    assert!(p.is_empty());
    p.move_selection_down();
    p.move_selection_up();
    assert_eq!(p.selected(), 0);
    assert!(p.current().is_none());
}

#[test]
fn toggle_flips_open_state() {
    let mut p = CommandPalette::new();
    assert!(!p.is_open());
    assert!(p.toggle());
    assert!(p.is_open());
    assert!(!p.toggle());
    assert!(!p.is_open());
}

#[test]
fn all_actions_covers_every_variant_kind() {
    // Spot check: every coarse-grained Action category appears at least
    // once in the palette universe.
    let all = all_actions();
    assert!(all.contains(&Action::NewTab));
    assert!(all.contains(&Action::OpenCommandPalette));
    assert!(all.contains(&Action::OpenSearch));
    assert!(all.contains(&Action::EditConfigFile));
    assert!(all.contains(&Action::Scroll(ScrollAction::ToTop)));
    assert!(all.contains(&Action::FocusPane(Direction::Left)));
}

// ---------------------------------------------------------------------------
// Full-coverage + VSCode-style fuzzy search tests (see PR feat(palette)).

#[test]
fn palette_lists_every_action_variant() {
    // Every variant kind in sonic_core::keymap::Action must be
    // represented at least once in the palette universe — otherwise
    // a brand-new bindable action would silently never appear.
    use sonic_shared::command_label::{variant_kind, ALL_VARIANT_KINDS};
    use sonic_shared::command_palette::covers_every_variant_kind;
    assert!(covers_every_variant_kind(), "all_actions() is missing a variant kind");
    let universe = all_actions();
    for kind in ALL_VARIANT_KINDS {
        assert!(
            universe.iter().any(|a| variant_kind(a) == *kind),
            "variant kind {kind} is not in palette universe"
        );
    }
    // And the universe size is at least the kind count (Direction-
    // parameterized kinds appear multiple times, so >= not ==).
    assert!(universe.len() >= ALL_VARIANT_KINDS.len());
}

#[test]
fn fuzzy_match_ranks_substring_before_subsequence() {
    // Typing "neta" should match "New Tab" (a subsequence: N-e-T-a)
    // and rank it ahead of any candidate where the chars only barely
    // appear. "Edit sonic.toml" has no 'n' followed by 'e' followed
    // by 't' followed by 'a', so it must NOT match at all.
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("neta");
    let visible_actions = p.visible();
    let labels: Vec<String> =
        visible_actions.iter().map(|a| sonic_shared::command_label::label(a)).collect();
    assert!(
        labels.iter().any(|l| l == "New Tab"),
        "'neta' should match 'New Tab' as a subsequence: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Edit sonic.toml"),
        "'neta' should NOT match 'Edit sonic.toml': {labels:?}"
    );

    // And against a query that exists as a contiguous substring in
    // one label, the contiguous match must outrank a merely-subsequence
    // hit. "new t" vs the candidates: "New Tab" has it contiguous;
    // a hypothetical scatter match like "Next Tab" has it scattered.
    p.set_query("new t");
    let top = p.current().cloned().expect("at least one match");
    assert_eq!(
        sonic_shared::command_label::label(&top),
        "New Tab",
        "contiguous substring should rank first"
    );
}

#[test]
fn enter_on_selected_dispatches_action() {
    // The palette state exposes the currently-selected Action via
    // `current()`. The app's enter handler reads that and forwards
    // to App::run_action. We assert the contract that current()
    // returns the action that Enter would dispatch.
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("open command palette");
    let dispatched = p.current().cloned().expect("at least one match");
    assert!(matches!(dispatched, Action::OpenCommandPalette));

    // EditConfigFile is reachable by alias even though no keybinding
    // is set for it in the default keymap.
    p.set_query("preferences");
    let dispatched = p.current().cloned().expect("at least one match");
    assert!(matches!(dispatched, Action::EditConfigFile));

    // ReloadConfig is also reachable.
    p.set_query("reload");
    let dispatched = p.current().cloned().expect("at least one match");
    assert!(matches!(dispatched, Action::ReloadConfig));
}

#[test]
fn esc_closes_palette() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("split");
    assert!(p.is_open());
    assert!(!p.is_empty());
    // The Esc key handler in app.rs calls .close().
    p.close();
    assert!(!p.is_open(), "palette must close on Esc");
    assert_eq!(p.query(), "", "query is cleared so reopening is fresh");
    assert_eq!(p.selected(), 0, "selection resets to top");
}

#[test]
fn keybinding_hint_uses_pretty_glyphs_when_bound() {
    use sonic_core::keymap::{ActionWrapper, Binding, Keymap, Meta};
    use sonic_shared::command_label::keybinding_hint;
    let km = Keymap {
        meta: Meta { name: "test".into(), version: "1".into() },
        bindings: vec![
            Binding { keys: "super+t".into(), action: ActionWrapper(Action::NewTab) },
            Binding {
                keys: "super+shift+p".into(),
                action: ActionWrapper(Action::OpenCommandPalette),
            },
        ],
    };
    assert_eq!(keybinding_hint(&km, &Action::NewTab).as_deref(), Some("⌘T"));
    assert_eq!(keybinding_hint(&km, &Action::OpenCommandPalette).as_deref(), Some("⌘⇧P"));
    // Unbound action returns None.
    assert!(keybinding_hint(&km, &Action::ReloadConfig).is_none());
}

// ---------------------------------------------------------------------------
// Scroll-to-selection + alias-keyword + empty-results placeholder tests.

#[test]
fn palette_scrolls_to_keep_selection_in_viewport() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_visible_rows(5);
    // Walk past the visible window — selection should always be inside it.
    for _ in 0..12 {
        p.move_selection_down();
        let off = p.scroll_offset();
        let sel = p.selected();
        let rows = p.visible_rows();
        assert!(sel >= off && sel < off + rows, "sel={sel} not in [{off}, {})", off + rows);
    }
}

#[test]
fn palette_selecting_below_fold_scrolls_down() {
    let mut p = CommandPalette::new();
    p.open();
    p.set_visible_rows(4);
    assert_eq!(p.scroll_offset(), 0);
    // Move past the fold: down 4 times lands selected=4, which forces
    // the viewport to start at index 1 so the selection stays visible.
    for _ in 0..4 {
        p.move_selection_down();
    }
    assert_eq!(p.selected(), 4);
    assert_eq!(p.scroll_offset(), 1, "viewport must scroll down to keep selection in view");
    // Going back up to the top must scroll the viewport back too.
    for _ in 0..4 {
        p.move_selection_up();
    }
    assert_eq!(p.selected(), 0);
    assert_eq!(p.scroll_offset(), 0);
}

#[test]
fn palette_arrow_up_from_first_wraps_to_last() {
    // The current behavior is wrap-around (see move_selection_wraps_around_bounds);
    // confirm scroll_offset follows the wrap so the (now last) row is visible.
    let mut p = CommandPalette::new();
    p.open();
    p.set_visible_rows(5);
    assert_eq!(p.selected(), 0);
    p.move_selection_up(); // wraps to last
    let n = p.len();
    assert_eq!(p.selected(), n - 1);
    let off = p.scroll_offset();
    assert!(p.selected() >= off && p.selected() < off + p.visible_rows());
}

#[test]
fn palette_query_sett_matches_edit_config() {
    // Typing "sett" must surface the config editor via the keyword alias path.
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("sett");
    let labels: Vec<String> =
        p.visible().iter().map(|a| sonic_shared::command_label::label(a)).collect();
    assert!(
        labels.iter().any(|l| l == "Edit sonic.toml"),
        "'sett' should match 'Edit sonic.toml' via keyword alias: {labels:?}"
    );
}

#[test]
fn palette_zero_matches_shows_no_commands_placeholder() {
    use sonic_shared::overlays::{PaletteLayout, NO_MATCHES};
    let mut p = CommandPalette::new();
    p.open();
    p.set_query("zzz_definitely_no_match_zzz");
    assert!(p.is_empty(), "filter must produce zero matches");
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    assert!(layout.rows.is_empty(), "no rows must be drawn");
    assert_eq!(
        layout.empty_label.as_deref(),
        Some(NO_MATCHES),
        "empty placeholder must be present"
    );
    // Empty query alone should NOT show the placeholder.
    p.set_query("");
    let layout = PaletteLayout::compute(&mut p, 1200.0, 800.0).expect("open");
    assert!(layout.empty_label.is_none(), "empty query is not the same as zero matches");
}
