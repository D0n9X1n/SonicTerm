use super::*;

/// Parse an OSC 7 payload while retaining its authority provenance.
///
/// `file://authority/path` requires a path separator after the authority.
/// Scheme-less payloads remain accepted for compatibility and carry an empty
/// authority. Valid `%XX` escapes in the path are decoded; malformed escapes
/// remain literal.
pub fn parse_osc7_cwd_snapshot(raw: &str) -> Option<Osc7Cwd> {
    let (authority, path_text) = if let Some(rest) = raw.strip_prefix("file://") {
        // When: `raw` has a `file://` prefix, preserve its authority so callers can distinguish local and remote paths.
        let path_start = rest.find('/')?;
        let authority = &rest[..path_start];
        if !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            // When: `authority` contains characters outside the OSC 7 host grammar, revoke the trusted CWD snapshot.
            return None;
        }
        (authority, &rest[path_start..])
    } else {
        // When: `raw` has no `file://` prefix, retain compatibility while representing its authority as local and empty.
        ("", raw)
    };
    let path = percent_decode_strict(path_text)?;
    if path.is_empty() || path.chars().any(char::is_control) {
        // When: decoded `path` is empty or contains control characters, reject it as an unsafe filesystem base.
        return None;
    }
    Some(Osc7Cwd { authority: authority.to_string(), path })
}

/// Parse an OSC 7 payload into its filesystem path for legacy callers.
///
/// This preserves the permissive title behavior: malformed percent escapes stay
/// literal and a `file://` authority is discarded. Security-sensitive code must
/// use [`parse_osc7_cwd_snapshot`] so authority and malformed-input provenance
/// cannot be lost.
pub fn parse_osc7_cwd(raw: &str) -> String {
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    let path_part = match stripped.find('/') {
        Some(index) => &stripped[index..],
        None => stripped,
    };
    percent_decode(path_part)
}

fn percent_decode_strict(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            // When: `bytes[index]` starts an escape, require and decode exactly one complete hexadecimal triplet.
            if index + 2 >= bytes.len() {
                // When: fewer than two bytes follow `%`, reject the truncated OSC 7 escape instead of preserving ambiguity.
                return None;
            }
            let high = hex_nibble(bytes[index + 1])?;
            let low = hex_nibble(bytes[index + 2])?;
            out.push((high << 4) | low);
            index += 3;
        } else {
            // When: `bytes[index]` is not `%`, preserve the literal path byte unchanged.
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            // When: bytes has a complete percent triplet at index, decode it only if both following digits are hexadecimal.
            if let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
            {
                // When: hex_nibble produced high and low, combine them into the original byte instead of preserving the escape text.
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| encoded.to_string())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(10 + byte - b'a'),
        b'A'..=b'F' => Some(10 + byte - b'A'),
        _ => None,
    }
}

impl Performer {
    /// Surface one parsed program-state OSC as a host event.
    fn push_program_osc(&mut self, program: program::ProgramOsc) {
        match program {
            program::ProgramOsc::Progress(report) => self.events.push(VtEvent::Progress(report)),
            program::ProgramOsc::Notify { title, body } => {
                self.events.push(VtEvent::Notify { title, body });
            }
            program::ProgramOsc::Ignored => {
                // When: program is ProgramOsc::Ignored, nothing changes.
            }
        }
    }

    pub(super) fn invalidate_semantic_osc(&mut self, code: u16) {
        // When: code selects CWD or hyperlink state, revoke it; other codes preserve the prior display title.
        match code {
            7 => {
                self.cwd_revision = self.cwd_revision.wrapping_add(1);
                self.osc7_cwd = None;
            }
            8 => {
                self.current_hyperlink = None;
                self.events.push(VtEvent::Hyperlink { id: None, uri: String::new() });
            }
            _ => {}
        }
    }

    pub(super) fn handle_semantic_osc(&mut self, code: u16, content: &[u8]) {
        if is_program_osc(code) {
            // When: code is 9 or 777, the program grammar reads every `;`-separated part; its text
            // is sanitized by the host, never rejected here.
            let params: Vec<&[u8]> = content.split(|byte| *byte == b';').collect();
            let program = if code == 9 {
                program::parse_osc9(&params)
            } else {
                // When: code is 777, the payload is an OSC 777 subcommand.
                program::parse_osc777(&params)
            };
            self.push_program_osc(program);
            return;
        }
        let Ok(text) = std::str::from_utf8(content) else {
            // When: content is invalid UTF-8, reject the update rather than replacing bytes in a path or URI.
            self.invalidate_semantic_osc(code);
            return;
        };
        if text.chars().any(char::is_control) {
            // When: text contains control characters, do not authorize a hidden path or link target.
            self.invalidate_semantic_osc(code);
            return;
        }
        match code {
            0 => self.osc_dispatch(&[b"0", content], false),
            2 => self.osc_dispatch(&[b"2", content], false),
            7 => self.osc_dispatch(&[b"7", content], false),
            8 => {
                // When: code is OSC 8, separate link parameters from the full URI without splitting the URI again.
                let Some((parameters, uri)) = text.split_once(';') else {
                    // When: text lacks the URI separator, end any preceding hyperlink.
                    self.invalidate_semantic_osc(code);
                    return;
                };
                self.osc_dispatch(&[b"8", parameters.as_bytes(), uri.as_bytes()], false);
            }
            _ => {
                // When: code is outside the owned OSC families, leave terminal state unchanged.
            }
        }
    }

    /// Free interned hyperlinks no cell references, returning the count.
    ///
    /// Runs only when admission has already failed, so a pane below the cap
    /// never pays for it. The scan is `O(visible + scrollback)` over every
    /// screen the grid owns.
    ///
    /// The root set is every cell plus [`Self::current_hyperlink`], which is
    /// live but not yet written to any cell: between OSC 8 open and the first
    /// printed character there is no cell holding it, and freeing it there
    /// would unlink the text about to be written.
    pub(super) fn reclaim_hyperlinks(&mut self) -> usize {
        let mut live: HashSet<HyperlinkId> = HashSet::new();
        if let Some(open) = self.current_hyperlink {
            live.insert(open);
        }
        self.grid.collect_live_hyperlinks(&mut live);
        self.hyperlinks.retain_live(&live)
    }
}

#[cfg(test)]
#[path = "vt_osc_tests.rs"]
mod vt_osc_tests;
