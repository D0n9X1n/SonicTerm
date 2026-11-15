//! The Agents palette page: a plain list of the connected MCP agents that the host fills from one
//! snapshot. Info rows (the server line, the empty-state line) are text only; a session row and
//! Disconnect all are selectable, and activating one is the host's job (it opens a confirm page).

use super::*;

/// What one Agents page row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRowKind {
    /// Text only: the server line, or why there is nothing to list; never highlighted.
    Info,
    /// One connected agent session, by its number.
    Session(u64),
    /// Every agent connected when the user confirms.
    DisconnectAll,
}

/// One Agents page row: what it is and the text it shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentRow {
    /// What activating the row does.
    pub kind: AgentRowKind,
    /// The prepared, localized text the row shows.
    pub label: String,
}

impl AgentRow {
    /// Whether the row can be highlighted and activated.
    fn selectable(&self) -> bool {
        self.kind != AgentRowKind::Info
    }
}

/// The Agents page's query placeholder, key help and rows while the palette shows it; `items`
/// indexes `rows`.
#[derive(Debug, Clone, Default)]
pub(super) struct AgentsPage {
    title: String,
    footer: String,
    rows: Vec<AgentRow>,
}

impl CommandPalette {
    /// Open the Agents page with `rows` in display order, highlighting the first selectable row.
    /// `title` is the query placeholder and `footer` the key help.
    pub fn start_agents(
        &mut self,
        title: impl Into<String>,
        footer: impl Into<String>,
        rows: Vec<AgentRow>,
    ) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::Agents;
        self.tabs_only = false;
        self.window_name_error = None;
        self.query.clear();
        self.selection = TextSelection::default();
        self.agents = AgentsPage { title: title.into(), footer: footer.into(), rows };
        self.scroll_offset = 0;
        self.filter_agent_rows();
        self.selected = self.first_selectable_row();
        self.ensure_selected_in_view();
    }

    /// The Agents rows the query keeps, in display order; empty in any other mode.
    pub fn agent_rows(&self) -> Vec<&AgentRow> {
        if self.mode != CommandPaletteMode::Agents {
            // When: mode is not Agents, rows from a closed page must not show.
            return Vec::new();
        }
        self.items.iter().filter_map(|&index| self.agents.rows.get(index)).collect()
    }

    /// What the highlighted Agents row does, if the page is open and a selectable row is highlighted.
    pub fn selected_agent_row(&self) -> Option<AgentRowKind> {
        if self.mode != CommandPaletteMode::Agents {
            // When: mode is not Agents, no Agents row is highlighted.
            return None;
        }
        let index = *self.items.get(self.selected)?;
        self.agents.rows.get(index).filter(|row| row.selectable()).map(|row| row.kind)
    }

    /// Highlight Agents row `index` of the filtered view; an info row is never highlighted.
    pub fn select_agent_row(&mut self, index: usize) -> bool {
        let selectable = self.mode == CommandPaletteMode::Agents
            && self
                .items
                .get(index)
                .and_then(|&row| self.agents.rows.get(row))
                .is_some_and(AgentRow::selectable);
        if selectable {
            self.selected = index;
            self.ensure_selected_in_view();
        }
        selectable
    }

    /// The Agents page's query placeholder.
    pub fn agents_title(&self) -> &str {
        &self.agents.title
    }

    /// The Agents page's key help.
    pub fn agents_footer(&self) -> &str {
        &self.agents.footer
    }

    /// Keep every row for an empty query; otherwise only the session rows whose text contains it,
    /// without case. Disconnect all is hidden while filtering, since it still means every agent.
    pub(super) fn filter_agent_rows(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.items = (0..self.agents.rows.len())
            .filter(|&index| {
                let row = &self.agents.rows[index];
                query.is_empty()
                    || (matches!(row.kind, AgentRowKind::Session(_))
                        && row.label.to_lowercase().contains(&query))
            })
            .collect();
    }

    /// Forget the page's rows when the palette closes.
    pub(super) fn clear_agents(&mut self) {
        self.agents = AgentsPage::default();
    }

    /// Add the page's rows, title and footer to the presentation hash while it shows, since a
    /// refreshed page can change only row text.
    pub(super) fn hash_agents(&self, hash: &mut impl Hasher) {
        if self.mode == CommandPaletteMode::Agents {
            self.agents.title.hash(hash);
            self.agents.footer.hash(hash);
            self.agents.rows.hash(hash);
        }
    }

    /// Whether `items[index]` can be highlighted on a page with unselectable rows: the checklist
    /// or the Agents page.
    pub(super) fn row_selectable(&self, index: usize) -> bool {
        let row = self.items[index];
        match self.mode {
            CommandPaletteMode::Agents => self.agents.rows[row].selectable(),
            _ => self.status_rows[row].selectable(),
        }
    }

    /// The filtered index of the first selectable row, or 0 when none is left.
    pub(super) fn first_selectable_row(&self) -> usize {
        (0..self.items.len()).find(|&index| self.row_selectable(index)).unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "command_palette_agents_tests.rs"]
mod command_palette_agents_tests;
