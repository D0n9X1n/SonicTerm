use super::*;

#[test]
fn app_accepts_one_process_privilege_snapshot() {
    // Protect every window from deriving privilege independently after startup.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());

    app.set_process_privilege(crate::ProcessPrivilege::Privileged);

    assert_eq!(app.process_privilege(), crate::ProcessPrivilege::Privileged);
}

#[test]
fn active_tab_foreground_privilege_updates_without_changing_its_title() {
    // Protect token-only elevation changes from coupling the warning to editable title text.
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    let mut pane = PaneState::new(parser.clone(), None);
    let mut tabs = sonicterm_ui::tabs::TabBar::new();
    tabs.push(sonicterm_ui::tabs::Tab::new("#1 shell"));
    let now = Instant::now();
    pane.fg_proc_cache = Some((
        now,
        Some(sonicterm_io::proc_info::ForegroundProcess {
            name: "pwsh".to_string(),
            privileged: true,
        }),
    ));

    refresh_active_tab_title(&mut tabs, &mut pane, &parser.lock(), 0, false);
    let privileged_title = tabs.active().expect("tab").title.clone();
    assert!(tabs.active().expect("tab").foreground_privileged);

    pane.fg_proc_cache = Some((
        now,
        Some(sonicterm_io::proc_info::ForegroundProcess {
            name: "pwsh".to_string(),
            privileged: false,
        }),
    ));
    refresh_active_tab_title(&mut tabs, &mut pane, &parser.lock(), 0, false);

    assert!(!tabs.active().expect("tab").foreground_privileged);
    assert_eq!(tabs.active().expect("tab").title, privileged_title);
    assert!(!tabs.active().expect("tab").title.contains("lock"));
}

#[test]
fn gsudo_foreground_state_clears_after_the_regular_shell_returns() {
    // Protect a regular SonicTerm tab from retaining the warning after gsudo exits.
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    let mut pane = PaneState::new(parser.clone(), None);
    let mut tabs = sonicterm_ui::tabs::TabBar::new();
    tabs.push(sonicterm_ui::tabs::Tab::new("#1 shell"));
    let now = Instant::now();
    pane.fg_proc_cache = Some((
        now,
        Some(sonicterm_io::proc_info::ForegroundProcess {
            name: "gsudo".to_string(),
            privileged: true,
        }),
    ));
    refresh_active_tab_title(&mut tabs, &mut pane, &parser.lock(), 0, false);
    assert!(tabs.active().expect("tab").foreground_privileged);

    pane.fg_proc_cache = Some((
        now,
        Some(sonicterm_io::proc_info::ForegroundProcess {
            name: "pwsh".to_string(),
            privileged: false,
        }),
    ));
    refresh_active_tab_title(&mut tabs, &mut pane, &parser.lock(), 0, false);

    assert!(!tabs.active().expect("tab").foreground_privileged);
}

#[cfg(windows)]
#[test]
fn inactive_tab_foreground_privilege_refreshes_without_moving_focus_or_changing_titles() {
    // Protect visible background tabs from retaining a stale gsudo warning until activation.
    let now = Instant::now();
    let process = |name: &str, privileged| {
        Some(sonicterm_io::proc_info::ForegroundProcess { name: name.to_string(), privileged })
    };
    let mut regular = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);
    regular.fg_proc_cache = Some((now, process("pwsh", false)));
    let mut background = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);
    background.fg_proc_cache = Some((now, process("gsudo", true)));
    let mut panes = HashMap::from([(10, regular), (20, background)]);
    let tab_states =
        vec![TabState::new(PaneTree::leaf(10), 10), TabState::new(PaneTree::leaf(20), 20)];
    let mut tabs = sonicterm_ui::tabs::TabBar::new();
    let active = tabs.push(sonicterm_ui::tabs::Tab::new("#1 regular"));
    tabs.push(sonicterm_ui::tabs::Tab::new("#2 background"));
    tabs.activate(0);
    let titles = tabs.tabs().iter().map(|tab| tab.title.clone()).collect::<Vec<_>>();

    refresh_window_tab_privileges(&mut tabs, &tab_states, &mut panes, false);
    assert!(!tabs.tabs()[0].foreground_privileged);
    assert!(tabs.tabs()[1].foreground_privileged);

    panes.get_mut(&20).expect("background pane").fg_proc_cache =
        Some((now, process("pwsh", false)));
    refresh_window_tab_privileges(&mut tabs, &tab_states, &mut panes, false);

    assert!(!tabs.tabs()[1].foreground_privileged);
    assert_eq!(tabs.active().map(|tab| tab.id), Some(active));
    assert_eq!(tabs.tabs().iter().map(|tab| tab.title.clone()).collect::<Vec<_>>(), titles);
}

#[test]
fn main_and_child_render_paths_refresh_visible_tab_privilege_state() {
    // Protect the indexed privilege refresh from being omitted by either window render path.
    for (name, source) in
        [("main", include_str!("window_event.rs")), ("child", include_str!("child_window.rs"))]
    {
        assert!(
            source.contains("refresh_window_tab_privileges("),
            "{name} must refresh privilege state for every visible tab"
        );
    }
}

#[test]
fn main_and_child_render_paths_forward_the_same_process_privilege_snapshot() {
    // Protect torn-out windows from omitting or independently recomputing the process warning.
    for (name, source, tabs) in [
        ("main", include_str!("window_event.rs"), "tabs_mref"),
        ("child", include_str!("child_window.rs"), "&child.tabs"),
    ] {
        assert!(
            source.contains("let process_privileged = self.process_privilege.is_privileged();"),
            "{name} must snapshot App process privilege before borrowing window state"
        );
        let render = source.find(".render(").expect("render call");
        let call =
            &source[render..source[render..].find(") {").map_or(source.len(), |end| render + end)];
        let tabs =
            call.find(tabs).unwrap_or_else(|| panic!("{name} render call must pass its tabs"));
        let privilege = call
            .find("process_privileged")
            .unwrap_or_else(|| panic!("{name} render call must pass process privilege"));
        let search = call
            .find("search")
            .unwrap_or_else(|| panic!("{name} render call must retain search state"));
        assert!(tabs < privilege && privilege < search, "{name} render argument order drifted");
    }
}
