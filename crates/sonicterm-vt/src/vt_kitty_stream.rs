//! The kitty graphics APC state machine: bounded control sections, one staging reservation per
//! transfer, chunk reassembly across APCs, refusals and aborts, and the yield that keeps decode
//! replies in order.
//!
//! Every APC reads its control section into bounded scratch first, with no staging reservation.
//! Only a new image command (transmit or query) then reserves staging, for its payload alone; a
//! continuation chunk streams its payload into its transfer's existing reservation; a place,
//! delete, refused or malformed command reserves nothing. So header traffic can never exhaust
//! the staging pools, a full pool never loses a delete, and one transfer holds one reservation.

use super::*;

/// Longest control section kept; a longer one is refused as malformed, never cut and executed.
pub(super) const MAX_KITTY_HEADER_BYTES: usize = 512;

/// A chunked kitty transfer spanning several APCs.
#[derive(Debug)]
pub(super) enum KittyTransfer {
    /// Chunks are arriving; the capture holds the payload so far, without headers.
    Loading {
        /// The first chunk's control section, unchanged.
        metadata: String,
        /// The parsed first-chunk command; a later chunk's `q` updates its quiet level.
        command: KittyCommand,
        /// The transfer's single staging reservation and payload.
        capture: MediaCapture,
    },
    /// The transfer was refused or cancelled; later chunks are swallowed through `m=0`.
    Aborted {
        /// The first chunk's command, used for the final error reply.
        command: KittyCommand,
        /// Whether the client was already told why, so the final chunk adds no second error.
        reported: bool,
    },
}

/// What an APC does with the bytes after its control section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScratchPhase {
    /// Reading the control section.
    Header,
    /// Appending payload to the open transfer's capture.
    Continue,
    /// A new command: payload goes to its own capture when it carries an image.
    New,
    /// Swallowing the chunk of an aborted transfer; its end may finish that transfer.
    Swallow,
    /// Ignoring a malformed, refused or orphaned APC to its terminator.
    Ignore,
}

/// One kitty APC in flight: its bounded control section and, for a new image command, its
/// payload's staging.
#[derive(Debug)]
pub(super) struct ApcScratch {
    header: [u8; MAX_KITTY_HEADER_BYTES],
    header_len: usize,
    pub(super) phase: ScratchPhase,
    pub(super) pending_esc: bool,
    /// Bytes this APC has received, so a host sees a slow header or ignored tail still arriving.
    pub(super) seen: usize,
    /// The parsed control section once it ended.
    command: Option<KittyCommand>,
    /// A new image command's payload staging; `None` before the header ends or when refused.
    pub(super) payload: Option<MediaCapture>,
}

impl ApcScratch {
    fn new() -> Self {
        Self {
            header: [0; MAX_KITTY_HEADER_BYTES],
            header_len: 0,
            phase: ScratchPhase::Header,
            pending_esc: false,
            seen: 0,
            command: None,
            payload: None,
        }
    }

    /// The control section after `G`, or `None` when the APC is not a graphics command.
    fn control(&self) -> Option<&[u8]> {
        self.header[..self.header_len].strip_prefix(b"G")
    }
}

impl Performer {
    /// End an open upload because another media capture is about to reserve staging.
    ///
    /// Called before the Sixel or iTerm2 capture is created, so the upload's reservation is back
    /// in the pool first: one parser never holds two, and the new image is not refused for it.
    pub(super) fn abort_kitty_transfer_for_other_media(&mut self) {
        self.abort_kitty_transfer();
    }

    /// Turn an open upload into an aborted one whose later chunks are swallowed; an already
    /// aborted transfer keeps its state.
    pub(super) fn abort_kitty_transfer(&mut self) {
        if !matches!(self.kitty_transfer, Some(KittyTransfer::Loading { .. })) {
            // When: matches finds kitty_transfer is not Loading, no reservation is released and its state stays.
            return;
        }
        if let Some(KittyTransfer::Loading { command, .. }) = self.kitty_transfer.take() {
            self.kitty_transfer = Some(KittyTransfer::Aborted { command, reported: false });
        }
    }
}

impl Parser {
    /// Open a kitty APC. Nothing is reserved until its control section asks for a payload.
    pub(super) fn begin_apc(&mut self) {
        self.apc_scratch = Some(Box::new(ApcScratch::new()));
        self.escape_family = EscapeFamily::String;
        self.performer.fast_path_ready = false;
    }

    /// Feed one APC byte.
    pub(super) fn consume_apc_scratch_byte(&mut self, byte: u8) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, there is no APC to feed.
            return;
        };
        scratch.seen = scratch.seen.saturating_add(1);
        if scratch.pending_esc {
            // When: scratch has pending_esc, this byte either completes ST or both bytes are payload.
            scratch.pending_esc = false;
            if byte == b'\\' {
                // When: byte completes ST, the APC ends here.
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
                    // When: byte ends the control section, the APC's role is decided now.
                    self.end_scratch_header();
                    return;
                }
                if scratch.header_len == MAX_KITTY_HEADER_BYTES {
                    // When: header_len reached MAX_KITTY_HEADER_BYTES, the section is malformed; it is never cut and run.
                    self.refuse_overlong_header();
                    return;
                }
                scratch.header[scratch.header_len] = byte;
                scratch.header_len += 1;
            }
            ScratchPhase::Continue => {
                // When: phase is Continue, the byte is payload for the open transfer's one reservation.
                let Some(KittyTransfer::Loading { capture, .. }) =
                    self.performer.kitty_transfer.as_mut()
                else {
                    // When: kitty_transfer is no longer Loading, the chunk's payload has no owner.
                    return;
                };
                capture.append_byte(byte);
                // Past the payload cap, the transfer's staging is released now and the rest swallowed.
                if capture.truncated {
                    if let Some(KittyTransfer::Loading { command, .. }) =
                        self.performer.kitty_transfer.take()
                    {
                        self.reply_kitty_error(&command, "ENOMEM:image too large");
                        self.performer.kitty_transfer =
                            Some(KittyTransfer::Aborted { command, reported: true });
                    }
                    if let Some(scratch) = self.apc_scratch.as_mut() {
                        scratch.phase = ScratchPhase::Swallow;
                    }
                }
            }
            ScratchPhase::New => {
                // When: phase is New, the byte is the new command's payload, kept only for an image.
                let Some(payload) = scratch.payload.as_mut() else {
                    // When: payload is None, the command carries no image, so its bytes are ignored.
                    return;
                };
                payload.append_byte(byte);
                // Past the payload cap, the staging is released now and the command refused once.
                if payload.truncated || !payload.admitted() {
                    self.refuse_new_payload();
                }
            }
            ScratchPhase::Swallow | ScratchPhase::Ignore => {
                // When: phase is Swallow or Ignore, the byte is discarded without staging.
            }
        }
    }

    /// Refuse a control section longer than [`MAX_KITTY_HEADER_BYTES`]; ends any open upload.
    fn refuse_overlong_header(&mut self) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, there is nothing to refuse.
            return;
        };
        scratch.phase = ScratchPhase::Ignore;
        let target = KittyCommand::reply_target(scratch.control().unwrap_or(&[]));
        self.end_transfer_for_new_command(false);
        self.reply_kitty_error(&target, KittyCommandError::Invalid.reply_message());
    }

    /// Refuse a new image command whose payload could not be staged whole, releasing it now.
    fn refuse_new_payload(&mut self) {
        let Some(scratch) = self.apc_scratch.as_mut() else {
            // When: apc_scratch vanished, there is nothing to refuse.
            return;
        };
        scratch.payload = None;
        scratch.phase = ScratchPhase::Ignore;
        let Some(command) = scratch.command.clone() else {
            // When: command is None, the header never parsed, so no client can be answered.
            return;
        };
        self.reply_kitty_error(&command, "ENOMEM:image too large");
        // Later chunks of the refused transfer are swallowed through m=0 without a second error.
        if command.more {
            self.performer.kitty_transfer =
                Some(KittyTransfer::Aborted { command, reported: true });
        }
    }

    /// Apply a continuation's explicit `q` to its transfer, open or aborted.
    fn apply_chunk_quiet(&mut self, chunk: &KittyCommand) {
        if !chunk.quiet_set {
            // When: quiet_set is false, the chunk leaves the transfer's quiet level as it was.
            return;
        }
        match self.performer.kitty_transfer.as_mut() {
            Some(
                KittyTransfer::Loading { command, .. } | KittyTransfer::Aborted { command, .. },
            ) => {
                command.quiet = chunk.quiet;
            }
            None => {
                // When: kitty_transfer is None, there is no transfer for the chunk's q to govern.
            }
        }
    }

    /// Decide what an APC is once its control section ended at `;`.
    fn end_scratch_header(&mut self) {
        let Some(scratch) = self.apc_scratch.as_ref() else {
            // When: apc_scratch vanished, there is no header to end.
            return;
        };
        let Some(control) = scratch.control() else {
            // When: control is None, the APC is not a graphics command and is ignored to its end.
            self.set_scratch_phase(ScratchPhase::Ignore);
            return;
        };
        let parsed = KittyCommand::parse(control);
        let phase = match parsed {
            // A chunk carrying only m and q continues an open transfer, swallows an aborted one, or is an orphan.
            Ok(chunk) if chunk.continuation_only => {
                self.apply_chunk_quiet(&chunk);
                let phase = match self.performer.kitty_transfer {
                    Some(KittyTransfer::Loading { .. }) => ScratchPhase::Continue,
                    Some(KittyTransfer::Aborted { .. }) => ScratchPhase::Swallow,
                    None => ScratchPhase::Ignore,
                };
                self.set_scratch_command(chunk);
                phase
            }
            Ok(command) => self.begin_new_command(command),
            // A command that does not parse is refused, answered when it names an image, and never drawn.
            Err(error) => {
                tracing::debug!(?error, "kitty graphics command not accepted");
                let target = KittyCommand::reply_target(control);
                self.end_transfer_for_new_command(false);
                self.reply_kitty_error(&target, error.reply_message());
                ScratchPhase::Ignore
            }
        };
        self.set_scratch_phase(phase);
    }

    /// Start a parsed command that is not a continuation; returns how its payload is read.
    fn begin_new_command(&mut self, command: KittyCommand) -> ScratchPhase {
        self.end_transfer_for_new_command(command.action == KittyAction::Delete);
        if command.medium == KittyMedium::Refused {
            // When: medium is Refused, the payload names a file, temporary file or shm object and is never read.
            self.reply_kitty_error(&command, "ENOTSUPPORTED:only direct transmission is supported");
            // Later chunks of a refused transfer are swallowed through m=0, without a second error.
            if command.more {
                self.performer.kitty_transfer =
                    Some(KittyTransfer::Aborted { command, reported: true });
            }
            return ScratchPhase::Ignore;
        }
        if command.carries_image() {
            // When: carries_image, the payload reserves staging now, after any open upload was released.
            let payload = MediaCapture::new(
                &self.performer.staging_pool,
                MediaProtocol::Kitty,
                String::new(),
            );
            let admitted = payload.admitted();
            if let Some(scratch) = self.apc_scratch.as_mut() {
                scratch.payload = Some(payload);
            }
            self.set_scratch_command(command);
            if !admitted {
                // When: admitted is false, the pool is full: the command is refused once, now.
                self.refuse_new_payload();
                return ScratchPhase::Ignore;
            }
            return ScratchPhase::New;
        }
        self.set_scratch_command(command);
        ScratchPhase::New
    }

    fn set_scratch_phase(&mut self, phase: ScratchPhase) {
        if let Some(scratch) = self.apc_scratch.as_mut() {
            scratch.phase = phase;
        }
    }

    fn set_scratch_command(&mut self, command: KittyCommand) {
        if let Some(scratch) = self.apc_scratch.as_mut() {
            scratch.command = Some(command);
        }
    }

    /// End an open transfer because a different command arrived.
    fn end_transfer_for_new_command(&mut self, silent: bool) {
        if !matches!(self.performer.kitty_transfer, Some(KittyTransfer::Loading { .. })) {
            // When: matches finds kitty_transfer is not Loading, an aborted transfer is simply forgotten.
            self.performer.kitty_transfer = None;
            return;
        }
        if let Some(KittyTransfer::Loading { command, .. }) = self.performer.kitty_transfer.take() {
            // A non-delete command interrupting an upload reports the abandoned upload.
            if !silent {
                self.reply_kitty_error(&command, KittyCommandError::Invalid.reply_message());
            }
        }
    }

    /// End an APC at its terminator.
    fn finish_apc_scratch(&mut self) {
        // An APC still in its Header phase at ST had no `;`, so its whole body is the control section.
        if self.apc_scratch.as_ref().is_some_and(|scratch| scratch.phase == ScratchPhase::Header) {
            self.end_scratch_header();
        }
        let Some(scratch) = self.apc_scratch.take() else {
            // When: apc_scratch vanished, there is no APC to finish.
            return;
        };
        let ApcScratch { phase, command, payload, .. } = *scratch;
        let row = self.performer.grid.cursor.row;
        let col = self.performer.grid.cursor.col;
        match (phase, command) {
            // A continuation without more ends its transfer, which is dispatched or reported.
            (ScratchPhase::Continue | ScratchPhase::Swallow, Some(chunk)) if !chunk.more => {
                self.finish_transfer(row, col);
            }
            (ScratchPhase::New, Some(command)) => {
                self.finish_new_command(command, payload, row, col)
            }
            _ => {
                // When: phase is Ignore, or a middle chunk with more ended, nothing more happens at its end.
            }
        }
        self.finish_escape();
    }

    /// Complete the transfer a final chunk ended.
    fn finish_transfer(&mut self, row: u16, col: u16) {
        match self.performer.kitty_transfer.take() {
            Some(KittyTransfer::Loading { mut metadata, command, capture }) => {
                // The transfer's current quiet level, which later chunks may have changed, is the one the host replies with.
                metadata.push_str(&format!(",q={}", command.quiet));
                match capture.into_kitty_payload_event(metadata, row, col) {
                    Some(event) => self.dispatch_kitty_event(event, &command),
                    None => self.reply_kitty_error(&command, "ENOMEM:image too large"),
                }
            }
            // An aborted transfer reports once: here only if the client was never told why.
            Some(KittyTransfer::Aborted { command, reported }) if !reported => {
                self.reply_kitty_error(&command, "ECANCELED:transfer aborted");
            }
            Some(KittyTransfer::Aborted { .. }) => {
                // When: the Aborted transfer was already reported, its end adds no second error.
            }
            None => {
                // When: kitty_transfer is None, the chunk's transfer was already ended by another command.
            }
        }
    }

    /// Dispatch, or open as a transfer, a new command whose APC ended.
    fn finish_new_command(
        &mut self,
        command: KittyCommand,
        payload: Option<MediaCapture>,
        row: u16,
        col: u16,
    ) {
        let metadata = self.apc_metadata_of(&command);
        let Some(capture) = payload else {
            // When: payload is None, a place or delete carries no image and dispatches with no data.
            let event =
                MediaEvent { protocol: MediaProtocol::Kitty, row, col, metadata, data: Vec::new() };
            self.dispatch_kitty_event(event, &command);
            return;
        };
        if command.more {
            // When: more chunks follow, the first chunk opens the transfer holding its payload only.
            self.performer.kitty_transfer =
                Some(KittyTransfer::Loading { metadata, command, capture });
            return;
        }
        match capture.into_kitty_payload_event(metadata, row, col) {
            Some(event) => self.dispatch_kitty_event(event, &command),
            None => self.reply_kitty_error(&command, "ENOMEM:image too large"),
        }
    }

    /// A parsed command's control section as event metadata.
    fn apc_metadata_of(&self, command: &KittyCommand) -> String {
        command.control_text()
    }

    /// When CAN/SUB or a host cancel ends an APC in flight, its transfer is aborted so the later
    /// chunks are swallowed instead of starting a new image.
    pub(super) fn abort_in_flight_apc(&mut self) {
        let Some(scratch) = self.apc_scratch.as_ref() else {
            // When: apc_scratch is None, no APC is in flight.
            return;
        };
        match (scratch.phase, scratch.command.clone()) {
            // An in-flight first chunk of a chunked transfer leaves its tail to be swallowed.
            (ScratchPhase::New, Some(command)) if command.carries_image() && command.more => {
                self.performer.kitty_transfer =
                    Some(KittyTransfer::Aborted { command, reported: false });
            }
            (ScratchPhase::Continue, _) => self.performer.abort_kitty_transfer(),
            _ => {
                // When: the APC is a header, an ignored tail or a single command, no transfer is affected.
            }
        }
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
        self.performer.abort_kitty_transfer();
    }
}

#[cfg(test)]
#[path = "vt_kitty_stream_tests.rs"]
mod vt_kitty_stream_tests;
