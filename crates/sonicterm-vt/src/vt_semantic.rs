//! OSC 133 shell markers and annotations, and ConEmu's OSC 9;12 prompt mark.
//!
//! A, B, C and D change the prompt regions MCP readiness reads and may latch the pane's shell
//! token. L, N, P, I, the options on A and 9;12 are annotations: they pass `admit_annotation`,
//! which never latches, and change only the per-screen semantic state in the grid.

use sonicterm_grid::grid::{
    ClickEvents, ClickLine, CmdlineText, PromptClick, PromptKind, PromptOptions, PromptRedraw,
    MAX_CMDLINE_BYTES,
};

use super::*;

/// Most raw bytes of a `cmdline=` or `cmdline_url=` value read; a longer one is dropped whole.
const MAX_CMDLINE_RAW_BYTES: usize = 4 * 1024;

impl Performer {
    /// Apply one OSC 133 marker whose `params` follow the `133`.
    pub(super) fn shell_marker(&mut self, params: &[&[u8]]) {
        let kind = params.first().and_then(|param| param.first().copied());
        let extra = params.get(1..).unwrap_or(&[]);
        if matches!(kind, Some(b'L' | b'N' | b'P' | b'I')) {
            // When: `matches!` finds an annotation `kind`, it never latches and never touches a
            // prompt region.
            self.shell_annotation(kind.unwrap_or_default(), extra);
            return;
        }
        let admitted = self.admit_shell_marker(extra);
        // The newest marker decides the observed state, a dropped one included: a nested
        // shell's tokenless markers mean the shell in front is not the verified one.
        self.shell_state = if admitted == Some(true) {
            super::ShellIntegrationState::Verified
        } else {
            // Not authenticated: admitted without a token, or refused.
            super::ShellIntegrationState::Unverified
        };
        let Some(authenticated) = admitted else {
            // When: `admit_shell_marker` refused the marker (a wrong token, or tokenless
            // after the pane authenticated), it changes neither the grid nor the events.
            tracing::debug!(target: "vt", "dropped an unauthenticated OSC 133 marker");
            return;
        };
        match kind {
            Some(b'A') => {
                self.semantic_prompt_start(extra);
                self.grid.record_prompt_start();
                // A new prompt ends the program-status records a running program left, at
                // this point in the stream, so a later report in the same batch survives.
                self.end_transient_program_status();
                self.events.push(VtEvent::Command(CommandEvent::PromptStart));
            }
            Some(b'B') => {
                self.grid.record_input_start();
                self.grid.mark_semantic_input(false);
                self.events.push(VtEvent::Command(CommandEvent::PromptEnd));
            }
            Some(b'C') => {
                self.grid.record_output_start_marked(authenticated);
                self.grid.mark_semantic_output();
                // Command text comes only from an authenticated C; any other C clears it.
                let cmdline = authenticated
                    .then(|| parse_cmdline(extra))
                    .flatten()
                    .and_then(CmdlineText::new);
                self.grid.set_semantic_cmdline(cmdline);
                self.events.push(VtEvent::Command(CommandEvent::CmdStart));
            }
            Some(b'D') => {
                let exit_i32 = extra
                    .first()
                    .and_then(|param| std::str::from_utf8(param).ok())
                    .and_then(|text| text.parse::<i32>().ok());
                // The source is read by key, the last one winning, wherever it sits.
                let source = extra
                    .iter()
                    .rev()
                    .find_map(|param| param.strip_prefix(super::vt_osc::EXIT_SOURCE_PARAM))
                    .and_then(sonicterm_grid::grid::ExitSource::from_param);
                self.grid.record_prompt_end_sourced(exit_i32, source, authenticated);
                self.grid.mark_semantic_output();
                let exit = exit_i32.and_then(|status| u8::try_from(status).ok());
                self.events.push(VtEvent::Command(CommandEvent::CmdEnd(exit)));
            }
            _ => {
                // When: kind is not a recognized shell marker, preserve prompt boundaries and emit no command event.
            }
        }
    }

    /// Apply one admitted-or-refused annotation `kind` (L, N, P or I) with parameters `extra`.
    fn shell_annotation(&mut self, kind: u8, extra: &[&[u8]]) {
        if kind == b'L'
            && !extra.iter().all(|param| param.starts_with(super::vt_osc::SHELL_TOKEN_PARAM))
        {
            // When: `kind` is L and `extra` holds a `param` that is not the token, the L is
            // invalid, so nothing changes, the observed shell state included.
            return;
        }
        if !self.admit_annotation(extra) {
            // When: `admit_annotation` refused it, the annotation changes nothing.
            tracing::debug!(target: "vt", "dropped an unauthenticated OSC 133 annotation");
            return;
        }
        match kind {
            b'L' => self.fresh_line(),
            b'N' => self.semantic_prompt_start(extra),
            b'P' => {
                let kind = option_value(extra, b"k").and_then(prompt_kind).unwrap_or_default();
                self.grid.mark_semantic_prompt(kind, None);
            }
            _ => self.grid.mark_semantic_input(true),
        }
    }

    /// A, N or 9;12 with parameters `extra`: fresh-line, then a new generation at the cursor row
    /// with options reset and read from `extra`.
    fn semantic_prompt_start(&mut self, extra: &[&[u8]]) {
        self.fresh_line();
        let kind = option_value(extra, b"k").and_then(prompt_kind).unwrap_or_default();
        self.grid.mark_semantic_prompt(kind, Some(prompt_options(extra)));
    }

    /// ConEmu's `9;12`: an A with no options or token, through the annotation admission.
    pub(super) fn conemu_prompt_mark(&mut self) {
        if self.admit_annotation(&[]) {
            self.semantic_prompt_start(&[]);
        }
    }

    /// CR then index when the cursor is off column 0; at the bottom margin the index scrolls like
    /// any line feed. SonicTerm has no left or right margins, so column 0 is the line start.
    fn fresh_line(&mut self) {
        if self.grid.cursor.col == 0 {
            // When: the cursor is at column 0, the line is already fresh.
            return;
        }
        if let Some(tap) = self.tap.as_mut() {
            // A stream reader armed the `tap`: the new line ends the recorded line.
            tap.push('\n');
        }
        self.advance_hard_line(self.erase_fill_cell(), true);
    }
}

/// The value of the first `key=` parameter in `extra`; later ones are ignored, as in Ghostty.
fn option_value<'param>(extra: &[&'param [u8]], key: &[u8]) -> Option<&'param [u8]> {
    extra.iter().find_map(|param| param.strip_prefix(key)?.strip_prefix(b"="))
}

/// The prompt kind a `k=` value names; any other value is unset.
fn prompt_kind(value: &[u8]) -> Option<PromptKind> {
    match value {
        b"i" => Some(PromptKind::Initial),
        b"r" => Some(PromptKind::Right),
        b"c" => Some(PromptKind::Continuation),
        b"s" => Some(PromptKind::Secondary),
        _ => None,
    }
}

/// The prompt options `extra` declares, each from its first occurrence; a malformed value is
/// unset and keeps its default.
fn prompt_options(extra: &[&[u8]]) -> PromptOptions {
    let redraw = match option_value(extra, b"redraw") {
        Some(b"0") => PromptRedraw::Off,
        Some(b"last") => PromptRedraw::LastLine,
        _ => PromptRedraw::Full,
    };
    let events = match option_value(extra, b"click_events") {
        Some(b"1") => Some(ClickEvents::Absolute),
        Some(b"2") => Some(ClickEvents::Relative),
        _ => None,
    };
    let line_motion = match option_value(extra, b"cl") {
        Some(b"line") => Some(ClickLine::Line),
        Some(b"m") => Some(ClickLine::Multiple),
        Some(b"v") => Some(ClickLine::ConservativeVertical),
        Some(b"w") => Some(ClickLine::SmartVertical),
        _ => None,
    };
    let special_key = option_value(extra, b"special_key") == Some(b"1");
    PromptOptions { redraw, click: PromptClick { events, line_motion, special_key } }
}

/// The command text a C's parameters carry: `cmdline=` (bash `%q`-quoted) wins over
/// `cmdline_url=` (percent-encoded) wherever each sits. `None` when absent, malformed, not UTF-8,
/// past 4 KiB raw or past 1 KiB decoded.
pub(super) fn parse_cmdline(extra: &[&[u8]]) -> Option<String> {
    let (raw, quoted) = match option_value(extra, b"cmdline") {
        Some(raw) => (raw, true),
        None => (option_value(extra, b"cmdline_url")?, false),
    };
    if raw.len() > MAX_CMDLINE_RAW_BYTES {
        // When: the `raw` value passes 4 KiB it is dropped whole, never cut.
        return None;
    }
    let text = if quoted {
        String::from_utf8(unquote_printf_q(raw)?).ok()?
    } else {
        // When: not `quoted`, the value is `cmdline_url=`, strict percent-encoding of UTF-8.
        super::vt_osc::percent_decode_strict(std::str::from_utf8(raw).ok()?)?
    };
    (text.len() <= MAX_CMDLINE_BYTES).then_some(text)
}

/// Undo bash's `printf %q`: `\x` takes `x` literally, `'…'` is literal and `$'…'` holds ANSI-C
/// escapes. `None` for a trailing backslash, an unclosed quote or an unknown escape.
fn unquote_printf_q(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'\\' => {
                out.push(*raw.get(index + 1)?);
                index += 2;
            }
            b'\'' => {
                let len = raw[index + 1..].iter().position(|byte| *byte == b'\'')?;
                out.extend_from_slice(&raw[index + 1..index + 1 + len]);
                index += len + 2;
            }
            b'$' if raw.get(index + 1) == Some(&b'\'') => {
                index = unquote_ansi_c(raw, index + 2, &mut out)?;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Some(out)
}

/// Decode a `$'…'` body starting at `raw[start]` into `out`; returns the index past its closing
/// quote.
fn unquote_ansi_c(raw: &[u8], start: usize, out: &mut Vec<u8>) -> Option<usize> {
    let mut index = start;
    loop {
        match *raw.get(index)? {
            b'\'' => {
                // When: the byte at `index` is the closing quote, the body ends after it.
                return Some(index + 1);
            }
            b'\\' => {
                // When: the byte at `index` is a backslash, the next byte names the escape; a
                // missing or unknown one drops the text.
                let escape = *raw.get(index + 1)?;
                index += 2;
                let byte = match escape {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'\\' | b'\'' | b'"' => escape,
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'e' | b'E' => 0x1b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'v' => 0x0b,
                    b'x' => {
                        let (value, used) = escape_digits(&raw[index..], 16, 2)?;
                        index += used;
                        value
                    }
                    b'0'..=b'7' => {
                        // The first octal digit is the escape itself, so it is read again.
                        index -= 1;
                        let (value, used) = escape_digits(&raw[index..], 8, 3)?;
                        index += used;
                        value
                    }
                    _ => {
                        // When: `escape` is not one bash's `%q` writes, the text is dropped.
                        return None;
                    }
                };
                out.push(byte);
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
}

/// Read one to `max_digits` digits in `radix` from the start of `bytes` as one byte value; `None`
/// with no digit or a value past 255. Returns the value and the digits used.
fn escape_digits(bytes: &[u8], radix: u32, max_digits: usize) -> Option<(u8, usize)> {
    let digits = bytes
        .iter()
        .take(max_digits)
        .map_while(|byte| char::from(*byte).to_digit(radix))
        .collect::<Vec<u32>>();
    if digits.is_empty() {
        // When: no `digits` follow the escape, it is malformed.
        return None;
    }
    let value = digits.iter().fold(0u32, |value, digit| value * radix + digit);
    Some((u8::try_from(value).ok()?, digits.len()))
}

#[cfg(test)]
#[path = "vt_semantic_tests.rs"]
mod vt_semantic_tests;
