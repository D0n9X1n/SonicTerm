//! The kitty graphics APC state machine: one staging reservation per transfer, chunk
//! reassembly across APCs, refusals and aborts, and the yield that keeps decode replies in order.
//!
//! An APC opened while no transfer is open stages header and payload in one [`MediaCapture`], as
//! every media capture does. An APC opened while a chunked transfer is open (or aborted) reads its
//! header into bounded scratch instead, so a continuation chunk never claims a second reservation:
//! its payload streams straight into the transfer's capture.

use super::*;

/// Longest control section a continuation chunk may carry before it is treated as a new command.
const MAX_KITTY_HEADER_BYTES: usize = 512;

/// A chunked kitty transfer spanning several APCs.
#[derive(Debug)]
pub(super) enum KittyTransfer {
    /// Chunks are arriving; the capture holds the payload so far, without headers.
    Loading {
        /// The first chunk's control section, with a later `q=` appended.
        metadata: String,
        /// The parsed first-chunk command, used for replies.
        command: KittyCommand,
        /// The transfer's single staging reservation and payload.
        capture: MediaCapture,
    },
    /// The transfer was refused or cancelled; later chunks are swallowed through `m=0`.
    Aborted {
        /// The first chunk's command, used for the final error reply.
        command: KittyCommand,
    },
}

/// What a scratch-header APC does with its payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScratchPhase {
    /// Reading the control section into scratch.
    Header,
    /// Appending payload to the open transfer's capture.
    Continue,
    /// Discarding payload: the transfer is aborted.
    Swallow,
}

/// An APC opened while a transfer is open: its header lives in bounded scratch, not in staging.
#[derive(Debug)]
pub(super) struct ApcScratch {
    header: [u8; MAX_KITTY_HEADER_BYTES],
    header_len: usize,
    pub(super) phase: ScratchPhase,
    pub(super) pending_esc: bool,
    /// The continuation's own command, for its `m` and `q` keys.
    chunk: Option<KittyCommand>,
}

impl ApcScratch {
    fn new() -> Self {
        Self {
            header: [0; MAX_KITTY_HEADER_BYTES],
            header_len: 0,
            phase: ScratchPhase::Header,
            pending_esc: false,
            chunk: None,
        }
    }
}

/// The control section of a staged capture: the bytes after `G` up to `;` or the end.
fn staged_header(capture: &MediaCapture) -> Option<&[u8]> {
    let body = capture.data.strip_prefix(b"G")?;
    Some(body.iter().position(|byte| *byte == b';').map_or(body, |end| &body[..end]))
}

impl Parser {
    /// Open an APC: scratch while a transfer is open, otherwise one staging reservation.
    pub(super) fn begin_apc(&mut self) {
        // While a transfer is open this APC may be its continuation, so it must not claim a second reservation.
        if self.kitty_transfer.is_some() {
            self.apc_scratch = Some(Box::new(ApcScratch::new()));
        } else {
            // When: kitty_transfer is None, this APC stages its header and payload in one new reservation.
            self.apc_capture = Some(MediaCapture::new(
                &self.performer.staging_pool,
                MediaProtocol::Kitty,
                String::new(),
            ));
        }
        self.escape_family = EscapeFamily::String;
        self.performer.fast_path_ready = false;
    }

    /// Feed one byte of an APC opened with scratch.
    pub(super) fn consume_apc_scratch_byte(&mut self, byte: u8) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, there is no APC to feed.
            return;
        };
        if scratch.pending_esc {
            // When: scratch has pending_esc, this byte either completes ST or both bytes are payload.
            scratch.pending_esc = false;
            if byte == b'\\' {
                // When: byte completes ST, the chunk ends here.
                self.finish_apc_scratch();
                return;
            }
            self.apc_scratch_body_byte(0x1b);
        }
        match byte {
            0x9c => self.finish_apc_scratch(),
            0x1b => {
                if let Some(scratch) = self.apc_scratch.as_mut() {
                    scratch.pending_esc = true;
                }
            }
            _ => self.apc_scratch_body_byte(byte),
        }
    }

    fn apc_scratch_body_byte(&mut self, byte: u8) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, the byte has no owner.
            return;
        };
        match scratch.phase {
            ScratchPhase::Header => {
                // When: phase is Header, the byte belongs to the control section until `;` ends it.
                if byte == b';' {
                    // When: byte ends the control section, decide whether the chunk continues the transfer.
                    self.end_scratch_header();
                    return;
                }
                if scratch.header_len == MAX_KITTY_HEADER_BYTES {
                    // When: the header outgrew scratch, it is no continuation; stage it as a new command.
                    self.promote_scratch_to_staged(None);
                    self.consume_apc_byte(byte);
                    return;
                }
                scratch.header[scratch.header_len] = byte;
                scratch.header_len += 1;
            }
            ScratchPhase::Continue => {
                if let Some(KittyTransfer::Loading { capture, .. }) = self.kitty_transfer.as_mut() {
                    capture.append_byte(byte);
                }
            }
            ScratchPhase::Swallow => {
                // When: phase is Swallow, the transfer is aborted and its payload is discarded without staging.
            }
        }
    }

    /// Parse a scratch header and choose continuation, swallowing, or a new staged command.
    fn end_scratch_header(&mut self) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, there is no header to end.
            return;
        };
        let header = &scratch.header[..scratch.header_len];
        let parsed = header.strip_prefix(b"G").map(KittyCommand::parse);
        match (parsed, self.kitty_transfer.as_ref()) {
            (Some(Ok(chunk)), Some(KittyTransfer::Loading { .. })) if chunk.continuation_only => {
                scratch.phase = ScratchPhase::Continue;
                scratch.chunk = Some(chunk);
            }
            (Some(Ok(chunk)), Some(KittyTransfer::Aborted { .. })) if chunk.continuation_only => {
                scratch.phase = ScratchPhase::Swallow;
                scratch.chunk = Some(chunk);
            }
            (parsed, _) => {
                let silent =
                    matches!(&parsed, Some(Ok(command)) if command.action == KittyAction::Delete);
                self.promote_scratch_to_staged(Some(silent));
                if let Some(capture) = self.apc_capture.as_mut() {
                    capture.append_byte(b';');
                }
            }
        }
    }

    /// Turn a scratch APC into an ordinary staged one, ending the open transfer first.
    ///
    /// `silent` is `Some(true)` for a delete, which ends an upload without an error reply. The
    /// transfer is dropped before the new reservation is admitted, so the two never coexist.
    fn promote_scratch_to_staged(&mut self, silent: Option<bool>) {
        let Some(scratch) = self.apc_scratch.take() else {
            // When: apc_scratch vanished, there is nothing to promote.
            return;
        };
        self.end_transfer_for_new_command(silent.unwrap_or(false));
        let mut capture =
            MediaCapture::new(&self.performer.staging_pool, MediaProtocol::Kitty, String::new());
        for byte in &scratch.header[..scratch.header_len] {
            capture.append_byte(*byte);
        }
        capture.pending_esc = scratch.pending_esc;
        self.apc_capture = Some(capture);
    }

    /// End an open transfer because a different command arrived.
    fn end_transfer_for_new_command(&mut self, silent: bool) {
        if let Some(KittyTransfer::Loading { command, .. }) = self.kitty_transfer.take() {
            // A non-delete command interrupting an upload reports the abandoned upload.
            if !silent {
                if let Some(reply) = command.reply(KittyCommandError::Invalid.reply_message()) {
                    self.performer.reply(&reply);
                }
            }
        }
    }

    /// End a scratch APC at its terminator.
    fn finish_apc_scratch(&mut self) {
        let Some(scratch) = self.apc_scratch.as_ref() else {
            // When: apc_scratch vanished, there is no chunk to finish.
            return;
        };
        if scratch.phase == ScratchPhase::Header {
            // When: phase is still Header at ST, the APC had no `;`, so its whole body is the control section.
            let header = &scratch.header[..scratch.header_len];
            let parsed = header.strip_prefix(b"G").map(KittyCommand::parse);
            let continues = matches!(&parsed, Some(Ok(chunk)) if chunk.continuation_only);
            if !continues {
                // When: continues is false, the payload-less APC is a new command; stage it so it dispatches normally.
                let silent =
                    matches!(&parsed, Some(Ok(command)) if command.action == KittyAction::Delete);
                self.promote_scratch_to_staged(Some(silent));
                self.finish_staged_apc();
                return;
            }
            if let Some(scratch) = self.apc_scratch.as_mut() {
                scratch.chunk = parsed.and_then(Result::ok);
            }
        }
        let Some(scratch) = self.apc_scratch.take() else {
            // When: apc_scratch vanished, there is no chunk to finish.
            return;
        };
        let chunk = scratch.chunk.unwrap_or_default();
        if chunk.more {
            // When: more chunks follow, the transfer stays open.
            self.finish_escape();
            return;
        }
        match self.kitty_transfer.take() {
            Some(KittyTransfer::Loading { mut metadata, mut command, capture }) => {
                // A final chunk that sets q applies it to the whole transfer's reply, as kitty does.
                if chunk.quiet != 0 {
                    command.quiet = chunk.quiet;
                    metadata.push_str(&format!(",q={}", chunk.quiet));
                }
                let row = self.performer.grid.cursor.row;
                let col = self.performer.grid.cursor.col;
                match capture.into_kitty_payload_event(metadata, row, col) {
                    Some(event) => self.dispatch_kitty_event(event, &command),
                    None => self.reply_kitty_error(&command, "ENOMEM:image too large"),
                }
            }
            Some(KittyTransfer::Aborted { command }) => {
                self.reply_kitty_error(&command, "ECANCELED:transfer aborted");
            }
            None => {
                // When: kitty_transfer is None, the chunk's transfer was already ended by another command.
            }
        }
        self.finish_escape();
    }

    /// Finish an APC that staged its header and payload in one capture.
    pub(super) fn finish_staged_apc(&mut self) {
        let Some(mut capture) = self.apc_capture.take() else {
            // When: apc_capture vanished, there is nothing to dispatch.
            return;
        };
        let row = self.performer.grid.cursor.row;
        let col = self.performer.grid.cursor.col;
        let parsed = staged_header(&capture).map(KittyCommand::parse);
        match parsed {
            Some(Ok(command)) if command.medium == KittyMedium::Refused => {
                // The payload names a file, temporary file or shm object, so it is refused unread.
                self.reply_kitty_error(
                    &command,
                    "ENOTSUPPORTED:only direct transmission is supported",
                );
                // Later chunks of a refused transfer are swallowed through m=0.
                if command.more {
                    self.kitty_transfer = Some(KittyTransfer::Aborted { command });
                }
            }
            Some(Ok(command)) if command.carries_image() && command.more => {
                if !capture.admitted() || capture.truncated {
                    self.kitty_transfer = Some(KittyTransfer::Aborted { command });
                } else {
                    // When: capture is admitted and not truncated, the first chunk opens the transfer without its header.
                    let header_len = capture
                        .data
                        .iter()
                        .position(|byte| *byte == b';')
                        .map_or(capture.data.len(), |end| end + 1);
                    let metadata = String::from_utf8_lossy(&capture.data[1..header_len.max(1)])
                        .trim_end_matches(';')
                        .to_owned();
                    capture.data.drain(..header_len);
                    self.kitty_transfer =
                        Some(KittyTransfer::Loading { metadata, command, capture });
                }
            }
            Some(Ok(command)) => {
                if let Some(event) = capture.into_kitty_command_event(row, col) {
                    self.dispatch_kitty_event(event, &command);
                } else if command.carries_image() {
                    // When: a whole-payload command could not be staged, kitty reports it rather than drawing a fragment.
                    self.reply_kitty_error(&command, "ENOMEM:image too large");
                }
            }
            Some(Err(error)) => {
                // Malformed or unsupported: the legacy event keeps today's untyped dispatch, and the
                // host's decode refuses it. No reply is possible without a parsed image id.
                tracing::debug!(?error, "kitty graphics command not accepted");
                if let Some(event) = capture.into_kitty_event(row, col) {
                    self.performer.events.push(VtEvent::Media(event));
                }
            }
            None => {
                if let Some(event) = capture.into_kitty_event(row, col) {
                    self.performer.events.push(VtEvent::Media(event));
                }
            }
        }
        self.finish_escape();
    }

    /// Push a kitty event and, when the host owes a reply, end the advance prefix after it.
    ///
    /// Ending the prefix lets the host decode outside the parser lock and append its reply before
    /// any later query is parsed, so replies stay in stream order.
    fn dispatch_kitty_event(&mut self, event: MediaEvent, command: &KittyCommand) {
        self.performer.events.push(VtEvent::Media(event));
        let host_replies = matches!(
            command.action,
            KittyAction::Transmit
                | KittyAction::TransmitAndDisplay
                | KittyAction::Query
                | KittyAction::Place
        ) && command.image_id != 0
            && command.quiet < 2;
        self.kitty_yield |= host_replies;
    }

    fn reply_kitty_error(&self, command: &KittyCommand, message: &str) {
        if let Some(reply) = command.reply(message) {
            self.performer.reply(&reply);
        }
    }

    /// Abort an open transfer whose remaining chunks must still be swallowed.
    pub(super) fn abort_kitty_transfer(&mut self) {
        if let Some(KittyTransfer::Loading { command, .. }) = self.kitty_transfer.take() {
            self.kitty_transfer = Some(KittyTransfer::Aborted { command });
        }
    }

    /// End an open transfer silently because another media capture began.
    ///
    /// Only one capture may be live per parser; a Sixel or iTerm2 transfer starting between kitty
    /// chunks ends the kitty upload rather than holding a second reservation.
    pub(super) fn yield_kitty_transfer_to_other_media(&mut self) {
        let other_media = self.performer.dcs_capture.is_some()
            || matches!(self.raw_osc, Some(RawOsc::Iterm2 { .. }));
        // Another family's capture live while an upload is open makes the upload yield its reservation.
        if other_media && matches!(self.kitty_transfer, Some(KittyTransfer::Loading { .. })) {
            self.abort_kitty_transfer();
        }
    }
}

#[cfg(test)]
#[path = "vt_kitty_stream_tests.rs"]
mod vt_kitty_stream_tests;
