//! IME (Input Method Editor) composition state.
//!
//! Owns the state machine for CJK and other multi-key input methods that
//! send a stream of `Preedit` events (the in-progress, not-yet-committed
//! text shown by the OS IME panel) followed by zero or one `Commit` events
//! (the finalized text the user picked).
//!
//! This module is intentionally render-agnostic: a future PR can pull
//! [`ImeState::preedit`] / [`ImeState::cursor`] to paint a composition
//! overlay. Today the consumer is just `app::App`, which feeds the
//! drained commits straight into the active pane's PTY.

/// Pure state machine driven by `winit::event::Ime` events.
#[derive(Debug, Default, Clone)]
pub struct ImeState {
    /// True between an IME `Enabled` event and `Disabled`, OR while a
    /// non-empty preedit string is in flight. While true, callers should
    /// suppress regular `KeyboardInput` character forwarding so the
    /// composition isn't double-typed.
    composing: bool,
    /// The current preedit string from the IME. Empty when not composing.
    preedit: String,
    /// Optional (start, end) byte cursor inside `preedit`, as reported by
    /// the OS IME.
    cursor: Option<(usize, usize)>,
    /// Accumulates committed strings until the host drains them via
    /// [`Self::take_commits`].
    commit_buffer: String,
}

impl ImeState {
    /// Construct an empty, non-composing state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Called for `Ime::Enabled`.
    pub fn handle_enabled(&mut self) {
        // Enabled by itself does not mean "actively composing" — that only
        // begins once a non-empty Preedit arrives. But we treat the IME
        // session as live so callers can decide policy.
        self.preedit.clear();
        self.cursor = None;
    }

    /// Called for `Ime::Disabled`. Clears the preedit; does not touch the
    /// commit buffer (the host may not have drained yet).
    pub fn handle_disabled(&mut self) {
        self.composing = false;
        self.preedit.clear();
        self.cursor = None;
    }

    /// Called for `Ime::Preedit { text, cursor }`. An empty `text` ends
    /// the composition (the IME panel was closed without a commit).
    pub fn handle_preedit(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        self.preedit.clear();
        self.preedit.push_str(text);
        self.cursor = cursor;
        self.composing = !self.preedit.is_empty();
    }

    /// Called for `Ime::Commit { text }`. Appends to the commit buffer and
    /// ends the composition; the host should call [`Self::take_commits`]
    /// to forward the bytes to the PTY.
    pub fn handle_commit(&mut self, text: &str) {
        self.commit_buffer.push_str(text);
        self.preedit.clear();
        self.cursor = None;
        self.composing = false;
    }

    /// Drain the pending committed text. Returns an empty string if there
    /// is nothing to send.
    #[must_use]
    pub fn take_commits(&mut self) -> String {
        std::mem::take(&mut self.commit_buffer)
    }

    /// True while a non-empty preedit is in flight. The host should
    /// ignore regular `KeyboardInput` text events while this is true so
    /// the in-flight composition isn't typed twice.
    #[must_use]
    pub fn is_composing(&self) -> bool {
        self.composing
    }

    /// Read-only access to the current preedit string (for a future
    /// composition overlay).
    #[must_use]
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// Read-only access to the preedit cursor.
    #[must_use]
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }
}
