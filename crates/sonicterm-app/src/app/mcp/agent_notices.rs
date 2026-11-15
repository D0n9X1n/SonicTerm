//! When agent connections reach the user. A connection is announced once, when its first admitted
//! tool call starts, so a probe that only initializes and lists tools shows nothing. Its departure
//! is shown only while the session still owns panes when the event loop handles it, and never for
//! a disconnect the user made from the Agents page, which shows its own confirmation. Every
//! connect and disconnect is still logged.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::Instant;

use super::agents::{AgentSessionId, Departure, PaneOwner};
use super::{LoopCall, LoopRequest, McpShared, LOOP_DEADLINE};
use crate::app::App;

impl McpShared {
    /// Count `session`'s admitted tool call and, on its first, announce the client. The connection
    /// counts as announced only when its notice was admitted to the queue.
    pub(crate) fn announce_first_call(&self, session: AgentSessionId) {
        let Some(name) = self.sessions.record_call(session, Instant::now()) else {
            // When: `record_call` returns no name, this is not the session's first call.
            return;
        };
        if self.notify(format!("Agent connected: {name}")) {
            self.sessions.mark_announced(session);
        }
    }

    /// Log a closed connection and, unless the user disconnected it or the server stopped, ask
    /// the loop whether to show its departure. The request holds a notice slot like any notice.
    pub(crate) fn report_departure(&self, session: AgentSessionId, departure: Option<Departure>) {
        let Some(departure) = departure else {
            // When: the session left no `departure` record, it was never live and nothing is said.
            tracing::info!(target: "mcp", session = session.get(), "agent disconnected");
            return;
        };
        tracing::info!(
            target: "mcp",
            session = session.get(),
            client = departure.client.as_deref().unwrap_or("unknown"),
            announced = departure.announced,
            by_user = departure.user_disconnected,
            "agent disconnected"
        );
        if departure.user_disconnected || self.is_stopped() {
            // When: the user disconnected it, or the server `is_stopped`, no departure notice shows.
            return;
        }
        let Some(slot) = self.admit_notice() else {
            // When: `admit_notice` found the queue full, the departure is logged only.
            return;
        };
        self.queue_notice(LoopCall::AgentLeft { session, client: departure.client, _slot: slot });
    }

    /// Queue `call`, a notice nobody waits for, under the current generation and wake the loop.
    // Ordering: generation Acquire reads the value the loop compares.
    pub(crate) fn queue_notice(&self, call: LoopCall) {
        // The reply channel is dropped at once; the loop's `try_send` to it is ignored.
        let (reply, _answer) = sync_channel(1);
        self.inbox.lock().push(LoopRequest {
            call,
            generation: self.generation.load(std::sync::atomic::Ordering::Acquire),
            deadline: Instant::now() + LOOP_DEADLINE,
            closed: Arc::new(AtomicBool::new(false)),
            // A notice reads no pane, so the session it carries is never checked.
            session: AgentSessionId::next(),
            reply,
        });
        let _ = self.wake();
    }
}

impl App {
    /// A departed session's notice on the loop: shown only when it still owns panes now, so a
    /// probe, an agent that created none, or one whose panes a reconnect already claimed is silent.
    pub(crate) fn mcp_agent_left(&mut self, session: AgentSessionId, client: Option<&str>) {
        let owned = self
            .registry
            .windows
            .values()
            .flat_map(|window| window.panes.values())
            .filter(|pane| pane.creator == PaneOwner::Agent(session))
            .count();
        if owned == 0 {
            // When: the session `owned` no pane, its departure leaves nothing behind to report.
            return;
        }
        let name = client.unwrap_or("unknown");
        self.show_notification_for_kind(
            self.frontmost_kind(),
            sonicterm_ui::overlays::NotificationLevel::Info,
            format!("Agent disconnected: {name}; {owned} pane(s) left without an agent"),
        );
    }
}

#[cfg(test)]
#[path = "agent_notices_tests.rs"]
mod agent_notices_tests;
