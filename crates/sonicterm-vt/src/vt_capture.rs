use super::*;

impl MediaCapture {
    pub(super) fn new(
        pool: &Arc<CaptureStagingPool>,
        protocol: MediaProtocol,
        metadata: String,
    ) -> Self {
        Self {
            protocol,
            metadata,
            data: Vec::new(),
            truncated: false,
            pending_esc: false,
            seen: 0,
            growth_exhausted: false,
            reservation: StagingReservation::admit(pool),
        }
    }

    /// Bytes this capture is holding, counting reserved capacity.
    ///
    /// Capacity rather than length because that is what the allocator is
    /// actually holding: a capture that grew and was truncated still owns the
    /// larger buffer.
    pub(super) fn retained_bytes(&self) -> usize {
        self.data.capacity().saturating_add(self.metadata.capacity())
    }

    pub(super) fn append_byte(&mut self, byte: u8) {
        self.seen = self.seen.saturating_add(1);

        if self.data.len() < self.reservation.budget() {
            // When: data still fits its reservation budget, so keeping byte preserves a complete payload without exceeding the pool.
            self.data.push(byte);
            return;
        }

        // Full. Ask the growth pool for a larger budget, once: a capture that
        // has been refused keeps receiving bytes, and retrying per byte would
        // put a contended atomic on the hot path of a transfer that is already
        // known to be capped.
        if !self.growth_exhausted && self.reservation.try_double() {
            // When: growth_exhausted is clear and reservation doubles, so byte can be kept without staging beyond the granted capacity.
            self.data.push(byte);
            return;
        }

        if !self.truncated {
            tracing::warn!(
                protocol = ?self.protocol,
                payload_bytes = self.seen,
                budget = self.reservation.budget(),
                cap = MAX_MEDIA_PAYLOAD_BYTES,
                "media payload exceeded staging limit; refusing whole image"
            );
        }
        self.growth_exhausted = true;
        self.truncated = true;
    }

    /// Whether this capture was admitted to the staging pools.
    ///
    /// A refused capture holds no staging and has kept no bytes, so it has
    /// nothing to dispatch.
    pub(super) fn admitted(&self) -> bool {
        self.reservation.admitted()
    }

    /// Whether this capture holds the whole payload it was offered.
    fn whole(&self) -> bool {
        self.admitted() && !self.truncated
    }

    /// Dispatch the payload, unless it is not the whole picture.
    ///
    /// A capture that was refused, or admitted and then cut, is dropped rather
    /// than surfaced, for the reason [`Parser::cancel_capture`] gives for
    /// discarding a partial one: a fragment of an image decodes to nothing
    /// useful, so surfacing it would trade memory for a broken picture instead
    /// of no picture.
    ///
    /// For Sixel the distinction is load-bearing rather than theoretical. Its
    /// decoder paints whatever bytes it is given and reports the bounding box
    /// of what it painted, so a cut payload decodes to a real image containing
    /// the top fraction of the picture — byte-identical to the whole one for
    /// as far as it goes, with nothing in it marking it incomplete. A user
    /// cannot tell that image from a complete one that happens to be shorter.
    pub(super) fn into_event(self, row: u16, col: u16) -> Option<MediaEvent> {
        if !self.whole() {
            // When: whole is false, dispatch would expose a refused capture or a silently truncated Sixel image.
            return None;
        }
        Some(MediaEvent {
            protocol: self.protocol,
            row,
            col,
            metadata: self.metadata,
            data: self.data,
        })
    }

    pub(super) fn into_iterm2_event(
        self,
        row: u16,
        col: u16,
        metadata: &[u8],
    ) -> Option<MediaEvent> {
        if !self.whole() {
            // When: whole is false, never allocate event metadata for a refused or truncated iTerm2 payload.
            return None;
        }
        let MediaCapture { protocol, data, reservation, .. } = self;
        drop(reservation);
        Some(MediaEvent {
            protocol,
            row,
            col,
            metadata: String::from_utf8_lossy(metadata).into_owned(),
            data,
        })
    }

    pub(super) fn into_kitty_event(mut self, row: u16, col: u16) -> Option<MediaEvent> {
        if !self.admitted() {
            // When: admitted is false, so the process pool refused this capture and no payload bytes are safe to dispatch.
            return None;
        }
        if self.pending_esc {
            self.append_byte(0x1b);
            self.pending_esc = false;
        }
        if !self.whole() {
            // When: whole is false after restoring pending ESC, so decoding would consume an incomplete Kitty payload.
            return None;
        }
        if self.data.first().copied() != Some(b'G') {
            // When: data lacks Kitty's G introducer, so treating the remaining bytes as graphics would misclassify an APC sequence.
            return None;
        }
        let payload = &self.data[1..];
        let (metadata, data) = split_once_byte(payload, b';')
            .map(|(metadata_bytes, data_bytes)| {
                (String::from_utf8_lossy(metadata_bytes).into_owned(), data_bytes.to_vec())
            })
            .unwrap_or_else(|| (String::new(), payload.to_vec()));
        Some(MediaEvent { protocol: MediaProtocol::Kitty, row, col, metadata, data })
    }

    /// Dispatch a kitty command whose control section parsed, splitting at the first `;`.
    ///
    /// Unlike [`Self::into_kitty_event`], an APC without `;` is all control section and no
    /// payload, which is what a payload-less `a=p` or `a=d` is.
    pub(super) fn into_kitty_command_event(mut self, row: u16, col: u16) -> Option<MediaEvent> {
        if !self.whole() || self.data.first().copied() != Some(b'G') {
            // When: the capture is not whole or lacks G, it cannot be a parsed kitty command.
            return None;
        }
        let header_end = self.data.iter().position(|byte| *byte == b';');
        let metadata_end = header_end.unwrap_or(self.data.len());
        let metadata = String::from_utf8_lossy(&self.data[1..metadata_end]).into_owned();
        self.data.drain(..header_end.map_or(self.data.len(), |end| end + 1));
        Some(MediaEvent { protocol: MediaProtocol::Kitty, row, col, metadata, data: self.data })
    }

    /// Dispatch a reassembled kitty transfer whose payload holds no headers.
    ///
    /// `None` when any chunk could not be staged whole, for the same reason a single capture is
    /// never dispatched in part.
    pub(super) fn into_kitty_payload_event(
        self,
        metadata: String,
        row: u16,
        col: u16,
    ) -> Option<MediaEvent> {
        if !self.whole() {
            // When: whole is false, some chunk was refused or cut, so the image would be a fragment.
            return None;
        }
        Some(MediaEvent { protocol: MediaProtocol::Kitty, row, col, metadata, data: self.data })
    }
}

/// Parse an iTerm2 `File=...` inline media payload.
///
/// Returns `None` for a payload larger than a capture may hold, rather than a
/// capped one: the bytes past the cap are the rest of the image, and an image
/// missing its tail decodes to nothing for a base64 protocol. Dispatching the
/// prefix would cost a decode that is guaranteed to fail.
pub(super) fn parse_iterm2_file_event(payload: &[u8], row: u16, col: u16) -> Option<MediaEvent> {
    if !payload.starts_with(b"File=") {
        // When: payload lacks the iTerm2 File prefix, do not reinterpret an unrelated OSC 1337 command as media.
        return None;
    }
    let (metadata, data) = split_once_byte(payload, b':')?;
    if data.len() > MAX_MEDIA_PAYLOAD_BYTES {
        // When: data exceeds MAX_MEDIA_PAYLOAD_BYTES, refuse the whole image rather than dispatching a partial base64 payload.
        tracing::warn!(
            payload_bytes = data.len(),
            cap = MAX_MEDIA_PAYLOAD_BYTES,
            "inline media payload refused: larger than a capture may hold"
        );
        return None;
    }
    Some(MediaEvent {
        protocol: MediaProtocol::Iterm2File,
        row,
        col,
        metadata: String::from_utf8_lossy(metadata).into_owned(),
        data: data.to_vec(),
    })
}

impl Parser {
    /// The capture an open kitty transfer holds between chunks, if any.
    fn kitty_transfer_capture(&self) -> Option<&MediaCapture> {
        match &self.kitty_transfer {
            Some(vt_kitty_stream::KittyTransfer::Loading { capture, .. }) => Some(capture),
            Some(vt_kitty_stream::KittyTransfer::Aborted { .. }) | None => None,
        }
    }

    /// Bytes and buffers this parser is holding mid-sequence.
    ///
    /// Covers the in-flight media capture, the raw OSC palette accumulator, and
    /// reserved capacity in both. Excludes the grid and the hyperlink registry,
    /// which report their own retention: a pane composes these figures rather
    /// than any one of them restating another.
    ///
    /// Items count live APC, DCS, or iTerm2 OSC capture buffers, which is at most
    /// one per parser — see [`Self::live_capture_count`].
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        let capture_bytes = self
            .apc_capture
            .iter()
            .chain(self.performer.dcs_capture.iter())
            .chain(self.kitty_transfer_capture())
            .map(MediaCapture::retained_bytes)
            .sum::<usize>();
        let osc_bytes = match &self.raw_osc {
            Some(RawOsc::Palette { content }) | Some(RawOsc::Semantic { content, .. }) => {
                content.capacity()
            }
            Some(RawOsc::Iterm2 { capture, .. }) => capture.retained_bytes(),
            Some(RawOsc::Probe { .. }) | None => 0,
        };
        ResourceAmount {
            bytes: capture_bytes.saturating_add(osc_bytes),
            items: self.live_capture_count(),
        }
    }

    /// Bytes received by the media capture currently in flight.
    ///
    /// Advances only while that capture is receiving, then returns to zero when
    /// it completes or is cancelled. A host samples this only while
    /// [`Self::live_capture_count`] is non-zero; equal consecutive values then
    /// distinguish a stalled capture from one that is still receiving.
    ///
    /// Paired with [`Parser::cancel_capture`], this is what lets a stalled
    /// transfer's staging be reclaimed. Neither half is useful alone: the
    /// parser cannot decide *when*, and the host cannot reach *what*.
    #[must_use]
    pub fn capture_progress(&self) -> usize {
        let direct = self
            .apc_capture
            .iter()
            .chain(self.performer.dcs_capture.iter())
            .chain(self.kitty_transfer_capture())
            .map(|capture| capture.seen)
            .sum::<usize>();
        let osc = match &self.raw_osc {
            Some(RawOsc::Iterm2 { capture, .. }) => capture.seen,
            Some(RawOsc::Probe { .. })
            | Some(RawOsc::Palette { .. })
            | Some(RawOsc::Semantic { .. })
            | None => 0,
        };
        direct.saturating_add(osc)
    }

    /// Abandon any capture or raw OSC palette accumulation in flight, releasing
    /// its retained allocation.
    ///
    /// Returns the bytes released. The partially-received payload is
    /// discarded rather than dispatched: a fragment of an image decodes to
    /// nothing useful, so surfacing it would trade memory for a broken
    /// picture instead of no picture.
    ///
    /// The sender is not told, so the rest of the transfer keeps arriving.
    /// Those bytes are swallowed rather than returned to ground: a payload is
    /// printable ASCII end to end, so a parser back in ground would print
    /// megabytes of base64 into the grid — the user would lose the image
    /// *and* the screen. The discard ends at the transfer's terminator, or at
    /// the first newline if the sender died and no terminator is ever coming.
    ///
    /// Intended for a capture the host has determined is stalled — see
    /// [`Parser::capture_progress`]. Cancelling one that is merely slow costs
    /// the user their transfer, so the staleness threshold belongs to the host
    /// where the clock and the user's configuration both are.
    ///
    /// Returns 0 and does nothing only when neither a media capture nor a raw
    /// OSC palette accumulation is retaining bytes.
    pub fn cancel_capture(&mut self) -> usize {
        let released = self.retained_amount().bytes;
        let semantic = match self.raw_osc.as_ref() {
            Some(RawOsc::Semantic { code, pending_esc, .. }) => Some((*code, *pending_esc)),
            _ => None,
        };
        if released == 0 && self.live_capture_count() == 0 && semantic.is_none() {
            // When: released and live_capture_count show no capture, keep ground unchanged rather than swallowing ordinary output.
            return 0;
        }
        let in_sequence = self.apc_capture.is_some()
            || self.apc_scratch.is_some()
            || self.performer.dcs_capture.is_some()
            || self.raw_osc.is_some();
        self.abort_kitty_transfer();
        if !in_sequence {
            // When: in_sequence is false, only a kitty transfer was open between chunks; ground stays, later chunks are swallowed.
            return released;
        }
        let escape_family =
            if matches!(self.raw_osc, Some(RawOsc::Iterm2 { .. } | RawOsc::Semantic { .. })) {
                EscapeFamily::Osc
            } else {
                EscapeFamily::String
            };
        if let Some((code, _)) = semantic {
            self.performer.invalidate_semantic_osc(code);
        }
        self.inner = vte::Parser::new();
        self.reset_cancelled_escape();
        // OSC payloads also accept BEL, unlike APC/DCS strings.
        self.escape_family = escape_family;
        self.discarding_oversized_escape = true;
        self.discard_escape_pending_esc = semantic.is_some_and(|(_, pending)| pending);
        self.discard_exits_on_newline = semantic.is_none();
        self.performer.fast_path_ready = false;
        released
    }

    /// Number of media captures currently accumulating.
    ///
    /// Beginning another escape family ends or cancels a capture already in
    /// flight, so APC, DCS, and iTerm2 OSC slots are alternatives rather than
    /// addends. That exclusivity is what needs holding, so it is exposed for
    /// assertion rather than left as an emergent property.
    ///
    /// An open kitty transfer counts as the parser's capture between its chunks: its later chunks
    /// read their headers into scratch and stream into the transfer's reservation, so the transfer
    /// and a staged APC are never both live.
    #[must_use]
    pub fn live_capture_count(&self) -> usize {
        usize::from(self.apc_capture.is_some())
            + usize::from(self.kitty_transfer_capture().is_some())
            + usize::from(self.performer.dcs_capture.is_some())
            + usize::from(matches!(self.raw_osc, Some(RawOsc::Iterm2 { .. })))
    }
}

#[cfg(test)]
#[path = "vt_capture_tests.rs"]
mod vt_capture_tests;
