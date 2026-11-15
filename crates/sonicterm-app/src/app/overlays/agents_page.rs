//! The Agents palette page: one snapshot of the connected MCP agents, joined with the panes each
//! owns, and a disconnect that always goes through the confirm page. Enter or a click on a session
//! row or on Disconnect all opens the confirm page (Cancel highlighted first); only Apply there
//! closes a connection, through the same path a client hang-up takes.

use std::time::{Duration, Instant};

use sonicterm_ui::command_palette::{AgentRow, AgentRowKind};
use sonicterm_ui::overlays::NotificationLevel;

use crate::app::mcp::agents::{PaneOwner, SessionSnapshot};
use crate::app::{App, FrontmostKind};

/// Who the confirm page will disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum DisconnectTarget {
    /// One session, by its number; looked up again in the live set on Apply.
    One(u64),
    /// Every session connected when the user applies.
    All,
}

/// A disconnect waiting on the confirm page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct AgentDisconnect {
    /// The palette session that shows the confirm page; Apply from any other is ignored.
    palette_session: u64,
    /// Who Apply disconnects.
    target: DisconnectTarget,
}

/// `elapsed` in its largest whole unit: seconds, minutes or hours.
fn short_age(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

impl App {
    /// Open the Agents page on the frontmost window, listing one snapshot of the live sessions.
    pub(in crate::app) fn open_agents_page(&mut self) {
        self.interaction.palette_pointer_capture = None;
        self.interaction.agent_disconnect = None;
        self.interaction.palette_attached_window = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        let rows = self.agent_rows();
        let title = self.i18n.translate("agents-title");
        let footer = self.i18n.translate("agents-footer");
        self.interaction.command_palette.start_agents(title, footer, rows);
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// The page's rows: the server line, then each live session newest first, then Disconnect
    /// all; one text row when the server is off or no agent is connected.
    fn agent_rows(&self) -> Vec<AgentRow> {
        let info = |label: String| AgentRow { kind: AgentRowKind::Info, label };
        let Some(server) = self.services.mcp_server.as_ref() else {
            // When: no `mcp_server` runs, the page says how to turn it on and lists nothing.
            return vec![info(self.i18n.translate("agents-server-off"))];
        };
        let shared = &server.shared;
        let header = self.i18n.t_args(
            "agents-server",
            Some(&[
                ("endpoint", server.endpoint.as_str()),
                ("control", shared.control().as_str()),
                ("read", shared.read().as_str()),
            ]),
        );
        let mut rows = vec![info(header)];
        let sessions = shared.sessions.snapshot();
        if sessions.is_empty() {
            // When: the snapshot holds no `sessions`, there is no row to act on.
            rows.push(info(self.i18n.translate("agents-none")));
            return rows;
        }
        let now = Instant::now();
        for entry in &sessions {
            rows.push(AgentRow {
                kind: AgentRowKind::Session(entry.session.get()),
                label: self.agent_row_label(entry, now),
            });
        }
        rows.push(AgentRow {
            kind: AgentRowKind::DisconnectAll,
            label: self.i18n.translate("agents-disconnect-all"),
        });
        rows
    }

    /// One session's row text: name, number, connected for, tool calls, last call ago, panes owned.
    fn agent_row_label(&self, entry: &SessionSnapshot, now: Instant) -> String {
        let panes = self
            .registry
            .windows
            .values()
            .flat_map(|window| window.panes.values())
            .filter(|pane| pane.creator == PaneOwner::Agent(entry.session))
            .count();
        let last = match entry.last_call {
            Some(at) => self.i18n.t_args(
                "agents-ago",
                Some(&[("age", short_age(now.saturating_duration_since(at)).as_str())]),
            ),
            None => self.i18n.translate("agents-never"),
        };
        self.i18n.t_args(
            "agents-session",
            Some(&[
                ("name", entry.client.as_deref().unwrap_or("unknown")),
                ("session", entry.session.get().to_string().as_str()),
                (
                    "connected",
                    short_age(now.saturating_duration_since(entry.connected_at)).as_str(),
                ),
                ("calls", entry.tool_calls.to_string().as_str()),
                ("last", last.as_str()),
                ("panes", panes.to_string().as_str()),
            ]),
        )
    }

    /// The live session numbered `number`, looked up in a fresh snapshot.
    fn live_agent(&self, number: u64) -> Option<SessionSnapshot> {
        let server = self.services.mcp_server.as_ref()?;
        server.shared.sessions.snapshot().into_iter().find(|entry| entry.session.get() == number)
    }

    /// Enter (or a click) on the highlighted row: open the confirm page for it. An info row does
    /// nothing; a session that already left closes the page with a notice instead.
    pub(in crate::app) fn activate_agent_row(&mut self) {
        let Some(kind) = self.interaction.command_palette.selected_agent_row() else {
            // When: `selected_agent_row` is None (an info row or an empty filter), Enter does nothing.
            return;
        };
        let (title, target) = match kind {
            AgentRowKind::Info => {
                // When: `kind` is `Info`, the row is text and opens nothing.
                return;
            }
            AgentRowKind::DisconnectAll => {
                (self.i18n.translate("agents-confirm-title-all"), DisconnectTarget::All)
            }
            AgentRowKind::Session(number) => {
                // When: `kind` is a `Session`, it is looked up again so a gone agent is not confirmed.
                let Some(entry) = self.live_agent(number) else {
                    // When: `live_agent` no longer finds the session, it left; nothing to confirm.
                    self.finish_agents_page(Some(self.i18n.translate("agents-gone")));
                    return;
                };
                let session = number.to_string();
                let name = entry.client.as_deref().unwrap_or("unknown");
                let title = self.i18n.t_args(
                    "agents-confirm-title-one",
                    Some(&[("name", name), ("session", session.as_str())]),
                );
                (title, DisconnectTarget::One(number))
            }
        };
        let items = ["agents-confirm-tools", "agents-confirm-client", "agents-confirm-panes"]
            .into_iter()
            .map(|key| self.i18n.translate(key))
            .collect();
        self.interaction.palette_pointer_capture = None;
        self.interaction.command_palette.start_confirm(
            title,
            self.i18n.translate("agents-confirm-apply"),
            self.i18n.translate("agents-confirm-cancel"),
            items,
        );
        self.interaction.agent_disconnect = Some(AgentDisconnect {
            palette_session: self.interaction.command_palette.session(),
            target,
        });
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// Close an open confirm page; `apply` is true only for Apply on it. The page belongs to the
    /// Agents disconnect when it opened it, otherwise to the default-handler command.
    pub(in crate::app) fn close_confirm_page(&mut self, apply: bool) {
        let session = self.interaction.command_palette.session();
        let pending = self.interaction.agent_disconnect.take();
        match pending.filter(|pending| pending.palette_session == session) {
            Some(pending) => self.confirm_agent_disconnect(pending.target, apply),
            None => self.confirm_default_handlers(apply),
        }
    }

    /// Apply or cancel a confirmed disconnect: the session is looked up in the live set again, so
    /// one that already left is a no-op, and "all" means every session connected now.
    fn confirm_agent_disconnect(&mut self, target: DisconnectTarget, apply: bool) {
        if !apply {
            // When: `apply` is false (Cancel or Escape), no connection closes.
            self.finish_agents_page(None);
            return;
        }
        let Some(shared) = self.services.mcp_server.as_ref().map(|server| server.shared.clone())
        else {
            // When: the server stopped since the page opened, every agent is already gone.
            self.finish_agents_page(Some(self.i18n.translate("agents-gone")));
            return;
        };
        let message = match target {
            DisconnectTarget::All => {
                let count = shared.disconnect_all_agents().to_string();
                self.i18n.t_args("agents-disconnected-all", Some(&[("count", count.as_str())]))
            }
            DisconnectTarget::One(number) => match self.live_agent(number) {
                Some(entry) if shared.disconnect_agent(entry.session) => {
                    let name = entry.client.as_deref().unwrap_or("unknown");
                    self.i18n.t_args("agents-disconnected-one", Some(&[("name", name)]))
                }
                // A session that left after Enter is gone; nothing else is touched.
                _ => self.i18n.translate("agents-gone"),
            },
        };
        self.finish_agents_page(Some(message));
    }

    /// Close the palette and, when there is one, show `message` on the window it was attached to.
    pub(in crate::app) fn finish_agents_page(&mut self, message: Option<String>) {
        let source = self.interaction.palette_attached_window;
        let kind = match source {
            Some(id) if self.registry.windows.contains_key(&id) => self.kind_for(id),
            _ => self.frontmost_kind(),
        };
        self.interaction.command_palette.close();
        self.interaction.palette_attached_window = None;
        self.interaction.agent_disconnect = None;
        self.request_redraw_for_overlay(source.or(self.registry.main_window_id));
        if let Some(message) = message {
            self.show_notification_for_kind(kind, NotificationLevel::Info, message);
        }
    }
}

#[cfg(test)]
#[path = "agents_page_tests.rs"]
mod agents_page_tests;
