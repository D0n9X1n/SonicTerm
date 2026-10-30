use super::*;

impl EscapeFamily {
    fn from_ground(byte: u8) -> Self {
        // When: byte is ESC, begin parser-owned escape accounting; every other ground byte remains under vte's ground decoder.
        if byte == 0x1b {
            Self::Esc
        } else {
            Self::Ground
        }
    }

    fn after_byte(self, byte: u8) -> Self {
        match self {
            Self::Ground => Self::from_ground(byte),
            Self::Esc => match byte {
                b'[' => Self::Csi,
                b']' => Self::Osc,
                b'P' => Self::DcsEntry,
                b'X' | b'^' | b'_' => Self::String,
                0x1b => Self::Esc,
                0x18 | 0x1a | 0x30..=0x7e => Self::Ground,
                0x20..=0x2f => Self::EscIntermediate,
                _ => Self::Esc,
            },
            Self::EscIntermediate => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a | 0x30..=0x7e => Self::Ground,
                _ => Self::EscIntermediate,
            },
            Self::Csi => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a | 0x40..=0x7e => Self::Ground,
                _ => Self::Csi,
            },
            Self::Osc => match byte {
                0x1b => Self::Esc,
                0x07 | 0x18 | 0x1a => Self::Ground,
                _ => Self::Osc,
            },
            Self::DcsEntry => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a => Self::Ground,
                0x20..=0x2f => Self::DcsIntermediate,
                0x30..=0x3f => Self::DcsParam,
                0x40..=0x7e => Self::DcsPassthrough,
                _ => Self::DcsEntry,
            },
            Self::DcsParam => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a => Self::Ground,
                0x20..=0x2f => Self::DcsIntermediate,
                0x3c..=0x3f => Self::DcsIgnore,
                0x40..=0x7e => Self::DcsPassthrough,
                _ => Self::DcsParam,
            },
            Self::DcsIntermediate => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a => Self::Ground,
                0x30..=0x3f => Self::DcsIgnore,
                0x40..=0x7e => Self::DcsPassthrough,
                _ => Self::DcsIntermediate,
            },
            Self::DcsIgnore => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a => Self::Ground,
                _ => Self::DcsIgnore,
            },
            Self::DcsPassthrough => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a | 0x9c => Self::Ground,
                _ => Self::DcsPassthrough,
            },
            Self::String => match byte {
                0x1b => Self::Esc,
                0x18 | 0x1a => Self::Ground,
                _ => Self::String,
            },
        }
    }
}

impl Parser {
    /// Feed raw bytes from the pty. Drains any queued events for the caller.
    ///
    /// Implements an ASCII SWAR fast-path: while the underlying vte state
    /// machine is in the Ground state (no escape sequence in flight), we
    /// scan the input via `memchr` for the next byte that vte would actually
    /// need to dispatch (ESC `0x1B`, BEL `0x07`, or anything outside the
    /// `[0x20, 0x7E]` printable-ASCII range), bulk-print the safe ASCII run
    /// straight into the grid, and only hand the remainder to vte. Hot
    /// payloads like `cat largefile` are ~99 % printable ASCII, so this
    /// bypasses vte's byte-at-a-time state machine for the common case while
    /// keeping behaviour identical to feeding the whole slice through vte.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        self.advance_prefix(bytes).1
    }

    /// Consume through the next reply-producing byte; deliver returned replies outside parser locks before resuming the suffix.
    pub fn advance_with_replies(&mut self, bytes: &[u8]) -> (usize, Vec<VtEvent>, Vec<u8>) {
        *self.performer.captured_replies.get_mut() = Some(Vec::new());
        let (consumed, events) = self.advance_prefix(bytes);
        let replies = self.performer.captured_replies.get_mut().take().unwrap();
        (consumed, events, replies)
    }

    fn advance_prefix(&mut self, bytes: &[u8]) -> (usize, Vec<VtEvent>) {
        let mut offset = 0;
        let len = bytes.len();
        while offset < len {
            if self
                .performer
                .captured_replies
                .borrow()
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty())
            {
                // When: captured_replies is nonempty, yield before another byte can grow staging or overtake delivery.
                break;
            }
            if std::mem::take(&mut self.kitty_yield)
                && self.performer.captured_replies.borrow().is_some()
            {
                // When: a kitty event awaits the host's decode reply and the caller collects replies per prefix,
                // yield so that reply precedes later replies. Plain `advance` has no such boundary and keeps going.
                break;
            }
            if self.discarding_oversized_escape {
                // When: discarding_oversized_escape owns this byte, so feeding vte could print a cancelled payload into the grid.
                self.consume_discarded_escape_byte(bytes[offset]);
                offset += 1;
                continue;
            }
            if self.apc_scratch.is_some() {
                // When: apc_scratch is active, the byte belongs to a chunk of the open kitty transfer.
                if matches!(bytes[offset], 0x18 | 0x1a) {
                    // When: CAN or SUB cancels a chunk, the whole transfer is aborted, not just this APC.
                    self.abort_kitty_transfer();
                    self.reset_cancelled_escape();
                } else {
                    self.consume_apc_scratch_byte(bytes[offset]);
                }
                offset += 1;
                continue;
            }
            if self.apc_capture.is_some() {
                // When: apc_capture is active, so bytes belong to Kitty payload staging rather than ordinary terminal text.
                self.consume_apc_byte(bytes[offset]);
                offset += 1;
                continue;
            }
            if self.kitty_transfer.is_some() {
                // When: a kitty upload is open between chunks, a Sixel or iTerm2 capture that began ends it.
                self.yield_kitty_transfer_to_other_media();
            }
            if self.performer.dcs_capture.is_some() && matches!(bytes[offset], 0x18 | 0x1a) {
                // When: dcs_capture receives CAN or SUB, abort the sequence before its remaining bytes can become image data.
                self.inner = vte::Parser::new();
                self.reset_cancelled_escape();
                offset += 1;
                continue;
            }
            if self.performer.decrqss_awaiting_backslash && bytes[offset] != b'\\' {
                // Abandon the pending query before an unrelated OSC/APC can bypass vte.
                self.performer.clear_decrqss();
            }
            if self.consume_raw_osc_byte(bytes[offset]) {
                // When: consume_raw_osc_byte owns bytes[offset], bypass vte so its private OSC Vec cannot retain a second copy.
                offset += 1;
                continue;
            }
            if self.escape_family == EscapeFamily::Esc && self.pending_esc && bytes[offset] == b'_'
            {
                // When: escape_family is Esc, pending_esc is set, and bytes[offset] is underscore, reset vte before Kitty takes ownership.
                self.inner = vte::Parser::new();
                self.pending_esc = false;
                self.begin_apc();
                offset += 1;
                continue;
            }
            if self.escape_family == EscapeFamily::Ground
                && self.performer.fast_path_ready
                && bytes[offset..].starts_with(b"\x1b_")
            {
                // When: escape_family is Ground and ESC underscore is contiguous, Kitty capture owns the sequence before vte can swallow it.
                self.begin_apc();
                offset += 2;
                continue;
            }
            if self.escape_family == EscapeFamily::Ground
                && self.performer.fast_path_ready
                && bytes[offset] == 0x9f
            {
                // When: escape_family is Ground and byte is C1 APC, Kitty capture owns the sequence exactly like ESC underscore.
                self.begin_apc();
                offset += 1;
                continue;
            }
            // When: escape_family is Ground and performer permits it, choose the printable ASCII bypass rather than vte.
            if self.escape_family == EscapeFamily::Ground && self.performer.fast_path_ready {
                // memchr3 for ESC / BEL / LF — the three commonest break
                // bytes — gives us a cheap upper bound on the run length.
                // We then scalar-verify the prefix is entirely printable
                // [0x20, 0x7E]; the first non-printable byte ends the run.
                let upper =
                    memchr::memchr3(0x1B, 0x07, 0x0A, &bytes[offset..]).unwrap_or(len - offset);
                let mut run_end = 0;
                while run_end < upper {
                    let byte = bytes[offset + run_end];
                    if !(0x20..=0x7E).contains(&byte) {
                        // When: byte is not printable ASCII, stop before the fast path bypasses vte control or UTF-8 decoding.
                        break;
                    }
                    run_end += 1;
                }
                // When: run_end covers printable ASCII, so direct graphic dispatch preserves the same cells while skipping vte.
                if run_end > 0 {
                    for &byte in &bytes[offset..offset + run_end] {
                        self.performer.print_graphic(byte as char);
                    }
                    offset += run_end;
                    continue;
                }
            }
            self.feed_vte_byte(bytes[offset]);
            offset += 1;
        }
        (offset, std::mem::take(&mut self.performer.events))
    }

    pub(super) fn feed_vte_byte(&mut self, byte: u8) {
        if self.performer.dcs_capture.is_some() && matches!(byte, 0x18 | 0x1a) {
            // When: dcs_capture receives CAN or SUB, reset vte before cancelled image bytes can survive to unhook.
            self.inner = vte::Parser::new();
            self.reset_cancelled_escape();
            return;
        }
        let started_escape = self.escape_family == EscapeFamily::Ground
            && EscapeFamily::from_ground(byte) != EscapeFamily::Ground;
        if started_escape {
            self.escape_bytes_in_flight = 1;
        } else if self.escape_family != EscapeFamily::Ground && self.performer.dcs_capture.is_none()
        {
            // When: escape_family is non-Ground and dcs_capture is absent, enforce the generic retention cap.
            self.escape_bytes_in_flight = self.escape_bytes_in_flight.saturating_add(1);
            if self.escape_bytes_in_flight > MAX_ESCAPE_SEQUENCE_BYTES {
                // When: escape_bytes_in_flight exceeds MAX_ESCAPE_SEQUENCE_BYTES, discard through the sequence terminator.
                let pending_esc = self.pending_esc;
                self.begin_discarding_oversized_escape(pending_esc);
                self.consume_discarded_escape_byte(byte);
                return;
            }
        }
        self.performer.observe_decrqss_terminator(byte);
        self.observe_osc_byte(byte);
        let was_fast_path_ready = self.performer.fast_path_ready;
        self.performer.fast_path_ready = false;
        let previous_family = self.escape_family;
        self.inner.advance(&mut self.performer, &[byte]);
        self.escape_family = previous_family.after_byte(byte);
        if self.escape_family == EscapeFamily::Ground {
            self.escape_bytes_in_flight = 0;
            if previous_family != EscapeFamily::Ground || (was_fast_path_ready && byte < 0xc0) {
                self.performer.fast_path_ready = true;
            }
        }
        if matches!(byte, 0x18 | 0x1a) {
            self.reset_cancelled_escape();
        }
    }

    pub(super) fn reset_cancelled_escape(&mut self) {
        self.raw_osc = None;
        self.iterm2_metadata_len = 0;
        self.pending_esc = false;
        self.apc_capture = None;
        self.apc_scratch = None;
        self.performer.dcs_capture = None;
        self.performer.clear_decrqss();
        self.escape_bytes_in_flight = 0;
        self.discarding_oversized_escape = false;
        self.discard_escape_pending_esc = false;
        self.discard_exits_on_newline = false;
        self.escape_family = EscapeFamily::Ground;
        self.performer.fast_path_ready = true;
    }

    fn begin_discarding_oversized_escape(&mut self, pending_esc: bool) {
        tracing::warn!(
            limit = MAX_ESCAPE_SEQUENCE_BYTES,
            "escape sequence exceeded memory limit; discarding through terminator"
        );
        self.inner = vte::Parser::new();
        self.raw_osc = None;
        self.performer.dcs_capture = None;
        self.performer.clear_decrqss();
        self.pending_esc = false;
        self.discarding_oversized_escape = true;
        self.discard_escape_pending_esc = pending_esc;
        self.performer.fast_path_ready = false;
    }

    fn consume_discarded_escape_byte(&mut self, byte: u8) {
        let string_terminated = match self.escape_family {
            EscapeFamily::Osc => {
                byte == 0x07 || byte == 0x9c || (self.discard_escape_pending_esc && byte == b'\\')
            }
            EscapeFamily::DcsEntry
            | EscapeFamily::DcsParam
            | EscapeFamily::DcsIntermediate
            | EscapeFamily::DcsIgnore
            | EscapeFamily::DcsPassthrough
            | EscapeFamily::String => {
                byte == 0x9c || (self.discard_escape_pending_esc && byte == b'\\')
            }
            EscapeFamily::Ground
            | EscapeFamily::Esc
            | EscapeFamily::EscIntermediate
            | EscapeFamily::Csi => false,
        };
        let final_byte = match self.escape_family {
            EscapeFamily::Csi => (0x40..=0x7e).contains(&byte),
            EscapeFamily::Esc | EscapeFamily::EscIntermediate => (0x30..=0x7e).contains(&byte),
            EscapeFamily::Ground
            | EscapeFamily::Osc
            | EscapeFamily::DcsEntry
            | EscapeFamily::DcsParam
            | EscapeFamily::DcsIntermediate
            | EscapeFamily::DcsIgnore
            | EscapeFamily::DcsPassthrough
            | EscapeFamily::String => false,
        };
        let abandoned_by_newline = self.discard_exits_on_newline && byte == b'\n';
        let terminated =
            string_terminated || final_byte || abandoned_by_newline || matches!(byte, 0x18 | 0x1a);
        if terminated {
            // When: terminated marks the escape boundary, so parser ground may resume without exposing discarded payload bytes.
            self.discarding_oversized_escape = false;
            self.discard_escape_pending_esc = false;
            self.discard_exits_on_newline = false;
            self.escape_bytes_in_flight = 0;
            self.performer.fast_path_ready = true;
            self.escape_family = EscapeFamily::Ground;
            // A newline is the user's output, not the transfer's terminator,
            // so it has to reach the grid rather than be eaten as one.
            if abandoned_by_newline {
                self.performer.execute(b'\n');
            }
            return;
        }
        self.discard_escape_pending_esc = byte == 0x1b;
    }

    pub(super) fn consume_apc_byte(&mut self, byte: u8) {
        if matches!(byte, 0x18 | 0x1a) {
            // When: byte matches CAN or SUB, cancel APC staging before aborted Kitty data can become media.
            self.reset_cancelled_escape();
            return;
        }
        let Some(capture) = self.apc_capture.as_mut() else {
            // When: apc_capture vanished, ignore this byte rather than treating payload continuation as a new sequence.
            return;
        };
        if capture.pending_esc {
            // When: capture has a pending_esc, resolve whether byte completes ST before committing either byte to the payload.
            capture.pending_esc = false;
            if byte == b'\\' {
                // When: byte completes ST, dispatch the capture only after its whole-payload checks succeed.
                self.finish_staged_apc();
                return;
            }
            capture.append_byte(0x1b);
        }
        // When: byte selects C1 ST, ESC, or payload handling for this APC byte.
        if byte == 0x9c {
            self.finish_staged_apc();
        } else if byte == 0x1b {
            capture.pending_esc = true;
        } else {
            capture.append_byte(byte);
        }
    }

    pub(super) fn finish_escape(&mut self) {
        self.escape_bytes_in_flight = 0;
        self.iterm2_metadata_len = 0;
        self.escape_family = EscapeFamily::Ground;
        self.performer.fast_path_ready = true;
    }
}

#[cfg(test)]
#[path = "vt_stream_tests.rs"]
mod vt_stream_tests;
