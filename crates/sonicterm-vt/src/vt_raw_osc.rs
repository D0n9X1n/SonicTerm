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
        // When: raw_osc selects prefix probing, OSC 4 accumulation, or OSC 1337 media staging.
        match raw_osc {
            RawOsc::Probe { mut content, mut content_len } => {
                let index = usize::from(content_len);
                if index < content.len() {
                    content[index] = byte;
                    content_len += 1;
                }
                let prefix = &content[..usize::from(content_len)];
                if matches!(prefix, b"0;" | b"2;" | b"7;" | b"8;") {
                    // When: prefix matches 0, 2, 7, or 8, own its payload before vte can split or truncate it.
                    self.inner = vte::Parser::new();
                    self.pending_esc = false;
                    self.raw_osc = Some(RawOsc::Semantic {
                        code: u16::from(prefix[0] - b'0'),
                        content: Vec::new(),
                        pending_esc: false,
                    });
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
                // A digit run with room left in the probe may still become `9;` or `777;`.
                let digits_pending = prefix.iter().all(u8::is_ascii_digit) && index < content.len();
                if !ITERM2_OSC_PREFIX.starts_with(prefix)
                    && !OSC4_PREFIX.starts_with(prefix)
                    && ![b"0;".as_slice(), b"2;", b"7;", b"8;"]
                        .iter()
                        .any(|value| value.starts_with(prefix))
                    && !digits_pending
                {
                    // When: prefix matches no owned OSC family, return ownership to vte.
                    return false;
                }
                // When: prefix selects OSC 4, complete OSC 1337, or continued bounded probing.
                if prefix == OSC4_PREFIX {
                    self.raw_osc = Some(RawOsc::Palette { content: Vec::new() });
                } else if prefix == ITERM2_OSC_PREFIX {
                    self.inner = vte::Parser::new();
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
                        self.performer.handle_semantic_osc(code, &content);
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
                    self.performer.handle_semantic_osc(code, &content);
                    self.finish_escape();
                    return true;
                }
                if byte == 0x1b {
                    pending_esc = true;
                } else if byte < 0x20 && is_program_osc(code) {
                    // When: byte is a C0 control inside a program payload, drop it as vte does in an OSC string.
                } else if content.len() == raw_osc_cap(code) {
                    // When: content reaches raw_osc_cap, refuse the whole update and swallow its remaining payload.
                    self.performer.invalidate_semantic_osc(code);
                    self.discarding_oversized_escape = true;
                    self.escape_family = EscapeFamily::Osc;
                    self.discard_escape_pending_esc = false;
                    return true;
                } else {
                    // When: byte is payload and content remains below its cap, retain it for whole-message validation.
                    content.push(byte);
                }
                self.raw_osc = Some(RawOsc::Semantic { code, content, pending_esc });
                true
            }
            RawOsc::Palette { mut content } => {
                match byte {
                    0x07 | 0x1b => {
                        self.performer.handle_osc4_raw(&content, byte == 0x07);
                        self.performer.suppress_next_osc4 = true;
                        self.pending_esc = byte == 0x1b;
                    }
                    _ => {
                        if content.len() < MAX_RAW_OSC4_BYTES {
                            content.push(byte);
                            self.raw_osc = Some(RawOsc::Palette { content });
                        }
                    }
                }
                false
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
                            self.performer.events.push(VtEvent::Media(event));
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

#[cfg(test)]
#[path = "vt_raw_osc_tests.rs"]
mod vt_raw_osc_tests;
