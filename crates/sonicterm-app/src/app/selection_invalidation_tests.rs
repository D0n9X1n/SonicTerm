use super::*;

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_grid::grid::{CellFlags, Color};

fn app_with_main_and_child() -> (App, u64, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.__test_child_active_pane(child).expect("seeded child pane");
    (app, main_pane, child, child_pane)
}

fn install_alt_selection(app: &mut App, window: WindowId, pane_id: u64, row: u64) {
    let (seq, is_alt) = {
        let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
        let mut parser = pane.parser.lock();
        let grid = parser.grid_mut();
        grid.enter_alt_screen();
        (grid.content_seq(), grid.is_alt())
    };
    app.windows.get_mut(&window).unwrap().selection = Some(Selection {
        start: (row, 0),
        end: (row, 3),
        anchored: true,
        pane_id: Some(pane_id),
        content_seq: seq,
        on_alt_screen: is_alt,
        scrollback_evicted: 0,
    });
}

fn install_primary_selection(app: &mut App, window: WindowId, pane_id: u64, row: u64) {
    let (seq, is_alt, evicted) = {
        let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
        let parser = pane.parser.lock();
        let grid = parser.grid();
        (grid.content_seq(), grid.is_alt(), grid.scrollback_evicted())
    };
    assert!(!is_alt, "primary selection fixture must stay on the primary screen");
    app.windows.get_mut(&window).unwrap().selection = Some(Selection {
        start: (row, 0),
        end: (row, 3),
        anchored: true,
        pane_id: Some(pane_id),
        content_seq: seq,
        on_alt_screen: is_alt,
        scrollback_evicted: evicted,
    });
}

fn write_row(app: &App, window: WindowId, pane_id: u64, row: u16, ch: char) {
    let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
    let mut parser = pane.parser.lock();
    let grid = parser.grid_mut();
    grid.goto(row, 0);
    grid.put_char(ch, Color::Default, Color::Default, CellFlags::empty());
}

fn clear_pane_dirty(app: &App, window: WindowId, pane_id: u64) {
    let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
    pane.parser.lock().grid_mut().clear_dirty();
}

fn pane_dirty_count(app: &App, window: WindowId, pane_id: u64) -> usize {
    let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
    pane.parser.lock().grid().dirty_count()
}

fn run_invalidation(app: &mut App, window: WindowId, pane_id: u64) -> bool {
    let window = app.windows.get_mut(&window).unwrap();
    let pane = window.panes.get(&pane_id).unwrap();
    let parser = pane.parser.lock();
    invalidate_selection_for_content(&mut window.selection, pane_id, parser.grid())
}

#[test]
fn main_and_child_clear_when_selected_alt_content_changes() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (window, pane) in [(main, main_pane), (child, child_pane)] {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 4, 'x');

        assert!(run_invalidation(&mut app, window, pane));
        assert!(
            app.windows.get(&window).unwrap().selection.is_none(),
            "{window:?} must not leave stale selected text copyable"
        );
    }
}

#[test]
fn main_and_child_preserve_selection_when_alt_wheel_changes_no_rows() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (window, pane) in [(main, main_pane), (child, child_pane)] {
        install_alt_selection(&mut app, window, pane, 4);

        // Models a wheel event ignored at a TUI scroll boundary: no grid row
        // changed after the selection baseline, so the selection must survive.
        assert!(!run_invalidation(&mut app, window, pane));
        assert!(app.windows.get(&window).unwrap().selection.is_some());
    }
}

#[test]
fn main_and_child_preserve_selection_for_unrelated_alt_updates() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (window, pane) in [(main, main_pane), (child, child_pane)] {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 8, 'x');

        assert!(!run_invalidation(&mut app, window, pane));
        assert!(
            app.windows.get(&window).unwrap().selection.is_some(),
            "{window:?} must keep a selection while an unrelated TUI row updates"
        );
    }
}

#[test]
fn valid_alt_copy_clears_main_and_child_selection_after_writing_exact_text() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    // A successful explicit copy is the commit point for an alternate-screen
    // selection: exact text reaches the clipboard before its highlight clears.
    for (kind, window, pane) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 4, 's');
        install_alt_selection(&mut app, window, pane, 4);
        clear_pane_dirty(&app, window, pane);
        app.__test_set_memory_clipboard("unchanged");

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("s"));
        assert!(app.windows.get(&window).unwrap().selection.is_none());
        assert!(
            pane_dirty_count(&app, window, pane) > 0,
            "{window:?} must repaint cleared highlight"
        );
    }
}

#[test]
fn clipboard_failure_preserves_valid_alt_selection_clipboard_and_clean_rows() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");
    app.__test_set_memory_clipboard("original");
    app.__test_set_clipboard_write_failure(true);

    // A rejected clipboard write is not completion: the valid volatile range and
    // existing clipboard survive, and no repaint is scheduled just to clear it.
    for (kind, window, pane) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 4, 'f');
        install_alt_selection(&mut app, window, pane, 4);
        clear_pane_dirty(&app, window, pane);

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("original"));
        assert!(app.windows.get(&window).unwrap().selection.is_some());
        assert_eq!(pane_dirty_count(&app, window, pane), 0);
    }
}

#[test]
fn stale_alt_copy_clears_before_write_and_preserves_clipboard() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");
    app.__test_set_memory_clipboard("unchanged");

    // Stale alternate-screen rows are rejected before the clipboard boundary,
    // so replacement terminal output can never overwrite the prior clipboard.
    for (kind, window, pane) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 4, 'x');

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("unchanged"));
        assert!(app.windows.get(&window).unwrap().selection.is_none());
    }
}

#[test]
fn unrelated_alt_update_still_copies_then_clears_selection() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    // Row-level validation ignores unrelated TUI updates, but a later successful
    // explicit copy still consumes the valid alternate-screen selection.
    for (kind, window, pane) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 4, 's');
        install_alt_selection(&mut app, window, pane, 4);
        write_row(&app, window, pane, 8, 'x');
        clear_pane_dirty(&app, window, pane);
        app.__test_set_memory_clipboard("unchanged");

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("s"));
        assert!(app.windows.get(&window).unwrap().selection.is_none());
        assert!(
            pane_dirty_count(&app, window, pane) > 0,
            "{window:?} must repaint cleared highlight"
        );
    }
}

#[test]
fn successful_primary_copy_keeps_main_and_child_selection() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    // Primary-screen selections remain reusable after an explicit successful
    // copy; only alternate-screen volatility changes the completion policy.
    for (kind, window, pane) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        write_row(&app, window, pane, 4, 'p');
        install_primary_selection(&mut app, window, pane, 4);
        clear_pane_dirty(&app, window, pane);
        app.__test_set_memory_clipboard("unchanged");

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("p"));
        assert!(app.windows.get(&window).unwrap().selection.is_some());
        assert_eq!(pane_dirty_count(&app, window, pane), 0);
    }
}

#[test]
fn main_and_child_rebase_primary_selection_after_history_eviction() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (window, pane_id) in [(main, main_pane), (child, child_pane)] {
        {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let mut parser = pane.parser.lock();
            let grid = parser.grid_mut();
            grid.set_scrollback_limit(1);
            grid.goto(0, 0);
            grid.put_char('A', Color::Default, Color::Default, CellFlags::empty());
            grid.goto(1, 0);
            grid.put_char('B', Color::Default, Color::Default, CellFlags::empty());
            grid.scroll_up(1);
        }
        let (seq, evicted) = {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let parser = pane.parser.lock();
            (parser.grid().content_seq(), parser.grid().scrollback_evicted())
        };
        app.windows.get_mut(&window).unwrap().selection = Some(Selection {
            start: (1, 0),
            end: (1, 0),
            anchored: true,
            pane_id: Some(pane_id),
            content_seq: seq,
            on_alt_screen: false,
            scrollback_evicted: evicted,
        });
        {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let mut parser = pane.parser.lock();
            let grid = parser.grid_mut();
            grid.goto(1, 0);
            grid.put_char('C', Color::Default, Color::Default, CellFlags::empty());
            grid.scroll_up(1);
        }

        assert!(!run_invalidation(&mut app, window, pane_id));
        let selection = app.windows.get(&window).unwrap().selection.unwrap();
        assert_eq!((selection.start.0, selection.end.0), (0, 0));
    }
}

#[test]
fn copy_before_redraw_uses_rebased_primary_text() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (kind, window, pane_id) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let mut parser = pane.parser.lock();
            let grid = parser.grid_mut();
            grid.set_scrollback_limit(1);
            grid.goto(0, 0);
            grid.put_char('A', Color::Default, Color::Default, CellFlags::empty());
            grid.goto(1, 0);
            grid.put_char('B', Color::Default, Color::Default, CellFlags::empty());
            grid.scroll_up(1);
        }
        let (seq, evicted) = {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let parser = pane.parser.lock();
            (parser.grid().content_seq(), parser.grid().scrollback_evicted())
        };
        app.windows.get_mut(&window).unwrap().selection = Some(Selection {
            start: (1, 0),
            end: (1, 0),
            anchored: true,
            pane_id: Some(pane_id),
            content_seq: seq,
            on_alt_screen: false,
            scrollback_evicted: evicted,
        });
        {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let mut parser = pane.parser.lock();
            let grid = parser.grid_mut();
            grid.goto(1, 0);
            grid.put_char('C', Color::Default, Color::Default, CellFlags::empty());
            grid.scroll_up(1);
        }
        app.__test_set_memory_clipboard("unchanged");

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("B"));
    }
}

#[test]
fn copy_before_redraw_rejects_a_selected_primary_row_rewritten_then_scrolled() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let (mut app, main_pane, child, child_pane) = app_with_main_and_child();
    let main = app.__test_main_window_id().expect("synthetic main window");

    for (kind, window, pane_id) in
        [(FrontmostKind::Main, main, main_pane), (FrontmostKind::Child(child), child, child_pane)]
    {
        write_row(&app, window, pane_id, 0, 'A');
        let (seq, evicted) = {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            let parser = pane.parser.lock();
            (parser.grid().content_seq(), parser.grid().scrollback_evicted())
        };
        app.windows.get_mut(&window).unwrap().selection = Some(Selection {
            start: (0, 0),
            end: (0, 0),
            anchored: true,
            pane_id: Some(pane_id),
            content_seq: seq,
            on_alt_screen: false,
            scrollback_evicted: evicted,
        });
        write_row(&app, window, pane_id, 0, 'X');
        {
            let pane = app.windows.get(&window).unwrap().panes.get(&pane_id).unwrap();
            pane.parser.lock().grid_mut().scroll_up(1);
        }
        app.__test_set_memory_clipboard("unchanged");

        app.copy_selection_for_kind(kind);

        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("unchanged"));
        assert!(app.windows.get(&window).unwrap().selection.is_none());
    }
}

#[test]
fn both_redraw_paths_invalidate_before_rendering() {
    for (name, source, selection_arg) in [
        ("main", include_str!("window_event.rs"), "&mut ws.selection"),
        ("child", include_str!("child_window.rs"), "&mut child.selection"),
    ] {
        let call = source
            .find("invalidate_selection_for_content(")
            .unwrap_or_else(|| panic!("{name} redraw must call the shared invalidation helper"));
        let render = source[call..]
            .find(".render(")
            .map(|offset| call + offset)
            .unwrap_or_else(|| panic!("{name} redraw must render after invalidation"));
        assert!(source[call..render].contains(selection_arg));
        assert!(call < render, "{name} must clear stale selection before the renderer borrows it");
    }
}
