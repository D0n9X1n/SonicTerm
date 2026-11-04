//! The control server's life inside the App: starting and stopping it as `[mcp]` changes, and
//! answering the requests connection threads queue for the event loop.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use sonicterm_cfg::config::{McpConfig, McpControl};
use sonicterm_resource::CommittedReservation;
use sonicterm_types::{OwnerKind, ResourceAmount, ResourceClass};

use super::{LoopCall, LoopReply, LoopRequest, McpShared, PaneEntry};
use crate::app::owners::{tracking_only_owner_limits, OwnerGuard};
use crate::app::{App, CopyModeState, WindowId};

/// A running control server: its shared state, its endpoint and the config it runs with.
pub(crate) struct McpServer {
    /// State shared with the listener and every connection.
    pub(crate) shared: Arc<McpShared>,
    /// The endpoint path published to new panes as `SONICTERM_CONTROL`.
    pub(crate) endpoint: String,
    /// Removes the endpoint's file when the server is dropped; `None` in tests that bind nothing.
    /// Held only for its drop.
    pub(crate) _endpoint_guard: Option<super::EndpointGuard>,
    /// The `[mcp]` values the server runs with.
    pub(crate) config: McpConfig,
    /// Connection buffers charged to `owner`; declared first so they are released before it.
    pub(crate) charges: HashMap<ResourceClass, CommittedReservation>,
    /// The governor's process-level `Mcp` owner; `None` when it could not register.
    pub(crate) owner: Option<OwnerGuard>,
}

// Lifecycle: McpServer drop calls stop on the listener and every connection; then
// `_endpoint_guard` removes the socket file, and `charges` and `owner` drop in order.
impl Drop for McpServer {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

/// The config spelling of a control level.
pub(crate) fn control_name(control: McpControl) -> &'static str {
    control.as_str()
}

impl App {
    /// Bring the server in line with `self.config.mcp`: start it, stop it, or change its control
    /// level. A reload that changes nothing in `[mcp]` leaves the server and its requests alone.
    pub(crate) fn reconcile_mcp(&mut self) {
        let wanted = self.config.mcp;
        let running = self.mcp_server.as_ref().map(|server| server.config);
        if running == Some(wanted) || (!wanted.enabled && running.is_none()) {
            // When: the `running` config already equals `wanted`, nothing changes.
            return;
        }
        if !wanted.enabled {
            // When: `wanted.enabled` is off, the server stops and its endpoint goes away.
            self.mcp_server = None;
            tracing::info!(target: "mcp", "control server stopped");
            return;
        }
        if let Some(server) = self.mcp_server.as_mut() {
            // When: a `server` runs and only the control level changed, queued requests are revoked.
            server.shared.set_control(wanted.control);
            server.config = wanted;
            tracing::info!(target: "mcp", control = wanted.control.as_str(), "control level changed");
            return;
        }
        let shared = McpShared::new(wanted.control, self.event_loop_proxy.clone());
        match super::start_listener(&shared) {
            Ok((endpoint, guard)) => {
                tracing::info!(target: "mcp", control = wanted.control.as_str(), "control server started");
                let owner = self.register_mcp_owner();
                self.mcp_server = Some(McpServer {
                    shared,
                    endpoint,
                    _endpoint_guard: Some(guard),
                    config: wanted,
                    charges: HashMap::new(),
                    owner,
                });
            }
            Err(error) => {
                tracing::warn!(target: "mcp", %error, "control server could not start");
            }
        }
    }

    /// Register the governor's `Mcp` owner below the process root. A server that cannot register
    /// still runs; its buffers are then missing from the ledger, which the log says.
    pub(crate) fn register_mcp_owner(&self) -> Option<OwnerGuard> {
        let root = self.governor.root_owner();
        match self.governor.create_child(root, OwnerKind::Mcp, tracking_only_owner_limits()) {
            Ok(owner) => Some(OwnerGuard::new(self.governor.clone(), owner)),
            Err(error) => {
                tracing::warn!(target: "mcp", ?error, "control server owner registration failed");
                None
            }
        }
    }

    /// Charge what every connection holds now to the `Mcp` owner. Runs with the retention sample,
    /// so the figure lags by at most one interval, like every other charge.
    // Ordering: connections Acquire reads the live count; it is a sampled gauge.
    pub(crate) fn charge_mcp_connections(&mut self) {
        let Some(server) = self.mcp_server.as_mut() else {
            // When: no `mcp_server` runs, there is nothing to charge.
            return;
        };
        let Some(owner) = server.owner.as_ref().map(OwnerGuard::id) else {
            // When: the server has no `owner`, its buffers cannot be charged.
            return;
        };
        let amount = ResourceAmount {
            bytes: usize::try_from(server.shared.buffered()).unwrap_or(usize::MAX),
            items: usize::try_from(server.shared.connections.load(Ordering::Acquire))
                .unwrap_or(usize::MAX),
        };
        crate::app::retention::charge_classes(
            &self.governor,
            owner,
            &mut server.charges,
            [(ResourceClass::ControlConnection, amount)],
        );
    }

    /// The endpoint path new panes get as `SONICTERM_CONTROL`, while the server runs.
    pub(crate) fn mcp_endpoint(&self) -> Option<&str> {
        self.mcp_server.as_ref().map(|server| server.endpoint.as_str())
    }

    /// Answer every request connection threads queued for the event loop.
    pub(crate) fn drain_mcp_requests(&mut self) {
        let Some(shared) = self.mcp_server.as_ref().map(|server| Arc::clone(&server.shared)) else {
            // When: no `mcp_server` runs, a late wake-up has nothing to answer.
            return;
        };
        for request in shared.take_requests() {
            if matches!(request.call, LoopCall::Notice { .. }) {
                // A taken notice frees its slot whether it is shown or dropped as revoked.
                shared.notice_shown();
            }
            let reply = self.answer_mcp_request(&shared, &request);
            // A full or disconnected reply channel means the waiter already gave up.
            let _ = request.reply.try_send(reply);
        }
    }

    /// Recheck a request against the server's current state, then do its work.
    // Ordering: Acquire on `closed` and `generation` pairs with their writers' Release stores.
    fn answer_mcp_request(&mut self, shared: &McpShared, request: &LoopRequest) -> LoopReply {
        if request.closed.load(Ordering::Acquire)
            || shared.is_stopped()
            || shared.generation.load(Ordering::Acquire) != request.generation
            || Instant::now() >= request.deadline
        {
            // When: the client is `closed`, the server stopped or changed `generation`, or the
            // `deadline` passed, the request is dropped unrun.
            return LoopReply::Refused("revoked", "the request was dropped before it ran".into());
        }
        let control = shared.control();
        match &request.call {
            LoopCall::ListPanes => LoopReply::Panes(self.mcp_pane_entries()),
            LoopCall::Notice { text } => {
                self.show_notification_for_kind(
                    self.frontmost_kind(),
                    sonicterm_ui::overlays::NotificationLevel::Info,
                    text.clone(),
                );
                LoopReply::Panes(Vec::new())
            }
            LoopCall::CreatePane { anchor, placement, launch } => {
                self.mcp_create_pane(control, *anchor, *placement, launch.clone())
            }
            LoopCall::SendText { pane_id, text, submit } => {
                self.mcp_send_text(control, *pane_id, text, *submit)
            }
            LoopCall::SendKeys { pane_id, keys } => self.mcp_send_keys(control, *pane_id, keys),
            LoopCall::RunStart { pane_id, command } => {
                self.mcp_run_start(control, *pane_id, command)
            }
            LoopCall::ClosePane { pane_id } => self.mcp_close_pane(control, *pane_id),
            LoopCall::ResolvePane { pane_id } => {
                match self.mcp_pane_entries().into_iter().find(|entry| entry.pane_id == *pane_id) {
                    Some(entry) => LoopReply::Pane(entry),
                    None => LoopReply::Refused("pane_gone", format!("no pane {pane_id}")),
                }
            }
        }
    }

    /// Every pane in every window, main first, then children in creation order.
    pub(crate) fn mcp_pane_entries(&self) -> Vec<PaneEntry> {
        let mut windows: Vec<WindowId> = self.windows.keys().copied().collect();
        windows.sort_by_key(|id| {
            let is_main = Some(*id) == self.main_window_id;
            (!is_main, self.window_key(*id).map(|key| key.0))
        });
        let mut entries = Vec::new();
        for (window_index, window_id) in windows.into_iter().enumerate() {
            let window = &self.windows[&window_id];
            if window.role.is_report() {
                // When: the window `role` is a report window, it holds no terminal panes.
                continue;
            }
            let readonly = window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only);
            let active_tab = window.tabs.active_index();
            for (tab_index, tab_state) in window.tab_states.iter().enumerate() {
                for pane_id in tab_state.tree.leaves() {
                    let Some(pane) = window.panes.get(&pane_id) else {
                        // When: a leaf has no live pane, it is mid-teardown and is skipped.
                        continue;
                    };
                    entries.push(PaneEntry {
                        pane_id,
                        window_index,
                        tab_index,
                        active: tab_index == active_tab && tab_state.active_pane == pane_id,
                        readonly,
                        agent_owned: pane.agent_owned,
                        parser: Arc::clone(&pane.parser),
                        output_generation: Arc::clone(&pane.output_generation),
                        exit_status: Arc::clone(&pane.exit_status),
                        run_typed_at: Arc::clone(&pane.run_typed_at),
                        agent_run: Arc::clone(&pane.agent_run),
                    });
                }
            }
        }
        entries
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
