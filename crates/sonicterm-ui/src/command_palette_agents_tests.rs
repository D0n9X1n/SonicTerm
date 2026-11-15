//! Pins the Agents page model: rows in the order the host gave, info rows never highlighted,
//! Up and Down skip them, a filter keeps only matching sessions, and the rows show only while the
//! page is open.

use super::*;

/// The page a host with two agents builds: a header, two sessions, then Disconnect all.
fn two_agents() -> Vec<AgentRow> {
    vec![
        AgentRow { kind: AgentRowKind::Info, label: "Endpoint /tmp/sock · control agent".into() },
        AgentRow { kind: AgentRowKind::Session(9), label: "claude-code · session 9".into() },
        AgentRow { kind: AgentRowKind::Session(4), label: "copilot · session 4".into() },
        AgentRow { kind: AgentRowKind::DisconnectAll, label: "Disconnect all agents".into() },
    ]
}

/// The kinds of the rows the page shows, in display order.
fn kinds(palette: &CommandPalette) -> Vec<AgentRowKind> {
    palette.agent_rows().iter().map(|row| row.kind).collect()
}

/// Opening the page shows every row in the host's order, with its title and footer, and highlights
/// the first session rather than the header, so Enter never lands on a line of text.
#[test]
fn the_page_opens_on_the_first_selectable_row() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "↵ disconnect · esc close", two_agents());
    assert!(palette.is_open());
    assert_eq!(palette.mode(), CommandPaletteMode::Agents);
    assert_eq!(palette.agents_title(), "Agents");
    assert_eq!(palette.agents_footer(), "↵ disconnect · esc close");
    assert_eq!(
        kinds(&palette),
        [
            AgentRowKind::Info,
            AgentRowKind::Session(9),
            AgentRowKind::Session(4),
            AgentRowKind::DisconnectAll
        ]
    );
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::Session(9)));
}

/// Down and Up step only through selectable rows and wrap past the header at both ends.
#[test]
fn navigation_skips_info_rows_and_wraps() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "", two_agents());
    palette.move_selection_down();
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::Session(4)));
    palette.move_selection_down();
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::DisconnectAll));
    palette.move_selection_down();
    assert_eq!(
        palette.selected_agent_row(),
        Some(AgentRowKind::Session(9)),
        "wraps past the header"
    );
    palette.move_selection_up();
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::DisconnectAll));
}

/// A filter keeps only the sessions whose text contains it, without case; the header and
/// Disconnect all are hidden while filtering, because "all" would still mean every agent.
#[test]
fn a_filter_keeps_matching_sessions_only() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "", two_agents());
    palette.set_query("COPILOT");
    assert_eq!(kinds(&palette), [AgentRowKind::Session(4)]);
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::Session(4)));
    palette.set_query("nothing like it");
    assert!(palette.agent_rows().is_empty());
    assert_eq!(palette.selected_agent_row(), None);
    palette.set_query("");
    assert_eq!(kinds(&palette).len(), 4, "clearing the filter restores every row");
}

/// A pointer can highlight a session row but never an info row, and only on the open page.
#[test]
fn only_selectable_rows_can_be_highlighted() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "", two_agents());
    assert!(!palette.select_agent_row(0), "the header is text");
    assert!(palette.select_agent_row(3));
    assert_eq!(palette.selected_agent_row(), Some(AgentRowKind::DisconnectAll));
    assert!(!palette.select_agent_row(9), "no such row");
    palette.close();
    assert!(!palette.select_agent_row(1), "closed");
}

/// The rows, title and footer show only while the page is open: another mode or a closed palette
/// shows none, and reopening the command list leaves nothing behind.
#[test]
fn rows_show_only_on_the_open_page() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "", two_agents());
    palette.close();
    assert!(palette.agent_rows().is_empty());
    assert_eq!(palette.selected_agent_row(), None);
    assert_eq!(palette.agents_title(), "");
    palette.open();
    assert!(palette.agent_rows().is_empty());
}

/// A page refreshed with different rows paints differently, so the frame cache cannot reuse the
/// old list; the same rows hash the same.
#[test]
fn the_rows_feed_the_presentation_hash() {
    let mut palette = CommandPalette::new();
    palette.start_agents("Agents", "", two_agents());
    let first = palette.presentation_hash();
    palette.start_agents("Agents", "", two_agents());
    assert_eq!(palette.presentation_hash(), first);
    let mut fewer = two_agents();
    fewer.remove(2);
    palette.start_agents("Agents", "", fewer);
    assert_ne!(palette.presentation_hash(), first);
}
