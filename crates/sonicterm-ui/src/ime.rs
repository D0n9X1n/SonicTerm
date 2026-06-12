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

/// Throttle for `Window::set_ime_cursor_area` calls.
///
/// macOS' InputMethodKit logs `error messaging the mach port for
/// IMKCFRunLoopWakeUpReliable` whenever the host hammers the IME cursor
/// area faster than the IMK runloop can drain its wake messages. The
/// terminal renders every frame the cursor blinks or new bytes arrive,
/// but the IME candidate window only needs to know the cell position
/// when it actually changes. Track the last reported (row, col) and
/// gate the winit call on a real move.
///
/// Render-agnostic — used by `app::App` to decide whether to call
/// `set_ime_cursor_area`. Kept here (next to the rest of the IME state)
/// so the unit test lives beside the state machine without dragging in
/// a winit dependency.
#[derive(Debug, Default, Clone)]
pub struct ImeCursorThrottle {
    last: Option<(u16, u16)>,
}

impl ImeCursorThrottle {
    /// Construct a throttle with no recorded position. The first call to
    /// [`Self::should_update`] always returns `true` so the IME learns
    /// the initial cursor location.
    #[must_use]
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Returns `true` if the (row, col) differs from the last accepted
    /// position. Records the new position on `true`. Callers must only
    /// invoke the underlying winit `set_ime_cursor_area` when this
    /// returns `true`.
    pub fn should_update(&mut self, row: u16, col: u16) -> bool {
        if self.last == Some((row, col)) {
            return false;
        }
        self.last = Some((row, col));
        true
    }

    /// Clear the recorded position so the next call always fires. Used
    /// when the surface geometry changes (resize / DPI / font size) and
    /// the IME needs to re-learn the cell position even though the
    /// (row, col) integer pair is unchanged.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

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

    /// Cancel an in-flight composition (Esc pressed, or focus lost). Drops
    /// the preedit WITHOUT promoting it to the commit buffer, so no bytes
    /// reach the PTY. Idempotent.
    pub fn cancel(&mut self) {
        self.preedit.clear();
        self.cursor = None;
        self.composing = false;
        // Note: commit_buffer is left intact — a host may have already
        // received a commit it hasn't drained yet, and cancel must not
        // eat that.
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

#[cfg(test)]
#[path = "ime/tests.rs"]
mod tests;
