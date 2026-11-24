use super::*;

/// The scheme of a working-directory URI: `file://` paths are percent-decoded, and kitty's
/// `kitty-shell-cwd://` paths are raw, so a literal `%` is kept.
fn cwd_uri_rest(raw: &str) -> Option<(&str, bool)> {
    raw.strip_prefix("file://")
        .map(|rest| (rest, true))
        .or_else(|| raw.strip_prefix("kitty-shell-cwd://").map(|rest| (rest, false)))
}

/// Parse an OSC 7 payload while retaining its authority provenance.
///
/// `file://authority/path` and `kitty-shell-cwd://authority/path` require a path separator after
/// the authority. Scheme-less payloads remain accepted for compatibility and carry an empty
/// authority. Valid `%XX` escapes in a `file://` or scheme-less path are decoded; a malformed
/// escape rejects the snapshot. A `kitty-shell-cwd://` path is kept raw.
pub fn parse_osc7_cwd_snapshot(raw: &str) -> Option<Osc7Cwd> {
    let (authority, path_text, decoded) = if let Some((rest, decoded)) = cwd_uri_rest(raw) {
        // When: `raw` names a URI scheme, preserve its authority so callers can distinguish local and remote paths.
        let path_start = rest.find('/')?;
        let authority = &rest[..path_start];
        if !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            // When: `authority` contains characters outside the OSC 7 host grammar, revoke the trusted CWD snapshot.
            return None;
        }
        (authority, &rest[path_start..], decoded)
    } else {
        // When: `raw` has no URI scheme, retain compatibility while representing its authority as local and empty.
        ("", raw, true)
    };
    let path = if decoded {
        percent_decode_strict(path_text)?
    } else {
        // When: decoded is false, the scheme is `kitty-shell-cwd://`, whose path is raw, so `%` is literal.
        path_text.to_string()
    };
    if path.is_empty() || path.chars().any(char::is_control) {
        // When: decoded `path` is empty or contains control characters, reject it as an unsafe filesystem base.
        return None;
    }
    Some(Osc7Cwd { authority: authority.to_string(), path, uri_path: true })
}

/// Parse an OSC 7 payload into its filesystem path for legacy callers.
///
/// This preserves the permissive title behavior: malformed percent escapes stay
/// literal and a URI authority is discarded. Security-sensitive code must
/// use [`parse_osc7_cwd_snapshot`] so authority and malformed-input provenance
/// cannot be lost.
pub fn parse_osc7_cwd(raw: &str) -> String {
    let (stripped, decoded) = cwd_uri_rest(raw).unwrap_or((raw, true));
    let path_part = match stripped.find('/') {
        Some(index) => &stripped[index..],
        None => stripped,
    };
    if decoded {
        percent_decode(path_part)
    } else {
        // When: decoded is false, the scheme is `kitty-shell-cwd://`, whose path is shown raw.
        path_part.to_string()
    }
}

/// The OSC 1337 key whose value reports the working directory. The collector stores it in this
/// spelling whatever ASCII case the program sent.
pub(super) const CURRENT_DIR_KEY: &[u8] = b"CurrentDir=";

/// Which sequence reported a working directory; each reads a bare (scheme-less) path its own way.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CwdSource {
    /// OSC 7: a bare path takes the scheme-less rule, percent-decoded and trusted.
    Osc7,
    /// ConEmu OSC 9;9: a bare path is literal after one pair of surrounding quotes, and trusted.
    ConEmu,
    /// OSC 1337 `CurrentDir=`: a bare path is shown but never trusted, since iTerm2 pairs it with a
    /// `RemoteHost` SonicTerm does not read.
    CurrentDir,
}

/// The directory report an OSC `code` payload holds, complete or cut, and its value: OSC 7, a
/// ConEmu `9;9;`, or an OSC 1337 `CurrentDir=` as the collector stores it.
pub(super) fn cwd_report(code: u16, content: &[u8]) -> Option<(CwdSource, &[u8])> {
    match code {
        7 => Some((CwdSource::Osc7, content)),
        9 => program::osc9_cwd_payload(content).map(|value| (CwdSource::ConEmu, value)),
        1337 => content.strip_prefix(CURRENT_DIR_KEY).map(|value| (CwdSource::CurrentDir, value)),
        _ => None,
    }
}

/// `text` without one pair of surrounding double quotes, as kitty reads OSC 9;9; an unmatched
/// quote stays literal.
fn strip_one_quote_pair(text: &str) -> &str {
    text.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')).unwrap_or(text)
}

pub(super) fn percent_decode_strict(encoded: &str) -> Option<String> {
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

/// The OSC 133 parameter a SonicTerm shell integration appends to every marker.
pub(super) const SHELL_TOKEN_PARAM: &[u8] = b"sonicterm_token=";
/// The OSC 133 D parameter naming where its exit code came from; SonicTerm's own key.
pub(super) const EXIT_SOURCE_PARAM: &[u8] = b"sonicterm_src=";

/// Compare two byte strings in time that depends only on their lengths, so a program probing the
/// token cannot learn it a byte at a time from how long a marker takes.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        // When: `left.len()` and `right.len()` differ, they cannot match; lengths are not secret.
        return false;
    }
    left.iter().zip(right).fold(0u8, |diff, (one, two)| diff | (one ^ two)) == 0
}

impl Performer {
    /// Whether an OSC 133 marker with parameters `extra` (after its kind) is admitted, and if so
    /// whether it is authenticated. With no pane token every marker is admitted unauthenticated. A
    /// wrong token is refused; the right one latches the pane, after which tokenless markers are
    /// refused too, so neither forged nor downgraded markers reach the grid or the events. The
    /// right token latches only before the pane took input: a shell with no integration leaves the
    /// token in every command's environment, so after input it proves nothing.
    pub(super) fn admit_shell_marker(&mut self, extra: &[&[u8]]) -> Option<bool> {
        let Some(expected) = self.shell_token.as_deref() else {
            // When: the pane has no `shell_token`, markers keep their untrusted behavior.
            return Some(false);
        };
        let carried = extra.iter().find_map(|param| param.strip_prefix(SHELL_TOKEN_PARAM));
        match carried {
            Some(token) if constant_time_eq(token, expected.as_bytes()) => {
                // When: the right `token` arrives it latches, unless input came first. The writer
                // sets the flag before queuing input, so output that input caused finds it set.
                let typed =
                    self.shell_input_seen.as_ref().is_some_and(|seen| seen.load(Ordering::SeqCst));
                if typed && !self.shell_authenticated {
                    // When: input was `typed` before any latch, a command may hold the token.
                    return Some(false);
                }
                self.shell_authenticated = true;
                Some(true)
            }
            // A marker carrying a wrong token is a forgery, and once the pane is authenticated a
            // tokenless one is a downgrade; both are refused.
            Some(_) => None,
            None if self.shell_authenticated => None,
            None => Some(false),
        }
    }
    /// Whether an OSC 133 annotation (L, N, P, I) or a 9;12 with parameters `extra` is admitted.
    /// It never latches: with no pane token or the right token it is admitted, a wrong token is
    /// refused, and a tokenless one is admitted only while the pane is not latched. The right
    /// token leaves the observed shell state as it was; anything else marks it unverified.
    pub(super) fn admit_annotation(&mut self, extra: &[&[u8]]) -> bool {
        let carried = extra.iter().find_map(|param| param.strip_prefix(SHELL_TOKEN_PARAM));
        let Some(expected) = self.shell_token.as_deref() else {
            // When: the pane has no `shell_token`, every annotation is admitted unauthenticated.
            self.shell_state = super::ShellIntegrationState::Unverified;
            return true;
        };
        if carried.is_some_and(|token| constant_time_eq(token, expected.as_bytes())) {
            // When: the right token is `carried`, admit it and keep the observed state.
            return true;
        }
        self.shell_state = super::ShellIntegrationState::Unverified;
        // A wrong token is a forgery and, after the latch, a tokenless one is a downgrade.
        carried.is_none() && !self.shell_authenticated
    }
    /// Read one OSC 99: a completed notification becomes a host event, and a query is answered
    /// with the parts and occasions SonicTerm shows.
    fn receive_kitty_notify(&mut self, content: &[u8]) {
        match self.kitty_notifications.receive(content) {
            kitty_notify::KittyNotify::Show { id, title, body, occasion } => {
                self.events.push(VtEvent::Notify { title, body, id, occasion });
            }
            kitty_notify::KittyNotify::Query { id } => {
                let reply =
                    format!("\x1b]99;i={id}:p=?;p=title,body:o=always,unfocused,invisible\x1b\\");
                self.reply(reply.as_bytes());
            }
            kitty_notify::KittyNotify::Nothing => {
                // When: the result is `Nothing`, a chunk was stored or the sequence was ignored.
            }
        }
    }

    /// Surface one parsed program-state OSC as a host event.
    fn push_program_osc(&mut self, program: program::ProgramOsc) {
        match program {
            program::ProgramOsc::Progress(report) => self.events.push(VtEvent::Progress(report)),
            program::ProgramOsc::Notify { title, body } => {
                let occasion = NotifyOccasion::Always;
                self.events.push(VtEvent::Notify { title, body, id: None, occasion });
            }
            program::ProgramOsc::PromptMark => self.conemu_prompt_mark(),
            program::ProgramOsc::Ignored => {
                // When: program is ProgramOsc::Ignored, nothing changes.
            }
        }
    }

    /// Store a complete OSC 1 name (the tab name); the parser only stores it and the host decides
    /// where it shows. An empty name, as a bare OSC 1 sends, clears it.
    fn set_icon_title(&mut self, text: &str) {
        self.icon_title = (!text.is_empty()).then(|| text.to_string());
    }

    /// Revoke the trusted directory snapshot with a new revision; the displayed path stays.
    fn revoke_cwd_snapshot(&mut self) {
        self.cwd_revision = self.cwd_revision.wrapping_add(1);
        self.osc7_cwd = None;
    }

    /// Apply one directory report from `source` with its raw `value`: the displayed path for
    /// titles, the host-aware snapshot for local use, and a new revision. An empty OSC 7 or 9;9
    /// clears both, and an empty `CurrentDir` is ignored. A `file://` or `kitty-shell-cwd://` URI
    /// reads alike from every source, and its host stays in the snapshot for the app's local check.
    pub(super) fn report_cwd(&mut self, source: CwdSource, value: &[u8]) {
        let text =
            std::str::from_utf8(value).ok().filter(|text| !text.chars().any(char::is_control));
        let Some(text) = text else {
            // When: text is absent (value not UTF-8, or a control in it), no hidden base is
            // trusted: the snapshot is revoked and the displayed path stays.
            self.revoke_cwd_snapshot();
            return;
        };
        let text = if source == CwdSource::ConEmu {
            strip_one_quote_pair(text)
        } else {
            // When: source is OSC 7 or `CurrentDir`, quotes are path text; only 9;9 strips a pair.
            text
        };
        if text.is_empty() {
            // When: text is empty, OSC 7 and 9;9 clear the directory; Ghostty ignores an empty
            // `CurrentDir`, and so does SonicTerm.
            if source != CwdSource::CurrentDir {
                self.cwd_revision = self.cwd_revision.wrapping_add(1);
                self.cwd = None;
                self.osc7_cwd = None;
            }
            return;
        }
        self.cwd_revision = self.cwd_revision.wrapping_add(1);
        // A URI, or OSC 7's scheme-less path: the strict parse decides trust and the permissive one
        // the display, so a malformed URI shows but is never trusted.
        if source == CwdSource::Osc7 || cwd_uri_rest(text).is_some() {
            self.osc7_cwd = parse_osc7_cwd_snapshot(text);
            let title_path = parse_osc7_cwd(text);
            if !title_path.is_empty() {
                self.cwd = Some(title_path);
            }
        } else {
            // When: text is a bare 9;9 or `CurrentDir` path, it shows literally; only 9;9 stores it
            // with an empty authority, for native validation to accept or reject later.
            self.cwd = Some(text.to_string());
            self.osc7_cwd = (source == CwdSource::ConEmu).then(|| Osc7Cwd {
                authority: String::new(),
                path: text.to_string(),
                uri_path: false,
            });
        }
    }

    /// Revoke what a cut, refused or invalid OSC `code` with payload `content` would have set.
    pub(super) fn invalidate_semantic_osc(&mut self, code: u16, content: &[u8]) {
        if cwd_report(code, content).is_some() {
            // When: content is a directory report from any source, revoke the trusted snapshot as
            // a cut OSC 7 does; the displayed path stays.
            self.revoke_cwd_snapshot();
            return;
        }
        // When: code selects hyperlink or notification state, revoke it; other codes preserve the prior display title.
        match code {
            8 => {
                self.current_hyperlink = None;
                self.events.push(VtEvent::Hyperlink { id: None, uri: String::new() });
            }
            // A refused or cancelled OSC 99 chunk names an id the parser never read, so every
            // pending notification is dropped: a later chunk can never complete a cut one.
            99 => self.kitty_notifications.clear(),
            _ => {}
        }
    }

    /// Apply one complete collected OSC `code` payload. A directory report is classified once, on
    /// the bytes as they arrived; only another program or color payload then loses its C0
    /// controls, as vte drops them, so a stripped payload is never read as a directory.
    pub(super) fn handle_semantic_osc(
        &mut self,
        code: u16,
        mut content: Vec<u8>,
        bell_terminated: bool,
    ) {
        if let Some((source, value)) = cwd_report(code, &content) {
            // When: content is a directory report (OSC 7, a ConEmu 9;9 or an OSC 1337
            // `CurrentDir=`), one rule reads every source before any other grammar sees it.
            self.report_cwd(source, value);
            return;
        }
        if code == 133 {
            // When: code is a complete OSC 133 marker, its controls are dropped as vte drops them,
            // and its parameters split only at unescaped semicolons.
            content.retain(|byte| *byte >= 0x20);
            self.shell_marker(&super::vt_semantic::split_marker_params(&content));
            return;
        }
        if is_program_osc(code) || vt_color::is_color_osc(code) {
            // A program or color request's controls are not payload; the collector kept them
            // only so its cap counted every wire byte.
            content.retain(|byte| *byte >= 0x20);
        }
        let content = content.as_slice();
        if code == 22 {
            // When: code is 22, the pointer-shape grammar reads the names; an unknown or non-UTF-8
            // name matches nothing and is ignored there.
            self.apply_pointer_shape_osc(content, bell_terminated);
            return;
        }
        if vt_color::is_color_osc(code) {
            // When: code is a color request, apply its whole payload in order; its replies end
            // with the request's own terminator, BEL when `bell_terminated`.
            self.apply_color_osc(code, content, bell_terminated);
            return;
        }
        if code == 1337 {
            // When: code is an OSC 1337 `SetUserVar` payload, each record applies alone; names
            // and values are decoded strictly there, never as this path's title text.
            vt_user_vars::apply_payload(&mut self.user_vars, content);
            return;
        }
        if code == 7501 {
            // When: code is an OSC 7501 program-status report or query, kitty's grammar reads it.
            self.apply_program_status_osc(content, bell_terminated);
            return;
        }
        if code == 99 {
            // When: code is 99, kitty's notification grammar reads it, chunk by chunk.
            self.receive_kitty_notify(content);
            return;
        }
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
            // When: content is invalid UTF-8, reject the update rather than replacing bytes in a URI.
            self.invalidate_semantic_osc(code, content);
            return;
        };
        if text.chars().any(char::is_control) {
            // When: text contains control characters, do not authorize a hidden link target.
            self.invalidate_semantic_osc(code, content);
            return;
        }
        match code {
            0 => self.osc_dispatch(&[b"0", content], false),
            1 => self.set_icon_title(text),
            2 => self.osc_dispatch(&[b"2", content], false),
            8 => {
                // When: code is OSC 8, separate link parameters from the full URI without splitting the URI again.
                let Some((parameters, uri)) = text.split_once(';') else {
                    // When: text lacks the URI separator, end any preceding hyperlink.
                    self.invalidate_semantic_osc(code, content);
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

#[cfg(test)]
#[path = "vt_osc_kitty_notify_tests.rs"]
mod vt_osc_kitty_notify_tests;

#[cfg(test)]
#[path = "vt_osc_cwd_tests.rs"]
mod vt_osc_cwd_tests;
