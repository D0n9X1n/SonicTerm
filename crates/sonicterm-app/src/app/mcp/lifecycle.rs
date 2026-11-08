//! The control server's life inside the App: starting and stopping it as `[mcp]` changes, and
//! answering the requests connection threads queue for the event loop.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use sonicterm_cfg::config::{McpConfig, McpControl};
use sonicterm_resource::CommittedReservation;
use sonicterm_types::{OwnerKind, ResourceAmount, ResourceClass};

use super::{LoopCall, LoopReply, LoopRequest, McpShared, PaneEntry, PaneMetadata};
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
    /// `[mcp] read` is not set and the user has not been told yet what that now means.
    pub(crate) read_notice_due: bool,
}

/// What an unset `[mcp] read` now means, shown once each time the server starts without it.
pub(crate) const READ_UNSET_NOTICE: &str =
    "[mcp] read is not set: agents now see only agent-created \
panes. Set read = \"any\" to restore the previous default.";

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
        let running = self.services.mcp_server.as_ref().map(|server| server.config);
        if running == Some(wanted) || (!wanted.enabled && running.is_none()) {
            // When: the `running` config already equals `wanted`, nothing changes.
            return;
        }
        if !wanted.enabled {
            // When: `wanted.enabled` is off, the server stops and its endpoint goes away.
            self.services.mcp_server = None;
            tracing::info!(target: "mcp", "control server stopped");
            return;
        }
        if let Some(server) = self.services.mcp_server.as_mut() {
            // When: a `server` runs and some `[mcp]` value changed; only `control` or `read`
            // revokes requests, and the connection limit applies to new connections alone.
            let previous = server.config;
            server.shared.set_connection_limit(wanted.max_connections());
            // The scope in force, with the default resolved: an unset key and an explicit
            // "agent" read the same panes, so switching between them revokes nothing.
            let scope = McpConfig::read;
            if previous.control != wanted.control || scope(&previous) != scope(&wanted) {
                server.shared.set_control(wanted.control);
                server.shared.set_read(wanted.read());
            }
            if (previous.isolation, previous.create_anchor)
                != (wanted.isolation, wanted.create_anchor)
            {
                // A changed `isolation` or `create_anchor` revokes calls admitted under the old
                // scope, as a `control` change does.
                server.shared.set_agent_scope(wanted.isolation, wanted.create_anchor);
            }
            server.config = wanted;
            tracing::info!(
                target: "mcp",
                control = wanted.control.as_str(),
                read = wanted.read().as_str(),
                isolation = wanted.isolation.as_str(),
                create_anchor = wanted.create_anchor.as_str(),
                "control level changed"
            );
            return;
        }
        let shared = McpShared::new(wanted.control, self.event_loop_proxy.clone());
        shared.set_read(wanted.read());
        shared.set_agent_scope(wanted.isolation, wanted.create_anchor);
        shared.set_connection_limit(wanted.max_connections());
        match super::start_listener(&shared) {
            Ok((endpoint, guard)) => {
                tracing::info!(target: "mcp", control = wanted.control.as_str(), "control server started");
                let owner = self.register_mcp_owner();
                self.services.mcp_server = Some(McpServer {
                    shared,
                    endpoint,
                    _endpoint_guard: Some(guard),
                    config: wanted,
                    charges: HashMap::new(),
                    owner,
                    read_notice_due: wanted.read.is_none(),
                });
                if wanted.read.is_none() {
                    // The default changed under the user: say so in the log now and in a
                    // notification once a window can show it.
                    tracing::warn!(target: "mcp", "{READ_UNSET_NOTICE}");
                }
                self.show_mcp_read_notice_if_due();
            }
            Err(error) => {
                tracing::warn!(target: "mcp", %error, "control server could not start");
            }
        }
    }

    /// Show [`READ_UNSET_NOTICE`] once a main window exists to show it, if the running server
    /// started without `[mcp] read`. Called when the server starts and when the first main window
    /// appears, so a server started before any window still tells the user.
    pub(crate) fn show_mcp_read_notice_if_due(&mut self) {
        if self.registry.main_window_id.is_none() {
            // When: no `main_window_id` yet, the notice waits for the first main window.
            return;
        }
        let Some(server) = self.services.mcp_server.as_mut() else {
            // When: no `mcp_server` runs, there is nothing to announce.
            return;
        };
        if !std::mem::take(&mut server.read_notice_due) {
            // When: `read_notice_due` was not set or was already taken, nothing more is said.
            return;
        }
        self.show_notification_for_kind(
            self.frontmost_kind(),
            sonicterm_ui::overlays::NotificationLevel::Info,
            READ_UNSET_NOTICE.to_string(),
        );
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
        if let Some(shared) =
            self.services.mcp_server.as_ref().map(|server| Arc::clone(&server.shared))
        {
            self.prune_resume_tokens(&shared.sessions.tokens);
        }
        let Some(server) = self.services.mcp_server.as_mut() else {
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
        self.services.mcp_server.as_ref().map(|server| server.endpoint.as_str())
    }

    /// Answer every request connection threads queued for the event loop.
    pub(crate) fn drain_mcp_requests(&mut self) {
        let Some(shared) =
            self.services.mcp_server.as_ref().map(|server| Arc::clone(&server.shared))
        else {
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
        let access = shared.access(request.session);
        // Read tools see only the panes the session's `"agent"` scope reaches under
        // `read = "agent"`; the creator is SonicTerm's own record, so nothing a client sends widens it.
        let readable = |entry: &PaneEntry| access.reads(shared.read(), entry.creator);
        match &request.call {
            LoopCall::ListPanes => {
                let mut entries = self.mcp_pane_entries();
                entries.retain(readable);
                LoopReply::Panes(entries)
            }
            LoopCall::Notice { text } => {
                self.show_notification_for_kind(
                    self.frontmost_kind(),
                    sonicterm_ui::overlays::NotificationLevel::Info,
                    text.clone(),
                );
                LoopReply::Panes(Vec::new())
            }
            LoopCall::CreatePane { anchor, placement, launch } => {
                self.mcp_create_pane(access, *anchor, *placement, launch.clone())
            }
            LoopCall::SendText { pane_id, text, submit } => {
                self.mcp_send_text(access, *pane_id, text, *submit)
            }
            LoopCall::SendKeys { pane_id, keys } => self.mcp_send_keys(access, *pane_id, keys),
            LoopCall::RunStart { pane_id, command } => {
                self.mcp_run_start(access, *pane_id, command)
            }
            LoopCall::ClosePane { pane_id } => self.mcp_close_pane(access, *pane_id),
            LoopCall::ClaimPanes { resume, own } => {
                self.mcp_claim_panes(&shared.sessions.tokens, access, resume, own)
            }
            LoopCall::ResolvePane { pane_id } => match self.mcp_resolve_pane(*pane_id) {
                Some(entry) if readable(&entry) => LoopReply::Pane(entry),
                Some(_) => LoopReply::Refused(
                    "out_of_scope",
                    format!(
                        "pane {pane_id} is outside [mcp] read = \"agent\" (the default); set \
                         read = \"any\" to let agents read every pane"
                    ),
                ),
                None => LoopReply::Refused("pane_gone", format!("no pane {pane_id}")),
            },
        }
    }

    /// Every window in the order agents see them: main first, then children in creation order.
    /// A pane's `window_index` is its window's position here, for the list and for one pane alike.
    fn mcp_window_order(&self) -> Vec<WindowId> {
        #[cfg(test)]
        WINDOW_ORDERS.with(|orders| orders.set(orders.get() + 1));
        let mut windows: Vec<WindowId> = self.registry.windows.keys().copied().collect();
        windows.sort_by_key(|id| {
            let is_main = Some(*id) == self.registry.main_window_id;
            (!is_main, self.window_key(*id).map(|key| key.0))
        });
        windows
    }

    /// Every pane in every window, main first, then children in creation order. `list_panes` uses
    /// it; a tool that names one pane uses [`Self::mcp_resolve_pane`] instead.
    pub(crate) fn mcp_pane_entries(&self) -> Vec<PaneEntry> {
        let mut entries = Vec::new();
        for (window_index, window_id) in self.mcp_window_order().into_iter().enumerate() {
            let window = &self.registry.windows[&window_id];
            if window.role.is_report() {
                // When: the window `role` is a report window, it holds no terminal panes.
                continue;
            }
            for (tab_index, tab_state) in window.tab_states.iter().enumerate() {
                tab_state.tree.for_each_leaf(&mut |pane_id| {
                    entries.extend(Self::mcp_entry(
                        window,
                        Some((window_index, tab_index)),
                        pane_id,
                    ));
                });
            }
        }
        entries
    }

    /// The handles for `pane_id` alone, without its UI position: one window is found, no window
    /// is ordered, and only this pane's handles are cloned. `None` when no terminal window holds
    /// the pane or it is mid-teardown.
    pub(crate) fn mcp_resolve_pane(&self, pane_id: u64) -> Option<PaneEntry> {
        let window = &self.registry.windows[&self.registry.window_of_pane(pane_id)?];
        if window.role.is_report() {
            // When: the `window` holding the pane is a report window, it is no terminal pane.
            return None;
        }
        Self::mcp_entry(window, None, pane_id)
    }

    /// The UI position `list_panes` would report for `pane_id`, for tests that check both paths
    /// agree.
    #[cfg(test)]
    pub(crate) fn mcp_pane_metadata(&self, pane_id: u64) -> Option<PaneMetadata> {
        let slot = self.registry.locate_pane(pane_id)?;
        let window = &self.registry.windows[&slot.window_id];
        if window.role.is_report() {
            // When: the `window` holding the pane is a report window, it is no terminal pane.
            return None;
        }
        let window_index = self.mcp_window_order().iter().position(|id| *id == slot.window_id)?;
        Self::mcp_entry(window, Some((window_index, slot.tab_index)), pane_id)?.metadata
    }

    /// One pane's entry, cloning only its own shared handles; `None` when `pane_id` is a leaf
    /// whose pane is already gone, as during teardown.
    fn mcp_entry(
        window: &crate::app::WindowState,
        position: Option<(usize, usize)>,
        pane_id: u64,
    ) -> Option<PaneEntry> {
        let pane = window.panes.get(&pane_id)?;
        let metadata = match position {
            Some((window_index, tab_index)) => {
                let tab_state = window.tab_states.get(tab_index)?;
                Some(PaneMetadata {
                    window_index,
                    tab_index,
                    active: tab_index == window.tabs.active_index()
                        && tab_state.active_pane == pane_id,
                    readonly: window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only),
                })
            }
            None => None,
        };
        #[cfg(test)]
        ENTRIES_BUILT.with(|built| built.set(built.get() + 1));
        Some(PaneEntry {
            pane_id,
            metadata,
            creator: pane.creator,
            parser: Arc::clone(&pane.parser),
            output_generation: Arc::clone(&pane.output_generation),
            exit_status: Arc::clone(&pane.exit_status),
            change_signal: Arc::clone(&pane.change_signal),
            run_typed_at: Arc::clone(&pane.run_typed_at),
            agent_run: Arc::clone(&pane.agent_run),
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Entries built on this thread, so a test can tell resolving one pane from listing them all.
    pub(crate) static ENTRIES_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Window orderings computed on this thread, so a test can tell that resolving one pane ordered
    /// no windows.
    pub(crate) static WINDOW_ORDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
