//! The work a connection thread asks the event loop to do, and the loop's answer: the payload of
//! every [`super::LoopRequest`].

use super::{claims, keys, run, PaneEntry};

/// Work only the event loop may do, with the reply it owes.
pub(crate) enum LoopCall {
    /// Every pane in every window, with the handles a read needs.
    ListPanes,
    /// One pane's read handles, if it exists.
    ResolvePane {
        /// The pane to resolve.
        pane_id: u64,
    },
    /// Show `text` in the existing in-app notification; nobody waits for the reply.
    Notice {
        /// The sanitized, bounded message.
        text: String,
    },
    /// Open a helper pane beside `anchor`.
    CreatePane {
        /// The pane the helper opens beside.
        anchor: u64,
        /// A split or a new tab.
        placement: crate::app::pane_target::Placement,
        /// What the helper runs.
        launch: crate::app::pane_target::AgentLaunch,
    },
    /// Type `text` into a pane, then Enter when `submit` is set.
    SendText {
        /// The target pane.
        pane_id: u64,
        /// The text, at most 64 KiB.
        text: String,
        /// Whether to add `\r`.
        submit: bool,
    },
    /// Send parsed keys to a pane.
    SendKeys {
        /// The target pane.
        pane_id: u64,
        /// The keys, already parsed.
        keys: Vec<keys::KeySpec>,
    },
    /// Start a command at a pane's shell prompt.
    RunStart {
        /// The target pane.
        pane_id: u64,
        /// The command line, without control characters.
        command: String,
    },
    /// Close one pane.
    ClosePane {
        /// The pane to close.
        pane_id: u64,
    },
    /// Select a pane's tab and the pane within it, raising its window only while SonicTerm is active.
    FocusPane {
        /// The pane to bring forward.
        pane_id: u64,
    },
    /// Set or clear the custom title of a pane's tab.
    SetTitle {
        /// A pane in the tab to retitle.
        pane_id: u64,
        /// The title, already free of control and format characters and capped; blank clears.
        title: String,
    },
    /// Show an agent's message, under SonicTerm's own prefix, in the in-app notification.
    Notify {
        /// The pane the message is about, which the session must be able to control.
        pane_id: u64,
        /// The message, already free of control and format characters and capped.
        text: String,
        /// The message's place in the notice queue, freed when the request is dropped; held only
        /// for its drop.
        _slot: super::control_loop_ui::NoticeSlot,
    },
    /// Move a closed session's panes to the caller.
    ClaimPanes {
        /// The closed session's token.
        resume: claims::ResumeToken,
        /// The caller's own token, proving it received it.
        own: claims::ResumeToken,
    },
}

/// What the loop answered.
pub(crate) enum LoopReply {
    /// The panes [`LoopCall::ListPanes`] found.
    Panes(Vec<PaneEntry>),
    /// The pane [`LoopCall::ResolvePane`] found.
    Pane(PaneEntry),
    /// The request was refused, with a stable code and a message.
    Refused(&'static str, String),
    /// [`LoopCall::CreatePane`] opened this pane.
    Created(crate::app::pane_target::CreatedPane),
    /// A write queued this many bytes; `receipt` is its delivery ticket and the pane's probe, or
    /// `None` for a pane without a PTY.
    Written(usize, Option<(u64, sonicterm_io::pty::PtyDeliveryProbe)>),
    /// [`LoopCall::RunStart`] started a run.
    RunStarted(Box<run::RunStarted>),
    /// [`LoopCall::ClosePane`] closed the pane.
    Closed,
    /// [`LoopCall::FocusPane`] selected the pane; `raised` when its window was asked to take focus.
    Focused {
        /// Whether the window was asked to take focus, not only to request attention.
        raised: bool,
    },
    /// [`LoopCall::SetTitle`] set this title, or cleared it (`None`).
    Titled(Option<String>),
    /// [`LoopCall::Notify`] showed the message.
    Notified,
    /// [`LoopCall::ClaimPanes`] moved this many panes.
    Claimed(usize),
}
