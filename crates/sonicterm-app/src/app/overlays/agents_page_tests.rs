//! Pins the Agents page in the App: its rows match the live-session snapshot joined with pane
//! ownership, navigation and filtering never disconnect, Enter opens the confirm page with Cancel
//! highlighted and Cancel changes nothing, Apply ends exactly the chosen agent, a session that
//! already left is a no-op, Disconnect all ends every live session, the page also opens with the
//! server off, and it attaches to the child window it was opened from.

use std::sync::Arc;

use sonicterm_cfg::config::{Config, McpConfig, McpControl, McpRead};
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::{AgentRowKind as Kind, CommandPaletteMode, ConfirmRow};
use winit::keyboard::{Key, NamedKey};

use crate::app::mcp::agents::PaneOwner;
use crate::app::mcp::{McpServer, McpShared};
use crate::app::App;

/// An App with one user pane and a running server at `control = "agent"` and `read = "agent"`.
fn app_with_server() -> (App, Arc<McpShared>, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let user = app.__test_seed_tab("user");
    app.config.mcp =
        McpConfig { enabled: true, read: Some(McpRead::Agent), ..McpConfig::default() };
    let shared = McpShared::new(McpControl::Agent, None);
    // The header shows the scope the server enforces, so the fixture sets one that is not the
    // shared state's initial value.
    shared.set_read(McpRead::Agent);
    let owner = app.register_mcp_owner();
    app.services.mcp_server = Some(McpServer {
        shared: Arc::clone(&shared),
        endpoint: "/tmp/sonicterm-test.sock".into(),
        _endpoint_guard: None,
        config: app.config.mcp,
        charges: std::collections::HashMap::new(),
        owner,
        read_notice_due: false,
    });
    (app, shared, user)
}

/// Press `key` on the open palette.
fn press(app: &mut App, key: NamedKey) {
    assert!(app.command_palette_handle_logical_key(&Key::Named(key)));
}

/// The kinds of the Agents page rows, in display order.
fn kinds(app: &App) -> Vec<Kind> {
    app.interaction.command_palette.agent_rows().iter().map(|row| row.kind).collect()
}

/// The main window's notification text, if one shows.
fn shown(app: &App) -> Option<String> {
    app.main().and_then(|main| main.notification.as_ref()).map(|bubble| bubble.message.clone())
}

/// The rows follow the snapshot: the endpoint line and the scope line, one row per live session
/// newest first with its name, number, calls and the panes it owns, then Disconnect all.
#[test]
fn the_rows_match_the_snapshot() {
    let (mut app, shared, user) = app_with_server();
    let older = shared.sessions.open();
    let newer = shared.sessions.open();
    shared.sessions.set_client(older, Some("claude".into()));
    shared.sessions.record_call(older, std::time::Instant::now());
    app.registry.pane_mut(user).unwrap().creator = PaneOwner::Agent(older);
    assert!(app.run_action(&Action::ShowAgents));
    assert_eq!(app.interaction.command_palette.mode(), CommandPaletteMode::Agents);
    assert_eq!(
        kinds(&app),
        [
            Kind::Info,
            Kind::Info,
            Kind::Session(newer.get()),
            Kind::Session(older.get()),
            Kind::DisconnectAll
        ]
    );
    let rows = app.interaction.command_palette.agent_rows();
    assert_eq!(rows[0].label, "Endpoint /tmp/sonicterm-test.sock");
    assert!(rows[1].label.contains("control agent"), "{}", rows[1].label);
    assert!(rows[1].label.contains("read agent"), "{}", rows[1].label);
    let claude = &rows[3].label;
    assert!(claude.starts_with(&format!("claude · #{} · ", older.get())), "{claude}");
    assert!(claude.contains("1 calls") && claude.ends_with("1 panes"), "{claude}");
    assert!(claude.contains("last 0s ago"), "{claude}");
    let unknown = &rows[2].label;
    assert!(unknown.starts_with("unknown · "), "{unknown}");
    assert!(!unknown.contains("last"), "a session with no call shows no last-call age: {unknown}");
    shared.stop();
}

/// Moving the highlight and typing a filter only change what the page shows; no session closes.
#[test]
fn navigation_and_filtering_never_disconnect() {
    let (mut app, shared, _user) = app_with_server();
    let (first, second) = (shared.sessions.open(), shared.sessions.open());
    shared.sessions.set_client(first, Some("claude".into()));
    app.run_action(&Action::ShowAgents);
    for key in [NamedKey::ArrowDown, NamedKey::ArrowDown, NamedKey::ArrowUp] {
        press(&mut app, key);
    }
    app.interaction.command_palette.input_str("claude");
    assert_eq!(kinds(&app), [Kind::Session(first.get())]);
    assert_eq!(shared.sessions.is_closed(first), Some(false));
    assert_eq!(shared.sessions.is_closed(second), Some(false));
    press(&mut app, NamedKey::Escape);
    assert!(!app.interaction.command_palette.is_open());
    assert_eq!(shared.sessions.is_closed(first), Some(false));
    shared.stop();
}

/// Enter on a session opens the confirm page with Cancel highlighted and the consequences listed;
/// Enter there, or Escape, closes it and changes nothing. Apply ends exactly that agent and shows
/// the page's own confirmation.
#[test]
fn enter_confirms_and_cancel_changes_nothing() {
    let (mut app, shared, _user) = app_with_server();
    let (other, chosen) = (shared.sessions.open(), shared.sessions.open());
    shared.sessions.set_client(chosen, Some("claude".into()));
    for cancel in [NamedKey::Enter, NamedKey::Escape] {
        app.run_action(&Action::ShowAgents);
        press(&mut app, NamedKey::Enter);
        let palette = &app.interaction.command_palette;
        assert_eq!(palette.mode(), CommandPaletteMode::Confirm);
        assert!(palette.confirm_title().contains("claude"), "{}", palette.confirm_title());
        assert!(!palette.confirm_applies(), "Cancel is highlighted first");
        let items: Vec<&str> = palette
            .confirm_rows()
            .iter()
            .filter_map(|row| match row {
                ConfirmRow::Item(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(items.iter().any(|text| text.contains("/mcp")), "{items:?}");
        assert!(items.iter().any(|text| text.contains("keep running")), "{items:?}");
        press(&mut app, cancel);
        assert!(!app.interaction.command_palette.is_open());
        assert_eq!(shared.sessions.is_closed(chosen), Some(false), "{cancel:?} changed nothing");
    }
    app.run_action(&Action::ShowAgents);
    press(&mut app, NamedKey::Enter);
    press(&mut app, NamedKey::ArrowUp);
    assert!(app.interaction.command_palette.confirm_applies());
    press(&mut app, NamedKey::Enter);
    assert_eq!(shared.sessions.is_closed(chosen), Some(true));
    assert_eq!(shared.sessions.is_closed(other), Some(false), "only the chosen agent");
    assert_eq!(shown(&app).as_deref(), Some("Disconnected claude"));
    assert!(shared.inbox.lock().is_empty(), "no separate departure notice is queued");
    shared.stop();
}

/// A session that left before Enter, or between Enter and Apply, is a no-op: nothing else closes
/// and the page says it was already gone.
#[test]
fn a_stale_session_is_a_no_op() {
    let (mut app, shared, _user) = app_with_server();
    let (other, gone) = (shared.sessions.open(), shared.sessions.open());
    app.run_action(&Action::ShowAgents);
    shared.sessions.close(gone);
    press(&mut app, NamedKey::Enter);
    assert!(!app.interaction.command_palette.is_open(), "no confirm page for a gone agent");
    assert_eq!(shown(&app).as_deref(), Some("That agent had already disconnected"));
    let late = shared.sessions.open();
    app.run_action(&Action::ShowAgents);
    press(&mut app, NamedKey::Enter);
    shared.sessions.close(late);
    press(&mut app, NamedKey::ArrowUp);
    press(&mut app, NamedKey::Enter);
    assert_eq!(shown(&app).as_deref(), Some("That agent had already disconnected"));
    assert_eq!(shared.sessions.is_closed(other), Some(false), "nothing else closed");
    shared.stop();
}

/// Disconnect all, confirmed, ends every session connected at confirm time, one that arrived
/// after the page opened included.
#[test]
fn disconnect_all_ends_every_live_session() {
    let (mut app, shared, _user) = app_with_server();
    let first = shared.sessions.open();
    app.run_action(&Action::ShowAgents);
    press(&mut app, NamedKey::ArrowDown);
    assert_eq!(app.interaction.command_palette.selected_agent_row(), Some(Kind::DisconnectAll));
    press(&mut app, NamedKey::Enter);
    let arrived = shared.sessions.open();
    press(&mut app, NamedKey::ArrowUp);
    press(&mut app, NamedKey::Enter);
    assert_eq!(shared.sessions.is_closed(first), Some(true));
    assert_eq!(shared.sessions.is_closed(arrived), Some(true));
    assert_eq!(shown(&app).as_deref(), Some("Disconnected 2 agents"));
    shared.stop();
}

/// With the server off the page still opens, with text rows saying how to turn it on; Enter does
/// nothing there.
#[test]
fn the_page_opens_with_the_server_off() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("user");
    assert!(app.run_action(&Action::ShowAgents));
    assert_eq!(kinds(&app), [Kind::Info, Kind::Info, Kind::Info]);
    let rows = app.interaction.command_palette.agent_rows();
    assert!(rows.iter().any(|row| row.label.contains("[mcp] enabled = true")), "{rows:?}");
    press(&mut app, NamedKey::Enter);
    assert_eq!(app.interaction.command_palette.mode(), CommandPaletteMode::Agents);
}

/// Run from a child window, the page attaches to that window, as every palette page does.
#[test]
fn the_page_attaches_to_a_child_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["work"]);
    assert!(app.run_action_for_window(&Action::ShowAgents, child));
    assert_eq!(app.interaction.palette_attached_window, Some(child));
    assert_eq!(app.interaction.command_palette.mode(), CommandPaletteMode::Agents);
}

/// The palette draws each row as one line clipped at its edge, about 60 columns wide at the default
/// font, so the page builds every row, the confirm title and every confirm line within
/// `AGENT_ROW_COLUMNS`, in each shipped locale, with long client names and a long endpoint.
#[test]
fn every_row_and_confirm_line_fits_the_palette_width() {
    use unicode_width::UnicodeWidthStr;
    let limit = super::AGENT_ROW_COLUMNS;
    let fits = |text: &str, what: &str| {
        assert!(text.width() <= limit, "{what} is {} columns: {text}", text.width());
    };
    let (mut app, shared, user) = app_with_server();
    if let Some(server) = app.services.mcp_server.as_mut() {
        server.endpoint =
            format!("/private/var/folders/{}/T/sonicterm-501/12345.sock", "x".repeat(48));
    }
    let wide = shared.sessions.open();
    shared.sessions.set_client(wide, Some("長いクライアント名".repeat(8)));
    let long = shared.sessions.open();
    shared
        .sessions
        .set_client(long, Some("claude-code-with-a-very-long-self-reported-name".repeat(3)));
    for _ in 0..120 {
        shared.sessions.record_call(long, std::time::Instant::now());
    }
    app.registry.pane_mut(user).unwrap().creator = PaneOwner::Agent(long);
    for locale in ["en", "ja", "zh-CN"] {
        app.i18n = sonicterm_ui::i18n::I18n::new(Some(locale));
        app.run_action(&Action::ShowAgents);
        let rows: Vec<String> = app
            .interaction
            .command_palette
            .agent_rows()
            .iter()
            .map(|row| row.label.clone())
            .collect();
        for label in &rows {
            fits(label, &format!("{locale} row"));
        }
        // Every selectable row opens a confirm page whose title and lines must fit as well.
        for index in 0..rows.len() {
            app.run_action(&Action::ShowAgents);
            if !app.interaction.command_palette.select_agent_row(index) {
                continue;
            }
            press(&mut app, NamedKey::Enter);
            let palette = &app.interaction.command_palette;
            fits(palette.confirm_title(), &format!("{locale} confirm title"));
            for row in palette.confirm_rows() {
                fits(row.label(), &format!("{locale} confirm line"));
            }
            press(&mut app, NamedKey::Escape);
        }
        let mut off = App::new(Theme::default(), Config::default(), Keymap::default());
        off.__test_seed_tab("user");
        off.i18n = sonicterm_ui::i18n::I18n::new(Some(locale));
        off.run_action(&Action::ShowAgents);
        for row in off.interaction.command_palette.agent_rows() {
            fits(&row.label, &format!("{locale} server-off row"));
        }
    }
    shared.stop();
}

/// A session row shows its name whole and its last-call age when the row fits; a name too long
/// for the row is cut with an ellipsis and the last-call age is dropped, never the pane count.
#[test]
fn a_long_name_is_cut_and_the_last_call_age_drops_first() {
    let (mut app, shared, _user) = app_with_server();
    let short = shared.sessions.open();
    shared.sessions.set_client(short, Some("claude".into()));
    shared.sessions.record_call(short, std::time::Instant::now());
    let long = shared.sessions.open();
    let name = "copilot-cli-with-a-really-long-client-name-for-testing";
    shared.sessions.set_client(long, Some(name.into()));
    shared.sessions.record_call(long, std::time::Instant::now());
    app.run_action(&Action::ShowAgents);
    let label = |session: crate::app::mcp::agents::AgentSessionId| {
        let rows = app.interaction.command_palette.agent_rows();
        rows.iter().find(|row| row.kind == Kind::Session(session.get())).unwrap().label.clone()
    };
    let short_row = label(short);
    assert!(short_row.starts_with("claude · #"), "{short_row}");
    assert!(short_row.contains("last 0s ago") && short_row.ends_with("0 panes"), "{short_row}");
    let long_row = label(long);
    assert!(long_row.starts_with("copilot-cli") && long_row.contains('…'), "{long_row}");
    assert!(!long_row.contains("last"), "the age is dropped first: {long_row}");
    assert!(long_row.ends_with("0 panes"), "{long_row}");
    shared.stop();
}
