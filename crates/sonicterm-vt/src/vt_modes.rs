use super::*;

impl PointerModes {
    const TRACKING_MASK: u8 = 0b11;
    const SGR: u8 = 1 << 2;
    const IS_ALT: u8 = 1 << 3;
    const APPLICATION_CURSOR: u8 = 1 << 4;

    /// Build an explicit pointer-mode snapshot for a router or test.
    pub const fn new(
        tracking: MouseTracking,
        sgr: bool,
        is_alt: bool,
        application_cursor: bool,
    ) -> Self {
        Self { tracking, sgr, is_alt, application_cursor }
    }

    /// Decode the byte [`Parser::pointer_input_snapshot`] produced; unknown high bits are ignored.
    pub fn from_bits(bits: u8) -> Self {
        let tracking = match bits & Self::TRACKING_MASK {
            0 => MouseTracking::Off,
            1 => MouseTracking::Button,
            2 => MouseTracking::ButtonMotion,
            _ => MouseTracking::AnyMotion,
        };
        Self {
            tracking,
            sgr: bits & Self::SGR != 0,
            is_alt: bits & Self::IS_ALT != 0,
            application_cursor: bits & Self::APPLICATION_CURSOR != 0,
        }
    }

    /// Pack the snapshot: tracking in bits 0-1, SGR bit 2, alternate screen bit 3, DECCKM bit 4.
    pub fn bits(self) -> u8 {
        let tracking = match self.tracking {
            MouseTracking::Off => 0,
            MouseTracking::Button => 1,
            MouseTracking::ButtonMotion => 2,
            MouseTracking::AnyMotion => 3,
        };
        tracking
            | (u8::from(self.sgr) * Self::SGR)
            | (u8::from(self.is_alt) * Self::IS_ALT)
            | (u8::from(self.application_cursor) * Self::APPLICATION_CURSOR)
    }

    /// DEC mouse tracking mode the application selected.
    pub fn tracking(self) -> MouseTracking {
        self.tracking
    }

    /// Whether reports use the SGR (?1006) encoding instead of the legacy one.
    pub fn sgr(self) -> bool {
        self.sgr
    }

    /// Whether the alternate screen is active.
    pub fn is_alt(self) -> bool {
        self.is_alt
    }

    /// Whether DECCKM application-cursor mode is active (wheel arrows use `ESC O`).
    pub fn application_cursor(self) -> bool {
        self.application_cursor
    }
}

impl KeyboardModes {
    const APPLICATION_CURSOR_KEYS: u8 = 1 << 0;
    const APPLICATION_KEYPAD: u8 = 1 << 1;
    const BACKARROW_KEY: u8 = 1 << 2;
    const NEWLINE: u8 = 1 << 3;
    const MODIFY_OTHER_KEYS_SHIFT: u8 = 4;
    const MODIFY_OTHER_KEYS_MASK: u8 = 0b11 << Self::MODIFY_OTHER_KEYS_SHIFT;
    const WIN32_INPUT: u8 = 1 << 6;

    /// Build an explicit keyboard-mode snapshot for an encoder or test.
    pub const fn new(
        application_cursor_keys: bool,
        application_keypad: bool,
        backarrow_key: bool,
        newline: bool,
        modify_other_keys: u8,
    ) -> Self {
        Self {
            application_cursor_keys,
            application_keypad,
            backarrow_key,
            newline,
            modify_other_keys: if modify_other_keys > 2 { 2 } else { modify_other_keys },
            win32_input: false,
        }
    }

    /// Set the DEC9001 request without changing other keyboard modes.
    pub const fn with_win32_input(mut self, enabled: bool) -> Self {
        self.win32_input = enabled;
        self
    }

    /// Whether DEC9001 requests Win32 input; nonzero Kitty flags still take precedence.
    pub fn win32_input(self) -> bool {
        self.win32_input
    }

    /// Reconstruct a mode snapshot from its compact cross-thread representation.
    pub fn from_bits(bits: u8) -> Self {
        Self {
            application_cursor_keys: bits & Self::APPLICATION_CURSOR_KEYS != 0,
            application_keypad: bits & Self::APPLICATION_KEYPAD != 0,
            backarrow_key: bits & Self::BACKARROW_KEY != 0,
            newline: bits & Self::NEWLINE != 0,
            modify_other_keys: ((bits & Self::MODIFY_OTHER_KEYS_MASK)
                >> Self::MODIFY_OTHER_KEYS_SHIFT)
                .min(2),
            win32_input: bits & Self::WIN32_INPUT != 0,
        }
    }

    /// Pack the mode snapshot into one byte for lock-free publication.
    pub fn bits(self) -> u8 {
        (u8::from(self.application_cursor_keys) * Self::APPLICATION_CURSOR_KEYS)
            | (u8::from(self.application_keypad) * Self::APPLICATION_KEYPAD)
            | (u8::from(self.backarrow_key) * Self::BACKARROW_KEY)
            | (u8::from(self.newline) * Self::NEWLINE)
            | self.modify_other_keys.min(2) << Self::MODIFY_OTHER_KEYS_SHIFT
            | (u8::from(self.win32_input) * Self::WIN32_INPUT)
    }

    /// Whether DECCKM application-cursor mode is active.
    pub fn application_cursor_keys(self) -> bool {
        self.application_cursor_keys
    }

    /// Whether DECKPAM application-keypad mode is active.
    pub fn application_keypad(self) -> bool {
        self.application_keypad
    }

    /// Whether DECBKM makes Backspace emit BS instead of DEL.
    pub fn backarrow_key(self) -> bool {
        self.backarrow_key
    }

    /// Whether ANSI newline mode makes Return emit CR LF.
    pub fn newline(self) -> bool {
        self.newline
    }

    /// Active xterm `modifyOtherKeys` level, clamped to `0..=2`.
    pub fn modify_other_keys(self) -> u8 {
        self.modify_other_keys
    }
}

impl Parser {
    /// Whether DECSET ?2004 (bracketed paste) is currently enabled.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.performer.bracketed_paste
    }

    /// Whether DECSET ?1006 (SGR mouse reporting) is currently enabled.
    pub fn mouse_sgr_enabled(&self) -> bool {
        self.performer.mouse_sgr
    }

    /// Whether DECCKM ?1 (application cursor keys) is currently enabled. When
    /// true, arrow-key sequences — including the synthetic ones SonicTerm
    /// emits for alt-screen wheel scroll — use the `ESC O A` form.
    pub fn application_cursor_keys(&self) -> bool {
        self.performer.app_cursor_keys
    }

    /// Return every active terminal mode that changes keyboard output encoding.
    pub fn keyboard_modes(&self) -> KeyboardModes {
        self.performer.keyboard_modes()
    }

    /// Count Win32 eligibility transitions and RIS; `(1 << 48) - 1` requires hosts to refuse Win32 input.
    pub fn keyboard_protocol_epoch(&self) -> u64 {
        self.performer.keyboard_protocol_epoch
    }

    /// Pack modes into bits 0..8, Kitty flags into 8..16, and the saturating epoch into 16..64.
    pub fn keyboard_input_snapshot(&self) -> u64 {
        u64::from(self.keyboard_modes().bits())
            | (u64::from(self.kitty_keyboard_flags()) << 8)
            | (self.keyboard_protocol_epoch() << 16)
    }

    /// Report synchronized output (DEC 2026): whether an update is open, its epoch, and the resets so far.
    pub fn synchronized_output(&self) -> SyncState {
        SyncState {
            set: self.performer.synchronized_output,
            epoch: self.performer.sync_epoch,
            resets: self.performer.sync_resets,
        }
    }

    /// Test-only: start synchronized output's epoch at `epoch`, so a test can reach an epoch boundary.
    #[doc(hidden)]
    pub fn __test_set_sync_epoch(&mut self, epoch: u64) {
        self.performer.sync_epoch = epoch;
    }

    /// Pack the pointer-routing modes into one byte; decode it with [`PointerModes::from_bits`].
    pub fn pointer_input_snapshot(&self) -> u8 {
        PointerModes::new(
            self.mouse_tracking(),
            self.mouse_sgr_enabled(),
            self.performer.grid.is_alt(),
            self.application_cursor_keys(),
        )
        .bits()
    }

    /// Return the current DEC mouse tracking mode selected by the application.
    pub fn mouse_tracking(&self) -> MouseTracking {
        self.performer.mouse_tracking
    }

    /// Active kitty keyboard protocol flags (the top of the progressive
    /// enhancement push/pop stack). `0` means no flags / legacy encoding.
    /// The host reads this to decide whether to emit CSI-u key encodings —
    /// e.g. Shift+Enter as `CSI 13 ; 2 u` when a TUI like Copilot CLI has
    /// pushed the disambiguate flag.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.performer.kitty_keyboard_flags()
    }
}

impl Performer {
    /// Active kitty keyboard protocol flags (top of the push/pop stack).
    /// Empty stack reports 0, meaning legacy (non-kitty) key encoding.
    pub(super) fn kitty_keyboard_flags(&self) -> u8 {
        *self.kitty_kbd_stack().last().unwrap_or(&0)
    }

    fn kitty_kbd_stack(&self) -> &Vec<u8> {
        &self.kitty_kbd_flags[usize::from(self.grid.is_alt())]
    }

    pub(super) fn kitty_kbd_stack_mut(&mut self) -> &mut Vec<u8> {
        let index = usize::from(self.grid.is_alt());
        &mut self.kitty_kbd_flags[index]
    }

    pub(super) fn keyboard_modes(&self) -> KeyboardModes {
        KeyboardModes {
            application_cursor_keys: self.app_cursor_keys,
            application_keypad: self.app_keypad,
            backarrow_key: self.backarrow_key,
            newline: self.newline_mode,
            modify_other_keys: self.modify_other_keys,
            win32_input: self.win32_input,
        }
    }

    pub(super) fn win32_input_eligible(&self) -> bool {
        self.win32_input && self.kitty_keyboard_flags() == 0
    }

    pub(super) fn advance_keyboard_protocol_epoch(&mut self) {
        self.keyboard_protocol_epoch =
            (self.keyboard_protocol_epoch + 1).min(KEYBOARD_PROTOCOL_EPOCH_MAX);
    }

    pub(super) fn record_win32_eligibility_transition(&mut self, before: bool) {
        // Intermediate eligibility changes invalidate held keys even when this batch restores the original mode.
        if before != self.win32_input_eligible() {
            self.advance_keyboard_protocol_epoch();
        }
    }

    /// Apply DEC 2026: a reset-to-set transition opens a new epoch, a set-to-reset one is counted.
    ///
    /// A repeated set keeps its epoch, so an application cannot renew its own hold by re-setting.
    pub(super) fn set_synchronized_output(&mut self, set: bool) {
        if set && !self.synchronized_output {
            // The mode opens from reset, so the update gets a fresh epoch.
            self.sync_epoch = self.sync_epoch.wrapping_add(1);
        } else if !set && self.synchronized_output {
            // When: `!set` while `synchronized_output` is set, an update ends; count it so no reset is missed.
            self.sync_resets = self.sync_resets.wrapping_add(1);
        }
        self.synchronized_output = set;
    }

    /// Handle a CSI sequence with `?` intermediate (DEC private modes).
    pub(super) fn handle_dec_private_mode(&mut self, params: &Params, set: bool) {
        self.reset_last_printed_char();
        for slice in params.iter() {
            let before = self.win32_input_eligible();
            let code = slice.first().copied().unwrap_or(0);
            match code {
                1 => self.app_cursor_keys = set,
                7 => self.grid.set_autowrap(set),
                67 => self.backarrow_key = set,
                25 => self.events.push(VtEvent::CursorVisibility(set)),
                47 => {
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        // When: set is false for mode 47, reveal the primary buffer without restoring a saved cursor.
                        self.grid.leave_alt_screen();
                    }
                    let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let set_or_reset = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?47{set_or_reset}: alt_screen_active={before}→{after}, cursor=({cursor_row},{cursor_col})"
                    );
                }
                1047 => {
                    // Same as ?47 — alt-screen switch WITHOUT cursor save/restore.
                    // Distinct from ?1049 (which also saves/restores the cursor)
                    // and from ?1048 (cursor save/restore only).
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        // When: set is false for mode 1047, reveal the primary buffer while leaving cursor ownership unchanged.
                        self.grid.leave_alt_screen();
                    }
                    let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let set_or_reset = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1047{set_or_reset}: alt_screen_active={before}→{after}, cursor=({cursor_row},{cursor_col})"
                    );
                }
                1048 => {
                    // Save / restore cursor only (DECSC / DECRC equivalent).
                    let before = self.grid.is_alt();
                    if set {
                        self.saved_cursor = Some(self.grid.cursor);
                    } else if let Some(saved) = self.saved_cursor {
                        // When: saved_cursor holds a saved position during reset, restore it without switching screen buffers.
                        self.grid.goto(saved.row, saved.col);
                    }
                    let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                    let set_or_reset = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1048{set_or_reset}: alt_screen_active={before}→{before}, cursor=({cursor_row},{cursor_col})"
                    );
                }
                1049 => {
                    let before = self.grid.is_alt();
                    if set {
                        // Guard against repeated ?1049h while already in alt
                        // screen — must not clobber the previously saved
                        // primary-screen cursor. xterm behaviour: second
                        // ?1049h is a no-op.
                        if !self.grid.is_alt() {
                            self.saved_cursor = Some(self.grid.cursor);
                            self.grid.enter_alt_screen();
                        }
                    } else {
                        // When: set is false for mode 1049, return to primary cells before restoring their saved cursor.
                        self.grid.leave_alt_screen();
                        if let Some(saved) = self.saved_cursor.take() {
                            self.grid.goto(saved.row, saved.col);
                        }
                    }
                    let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let set_or_reset = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1049{set_or_reset}: alt_screen_active={before}→{after}, cursor=({cursor_row},{cursor_col})"
                    );
                }
                9001 => self.win32_input = set,
                2004 => self.bracketed_paste = set,
                1006 => self.mouse_sgr = set,
                1000 | 1002 | 1003 => {
                    let mode = match code {
                        1000 => MouseTracking::Button,
                        1002 => MouseTracking::ButtonMotion,
                        1003 => MouseTracking::AnyMotion,
                        _ => unreachable!(),
                    };
                    if set {
                        self.mouse_tracking = mode;
                    } else if self.mouse_tracking == mode {
                        // When: DECRST names the selected mode, disable tracking without restoring an older selection.
                        self.mouse_tracking = MouseTracking::Off;
                    }
                }
                1004 => self.focus_reporting = set,
                2026 => self.set_synchronized_output(set),
                _ => {
                    // When: code is not implemented, preserve current terminal state so unknown private modes remain compatible.
                }
            }
            self.record_win32_eligibility_transition(before);
        }
    }
}

#[cfg(test)]
#[path = "vt_modes_tests.rs"]
mod vt_modes_tests;
