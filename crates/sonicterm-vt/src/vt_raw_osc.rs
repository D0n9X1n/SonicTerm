use super::*;

impl Parser {
    fn append_iterm2_metadata_byte(&mut self, capture: &mut MediaCapture, byte: u8) {
        capture.seen = capture.seen.saturating_add(1);
        if !capture.admitted() {
            // When: capture is not admitted, retain no metadata outside the process staging pool.
            return;
        }
        if self.iterm2_metadata_len < self.iterm2_metadata.len() {
            // When: iterm2_metadata_len remains below the fixed buffer length, retain this metadata byte inline.
            self.iterm2_metadata[self.iterm2_metadata_len] = byte;
            self.iterm2_metadata_len += 1;
            return;
        }
        if !capture.truncated {
            tracing::warn!(
                protocol = ?capture.protocol,
                metadata_bytes = capture.seen,
                cap = MAX_ITERM2_METADATA_BYTES,
                "media metadata exceeded limit; refusing whole image"
            );
        }
        capture.truncated = true;
    }

    pub(super) fn observe_osc_byte(&mut self, byte: u8) {
        if self.pending_esc {
            // When: pending_esc is set, inspect byte for the OSC introducer before forwarding later content to raw capture.
            self.pending_esc = false;
            if byte == b']' {
                // When: byte opens OSC after ESC, start the bounded prefix probe alongside vte.
                self.raw_osc =
                    Some(RawOsc::Probe { content: [0; ITERM2_OSC_PREFIX.len()], content_len: 0 });
                return;
            }
        }
        self.pending_esc = byte == 0x1b;
    }

    pub(super) fn consume_raw_osc_byte(&mut self, byte: u8) -> bool {
        let Some(raw_osc) = self.raw_osc.take() else {
            // When: raw_osc is absent, leave this byte for vte or APC ownership.
            return false;
        };
        // When: raw_osc selects prefix probing, whole-payload capture, or OSC 1337 media staging.
        match raw_osc {
            RawOsc::Probe { mut content, mut content_len } => {
                // A bare color or pointer-shape number in the probe ends at BEL or ESC.
                let bare_code = matches!(byte, 0x07 | 0x1b)
                    .then(|| {
                        let probed = &content[..usize::from(content_len)];
                        (probed == b"22")
                            .then_some(22)
                            .or_else(|| is_icon_title_number(probed).then_some(1))
                            .or_else(|| vt_color::color_osc_number(probed))
                    })
                    .flatten();
                if let Some(code) = bare_code {
                    // When: bare_code holds a bare color request (`104`, `110`), a bare `22` or a
                    // bare OSC 1, own it: BEL applies it now, ESC waits for the `\` that completes ST.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    if byte == 0x07 {
                        self.performer.handle_semantic_osc(code, &[], true);
                        self.finish_escape();
                    } else {
                        // When: byte is ESC, only a following `\` completes the request.
                        self.raw_osc =
                            Some(RawOsc::Semantic { code, content: Vec::new(), pending_esc: true });
                    }
                    return true;
                }
                let index = usize::from(content_len);
                if index < content.len() {
                    content[index] = byte;
                    content_len += 1;
                }
                let prefix = &content[..usize::from(content_len)];
                // OSC 1 in any numeric spelling, else the canonical 0, 2, 7 or 8.
                let text_code = prefix
                    .strip_suffix(b";")
                    .is_some_and(is_icon_title_number)
                    .then_some(1)
                    .or_else(|| {
                        matches!(prefix, b"0;" | b"2;" | b"7;" | b"8;")
                            .then(|| u16::from(prefix[0] - b'0'))
                    });
                if let Some(code) = text_code {
                    // When: text_code names OSC 1 in any spelling, or 0, 2, 7 or 8, own its
                    // payload before vte can split or truncate it.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    self.raw_osc =
                        Some(RawOsc::Semantic { code, content: Vec::new(), pending_esc: false });
                    return true;
                }
                if let Some(code) = program_osc_number(prefix) {
                    // When: program_osc_number reads 9 or 777, zero-padded or not, own the program
                    // payload so no part is lost to vte's parameter cap.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    self.raw_osc =
                        Some(RawOsc::Semantic { code, content: Vec::new(), pending_esc: false });
                    return true;
                }
                if prefix == b"22;" {
                    // When: the prefix names a pointer-shape request, own its whole payload so the
                    // 4 KiB bound drops an over-cap packet whole.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    self.raw_osc = Some(RawOsc::Semantic {
                        code: 22,
                        content: Vec::new(),
                        pending_esc: false,
                    });
                    return true;
                }
                if let Some(code) = prefix.strip_suffix(b";").and_then(vt_color::color_osc_number) {
                    // When: the prefix names a color request, own its whole payload so it applies
                    // only once complete and no part is lost to vte's parameter cap.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    self.raw_osc =
                        Some(RawOsc::Semantic { code, content: Vec::new(), pending_esc: false });
                    return true;
                }
                // A digit run, optionally signed `+`, with room left in the probe may still become
                // `9;`, `777;` or a color request.
                let digits = prefix.strip_prefix(b"+").unwrap_or(prefix);
                let digits_pending = digits.iter().all(u8::is_ascii_digit) && index < content.len();
                if !ITERM2_OSC_PREFIX.starts_with(prefix)
                    && ![b"0;".as_slice(), b"1;", b"2;", b"7;", b"8;"]
                        .iter()
                        .any(|value| value.starts_with(prefix))
                    && !digits_pending
                {
                    // When: prefix matches no owned OSC family, return ownership to vte.
                    return false;
                }
                // When: prefix selects complete OSC 1337, or continued bounded probing.
                if prefix == ITERM2_OSC_PREFIX {
                    self.inner = vte::Parser::new();
                    // An open kitty upload releases its reservation before this capture asks for one.
                    self.performer.abort_kitty_transfer_for_other_media();
                    let mut capture = MediaCapture::new(
                        &self.performer.staging_pool,
                        MediaProtocol::Iterm2File,
                        String::new(),
                    );
                    for byte in b"File=" {
                        self.append_iterm2_metadata_byte(&mut capture, *byte);
                    }
                    self.raw_osc =
                        Some(RawOsc::Iterm2 { capture, metadata_done: false, pending_esc: false });
                    return true;
                } else {
                    self.raw_osc = Some(RawOsc::Probe { content, content_len });
                }
                false
            }
            RawOsc::Semantic { code, mut content, mut pending_esc } => {
                if matches!(byte, 0x18 | 0x1a) {
                    // When: byte matches 0x18 or 0x1a, discard semantic state before returning to ground.
                    self.performer.invalidate_semantic_osc(code);
                    self.finish_escape();
                    return true;
                }
                if pending_esc {
                    // When: pending_esc is set, only backslash completes ST; other escapes retain normal parser ownership.
                    if byte == b'\\' {
                        // When: byte completes ST, apply only the complete semantic payload.
                        drop_payload_controls(code, &mut content);
                        self.performer.handle_semantic_osc(code, &content, false);
                        self.finish_escape();
                        return true;
                    }
                    self.performer.invalidate_semantic_osc(code);
                    self.finish_escape();
                    self.feed_vte_byte(0x1b);
                    return false;
                }
                if byte == 0x07 {
                    // When: byte is BEL, dispatch the complete bounded semantic payload.
                    drop_payload_controls(code, &mut content);
                    self.performer.handle_semantic_osc(code, &content, true);
                    self.finish_escape();
                    return true;
                }
                if byte == 0x1b {
                    pending_esc = true;
                } else if content.len() == raw_osc_cap(code) {
                    // When: content reaches raw_osc_cap, refuse the whole update and swallow the rest
                    // through this collector's own terminators, so no suffix of it prints.
                    self.performer.invalidate_semantic_osc(code);
                    self.raw_osc = Some(RawOsc::Refused { pending_esc: false });
                    return true;
                } else {
                    // When: byte is payload below its cap, retain it, C0 controls included, so the
                    // cap counts wire bytes; program and color payloads drop those at dispatch.
                    content.push(byte);
                }
                self.raw_osc = Some(RawOsc::Semantic { code, content, pending_esc });
                true
            }
            RawOsc::Refused { pending_esc } => {
                if matches!(byte, 0x18 | 0x1a) || (pending_esc && byte == b'\\') {
                    // When: byte is CAN, SUB or completes ST, the refused message ends and nothing
                    // of it applies.
                    self.finish_escape();
                    return true;
                }
                if pending_esc {
                    // When: pending_esc did not start ST, the ESC ends the refused message as it
                    // ends an accepted one, and it and this byte, a BEL included, reach vte.
                    self.finish_escape();
                    self.feed_vte_byte(0x1b);
                    return false;
                }
                if byte == 0x07 {
                    // When: byte is a BEL inside the refused payload, it ends the message.
                    self.finish_escape();
                    return true;
                }
                self.raw_osc = Some(RawOsc::Refused { pending_esc: byte == 0x1b });
                true
            }
            RawOsc::Iterm2 { mut capture, mut metadata_done, mut pending_esc } => {
                if matches!(byte, 0x18 | 0x1a) {
                    // When: byte matches CAN or SUB, discard the incomplete OSC media transfer and return to ground.
                    self.finish_escape();
                    return true;
                }
                let terminated = byte == 0x07 || byte == 0x9c || (pending_esc && byte == b'\\');
                if terminated {
                    // When: terminated is true, dispatch only a whole OSC 1337 payload with a metadata separator.
                    if metadata_done {
                        if let Some(event) = capture.into_iterm2_event(
                            self.performer.grid.cursor.row,
                            self.performer.grid.cursor.col,
                            &self.iterm2_metadata[..self.iterm2_metadata_len],
                        ) {
                            self.performer.push_media(event);
                        }
                    }
                    self.finish_escape();
                    return true;
                }
                if pending_esc {
                    // When: pending_esc did not complete ST, preserve it in the field selected by metadata_done.
                    if metadata_done {
                        capture.append_byte(0x1b);
                    } else {
                        self.append_iterm2_metadata_byte(&mut capture, 0x1b);
                    }
                    pending_esc = false;
                }
                // When: byte and metadata_done select terminator state, metadata completion, or payload ownership.
                if byte == 0x1b {
                    pending_esc = true;
                } else if !metadata_done && byte == b':' {
                    metadata_done = true;
                } else if metadata_done {
                    capture.append_byte(byte);
                } else {
                    self.append_iterm2_metadata_byte(&mut capture, byte);
                }
                self.raw_osc = Some(RawOsc::Iterm2 { capture, metadata_done, pending_esc });
                true
            }
        }
    }
}

/// Whether `spelling` (digits, optionally signed `+`) names OSC 1, as vte's number parse would
/// read it, so `01` and `+1` reach the same bounded owner as the canonical `1`.
fn is_icon_title_number(spelling: &[u8]) -> bool {
    let digits = spelling.strip_prefix(b"+").unwrap_or(spelling);
    !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && digits.iter().fold(0u32, |value, digit| {
            value.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
        }) == 1
}

/// Remove the C0 controls a program or color `code` payload carried, as vte drops them in an OSC
/// string; the collector kept them only so its cap counted every wire byte.
fn drop_payload_controls(code: u16, content: &mut Vec<u8>) {
    if is_program_osc(code) || vt_color::is_color_osc(code) {
        // A program or color request's controls are not payload.
        content.retain(|byte| *byte >= 0x20);
    }
}

#[cfg(test)]
#[path = "vt_raw_osc_tests.rs"]
mod vt_raw_osc_tests;
