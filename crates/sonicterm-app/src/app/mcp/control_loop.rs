//! The event-loop side of the control tools: scope checks, and the pane mutations only the loop
//! may make. Each call rechecks scope when it runs, not only when it was queued.

use std::sync::atomic::Ordering;

use sonicterm_cfg::config::McpControl;

use super::LoopReply;
use crate::app::input_dispatch::{wrap_paste, PtyInputSource};
use crate::app::keyboard_protocol::KeyboardSnapshot;
use crate::app::pane_target::{AgentLaunch, Placement, TargetError};
use crate::app::App;

/// Why a control tool was refused, with its stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `[mcp] control = "off"`.
    ControlOff,
    /// The pane is not the agent's and `control = "agent"`.
    OutOfScope,
    /// The pane's window is in READONLY.
    ReadOnly,
    /// No live pane has that id.
    PaneGone,
    /// The pane's input queue is full or holds queued input.
    InputBusy,
    /// Another agent run owns the pane, or its command is still running.
    Busy,
    /// The pane shows no shell integration (no OSC 133 B).
    NoShellIntegration,
    /// Something is typed at the prompt, or typing arrived while the previous command ran.
    DraftPresent,
    /// The window could not host a new pane.
    Unavailable,
    /// The pane is the main window's last tab.
    LastTab,
}

impl Refusal {
    /// The code a tool error carries.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::ControlOff => "control_off",
            Self::OutOfScope => "out_of_scope",
            Self::ReadOnly => "readonly",
            Self::PaneGone => "pane_gone",
            Self::InputBusy => "input_busy",
            Self::Busy => "busy",
            Self::NoShellIntegration => "no_shell_integration",
            Self::DraftPresent => "draft_present",
            Self::Unavailable => "unavailable",
            Self::LastTab => "last_tab",
        }
    }

    /// A one-line explanation for the agent.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::ControlOff => "control tools are off ([mcp] control = \"off\")",
            Self::OutOfScope => "the pane was not created by an agent; [mcp] control = \"agent\"",
            Self::ReadOnly => "the pane's window is in READONLY",
            Self::PaneGone => "no pane has that id",
            Self::InputBusy => "the pane's input queue is busy",
            Self::Busy => "an agent run owns the pane or its command is still running",
            Self::NoShellIntegration => "the pane shows no shell prompt marks (OSC 133)",
            Self::DraftPresent => {
                "text is typed at the prompt, or typing arrived during the last command"
            }
            Self::Unavailable => "the window cannot host a new pane now",
            Self::LastTab => "the pane is the main window's last tab",
        }
    }

    /// This refusal as the loop's reply.
    pub(crate) fn reply(self) -> LoopReply {
        LoopReply::Refused(self.code(), self.message().to_owned())
    }
}

impl From<TargetError> for Refusal {
    fn from(error: TargetError) -> Self {
        match error {
            TargetError::PaneGone => Self::PaneGone,
            TargetError::ReadOnly => Self::ReadOnly,
            TargetError::InputBusy => Self::InputBusy,
            TargetError::Unavailable => Self::Unavailable,
        }
    }
}

/// The C1 range and every C0 control: a `run_command` command must contain none.
pub(crate) fn has_control_character(command: &str) -> bool {
    command.chars().any(|character| character.is_control())
}

impl App {
    /// Whether an agent may act on `pane_id` under `control`. `anchor_only` allows any pane under
    /// `"agent"`, for `create_pane`, which never touches the anchor's input.
    pub(crate) fn authorize_control(
        &self,
        control: McpControl,
        pane_id: u64,
        anchor_only: bool,
    ) -> Result<(), Refusal> {
        if control == McpControl::Off {
            // When: `control` is `Off`, every control tool is refused.
            return Err(Refusal::ControlOff);
        }
        let Some(pane) = self.windows.values().find_map(|window| window.panes.get(&pane_id)) else {
            // When: no window holds `pane_id`, there is nothing to act on.
            return Err(Refusal::PaneGone);
        };
        if self.pane_window_read_only(pane_id) {
            // When: `pane_window_read_only` holds, READONLY refuses every agent action.
            return Err(Refusal::ReadOnly);
        }
        if control == McpControl::Agent && !pane.agent_owned && !anchor_only {
            // When: under `Agent` scope the pane is not `agent_owned`, it is the user's.
            return Err(Refusal::OutOfScope);
        }
        Ok(())
    }

    /// `create_pane` on the loop.
    pub(crate) fn mcp_create_pane(
        &mut self,
        control: McpControl,
        anchor: u64,
        placement: Placement,
        launch: AgentLaunch,
    ) -> LoopReply {
        if let Err(refusal) = self.authorize_control(control, anchor, true) {
            // When: `authorize_control` refused the anchor, no pane is created.
            return refusal.reply();
        }
        match self.create_pane_at(anchor, placement, launch) {
            Ok(created) => LoopReply::Created(created),
            Err(error) => Refusal::from(error).reply(),
        }
    }

    /// `send_text` on the loop: bracketed-paste-wrapped when the pane asked for it, then a `\r`
    /// when `submit` is set.
    pub(crate) fn mcp_send_text(
        &mut self,
        control: McpControl,
        pane_id: u64,
        text: &str,
        submit: bool,
    ) -> LoopReply {
        if let Err(refusal) = self.authorize_writable(control, pane_id) {
            // When: `authorize_writable` refused the pane, nothing is written.
            return refusal.reply();
        }
        let bracketed = self
            .windows
            .values()
            .find_map(|window| window.panes.get(&pane_id))
            .map(|pane| {
                crate::app::frame_counters::lock_parser(&pane.parser).bracketed_paste_enabled()
            })
            .unwrap_or(false);
        let mut bytes = wrap_paste(text, bracketed);
        if submit {
            bytes.push(b'\r');
        }
        self.mcp_write(pane_id, bytes)
    }

    /// `send_keys` on the loop: the pane's keyboard modes are read under its parser lock right
    /// before encoding.
    pub(crate) fn mcp_send_keys(
        &mut self,
        control: McpControl,
        pane_id: u64,
        keys: &[super::keys::KeySpec],
    ) -> LoopReply {
        if let Err(refusal) = self.authorize_writable(control, pane_id) {
            // When: `authorize_writable` refused the pane, nothing is written.
            return refusal.reply();
        }
        let Some(snapshot) =
            self.windows.values().find_map(|window| window.panes.get(&pane_id)).map(|pane| {
                KeyboardSnapshot::from_bits(
                    crate::app::frame_counters::lock_parser(&pane.parser).keyboard_input_snapshot(),
                )
            })
        else {
            // When: the pane vanished between the check and the read, nothing is written.
            return Refusal::PaneGone.reply();
        };
        match super::keys::encode_keys(keys, snapshot, cfg!(windows)) {
            Ok(bytes) => self.mcp_write(pane_id, bytes),
            Err(error) => LoopReply::Refused("invalid_arguments", format!("{error:?}")),
        }
    }

    /// Scope, then the pane's run reservation: an agent run on the pane refuses every other
    /// agent write.
    // Ordering: agent_run Acquire pairs with the run's AcqRel swap and Release clear.
    fn authorize_writable(&self, control: McpControl, pane_id: u64) -> Result<(), Refusal> {
        self.authorize_control(control, pane_id, false)?;
        let running = self
            .windows
            .values()
            .find_map(|window| window.panes.get(&pane_id))
            .is_some_and(|pane| pane.agent_run.load(Ordering::Acquire));
        if running {
            // When: an agent run owns the pane (`running`), other agent writes are refused.
            return Err(Refusal::Busy);
        }
        Ok(())
    }

    /// Queue `bytes` as agent typing and report how many were written.
    fn mcp_write(&mut self, pane_id: u64, bytes: Vec<u8>) -> LoopReply {
        match self.write_agent_input(pane_id, bytes, PtyInputSource::Agent) {
            Ok(written) => LoopReply::Written(written),
            Err(error) => Refusal::from(error).reply(),
        }
    }

    /// `close_pane` on the loop: one of several leaves closes alone, a sole leaf closes its tab
    /// (reaping an emptied child window), and the main window's last tab is refused.
    pub(crate) fn mcp_close_pane(&mut self, control: McpControl, pane_id: u64) -> LoopReply {
        if let Err(refusal) = self.authorize_control(control, pane_id, false) {
            // When: `authorize_control` refused the pane, nothing closes.
            return refusal.reply();
        }
        let Some(location) = self.locate_pane(pane_id) else {
            // When: `locate_pane` finds no tab for the pane, nothing closes.
            return Refusal::PaneGone.reply();
        };
        let Some(window) = self.windows.get(&location.window_id) else {
            // When: the pane's window is gone, nothing closes.
            return Refusal::PaneGone.reply();
        };
        let leaves =
            window.tab_states.get(location.tab_index).map_or(0, |tab| tab.tree.leaves().len());
        let last_tab = location.is_main && window.tab_states.len() == 1 && leaves == 1;
        if last_tab {
            // When: it is the main window's `last_tab`, closing it would quit SonicTerm.
            return Refusal::LastTab.reply();
        }
        if leaves > 1 {
            self.close_pty_pane(pane_id);
        } else if location.is_main {
            // When: the sole leaf `is_main`, its tab closes and an emptied main window is reaped.
            self.close_tab_at(location.tab_index);
            self.reap_empty_main_window_after_close();
        } else {
            // When: neither several `leaves` nor `is_main`, the child's tab closes and an emptied
            // child window reaps itself.
            self.close_tab_at_in_child(location.window_id, location.tab_index);
        }
        tracing::info!(target: "mcp", pane = pane_id, "agent closed pane");
        LoopReply::Closed
    }
}

#[cfg(test)]
#[path = "control_loop_tests.rs"]
mod control_loop_tests;

#[cfg(test)]
#[path = "control_loop_boundary_tests.rs"]
mod control_loop_boundary_tests;
