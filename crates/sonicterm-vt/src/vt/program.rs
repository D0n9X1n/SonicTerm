//! Program-state OSC grammar: ConEmu `OSC 9` (progress subcommand, the `9;9` working-directory
//! split-off and plain notifications) and `OSC 777 ; notify`.
//!
//! The parser only validates and bounds these strings; it never sanitizes, cuts,
//! rate-limits or holds config. The host sanitizes first, then applies its display caps.

/// What an `OSC 9;4` report asks the platform progress surfaces to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressState {
    /// `1`: ordinary progress.
    Normal,
    /// `2`: the program reports an error.
    Error,
    /// `3`: work with no known fraction.
    Indeterminate,
    /// `4`: the work is paused.
    Paused,
}

/// One active `OSC 9;4` report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressReport {
    /// The reported state.
    pub state: ProgressState,
    /// Percent complete, `0..=100`; `None` when the report carried none it could read (always
    /// for indeterminate). `None` keeps the state; the host shows the pane's last value, or 0.
    pub percent: Option<u8>,
}

/// Raw safety bound on one program-state sequence's payload. A longer one is refused whole,
/// never cut, so the host always sanitizes the complete text before its own display caps.
pub const MAX_NOTIFY_BYTES: usize = 16 * 1024;

/// What one program-state OSC asks of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProgramOsc {
    /// `9;4`: show this progress, or clear it on `None`.
    Progress(Option<ProgressReport>),
    /// `9;<text>` or `777;notify;<title>;<body>`.
    Notify {
        /// The OSC 777 title; OSC 9 has none.
        title: Option<String>,
        /// The text.
        body: String,
    },
    /// `9;12`: ConEmu's prompt start, whatever follows it.
    PromptMark,
    /// Malformed, a ConEmu subcommand other than `4` or `12`, or another OSC 777 subcommand.
    Ignored,
}

/// Parse the parameters after `9`.
pub(super) fn parse_osc9(params: &[&[u8]]) -> ProgramOsc {
    let Some(first) = params.first() else {
        // When: params has no first parameter, there is neither a subcommand nor text.
        return ProgramOsc::Ignored;
    };
    if let Some(subcommand) = conemu_subcommand(first) {
        // When: conemu_subcommand names 1..=12, a command with parameters and never text.
        return match (subcommand, params.len()) {
            // Ghostty reads `9;12` as a prompt start and ignores what follows it.
            (12, _) => ProgramOsc::PromptMark,
            // Bare (one parameter) the table is Ghostty's: `5` and `10` are commands, the rest text.
            (5 | 10, 1) => ProgramOsc::Ignored,
            (_, 1) => joined_text(params)
                .map_or(ProgramOsc::Ignored, |body| ProgramOsc::Notify { title: None, body }),
            (4, _) => parse_progress(&params[1..]),
            _ => ProgramOsc::Ignored,
        };
    }
    let Some(body) = joined_text(params) else {
        // When: joined_text finds no UTF-8 text, nothing can be shown.
        return ProgramOsc::Ignored;
    };
    ProgramOsc::Notify { title: None, body }
}

/// Parse the parameters after `777`: only `notify ; <title> ; <body…>` is accepted, and further
/// `;`-separated parameters rejoin the body. `notify ; <text>` with no body is body `<text>` with
/// no title, as WezTerm shows it.
pub(super) fn parse_osc777(params: &[&[u8]]) -> ProgramOsc {
    let [subcommand, rest @ ..] = params else {
        // When: params is empty, there is no subcommand.
        return ProgramOsc::Ignored;
    };
    if *subcommand != b"notify".as_slice() {
        // When: subcommand is not `notify`, other OSC 777 commands are ignored.
        return ProgramOsc::Ignored;
    }
    let Ok(parts) =
        rest.iter().map(|part| std::str::from_utf8(part)).collect::<Result<Vec<&str>, _>>()
    else {
        // When: parts holds a part that is not UTF-8, the sequence is ignored.
        return ProgramOsc::Ignored;
    };
    let (title, body) = match parts.as_slice() {
        [] => {
            // When: parts is empty, `notify` has neither a title nor a body.
            return ProgramOsc::Ignored;
        }
        // A lone `text` is the body with no title, as WezTerm shows it.
        [text] => (String::new(), (*text).to_owned()),
        [title, body @ ..] => ((*title).to_owned(), body.join(";")),
    };
    if title.len() + body.len() > MAX_NOTIFY_BYTES {
        // When: title and body pass MAX_NOTIFY_BYTES, the sequence is refused whole.
        return ProgramOsc::Ignored;
    }
    if title.trim().is_empty() && body.trim().is_empty() {
        // When: title and body are both empty, there is nothing to say.
        return ProgramOsc::Ignored;
    }
    let title = (!title.trim().is_empty()).then_some(title);
    ProgramOsc::Notify { title, body }
}

/// The path a ConEmu `9;9;<path>` report carries, everything after its second `;`, separators
/// included; `None` for any other OSC 9 payload, a bare `9;9` without the separator included.
/// The subcommand is read with its C0 controls ignored, as the notification grammar will read it,
/// and a report with a control there returns the whole payload, so the report rule refuses it.
pub(super) fn osc9_cwd_payload(content: &[u8]) -> Option<&[u8]> {
    let (subcommand, rest) = content.split_at(content.iter().position(|byte| *byte == b';')?);
    let digits: Vec<u8> = subcommand.iter().copied().filter(|byte| *byte >= 0x20).collect();
    if conemu_subcommand(&digits) != Some(9) {
        // When: conemu_subcommand reads `digits` (controls ignored) as anything but 9, no report.
        return None;
    }
    if digits.len() != subcommand.len() {
        // When: a control sits in the subcommand, keep it in the value so the report is refused.
        return Some(content);
    }
    Some(&rest[1..])
}

/// The ConEmu subcommand `param` names: an all-digit value in `1..=12`, leading zeros included.
fn conemu_subcommand(param: &[u8]) -> Option<u8> {
    if param.is_empty() || !param.iter().all(u8::is_ascii_digit) {
        // When: param is empty or not all digits, it names no subcommand.
        return None;
    }
    // Saturating, so a long digit run reads as out of range instead of wrapping into it.
    let value = param.iter().fold(0u32, |value, digit| {
        value.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
    });
    (1..=12).contains(&value).then_some(value as u8)
}

/// `9;4;<st>[;<pr>][;…]` (Ghostty): `st` is one digit `0`-`4`. Normal reads a missing `pr` as 0;
/// error and paused keep none; a parsed `pr` is clamped to 100, and an unparsable or overflowing
/// one leaves no percent for any state. Remove and indeterminate ignore `pr`.
fn parse_progress(params: &[&[u8]]) -> ProgramOsc {
    let state = match params.first().copied() {
        Some(b"0") => {
            // When: params start with `0`, the report removes the bar whatever follows.
            return ProgramOsc::Progress(None);
        }
        Some(b"1") => ProgressState::Normal,
        Some(b"2") => ProgressState::Error,
        Some(b"3") => ProgressState::Indeterminate,
        Some(b"4") => ProgressState::Paused,
        _ => {
            // When: params has no state, or it is not one digit 0-4, the report is malformed.
            return ProgramOsc::Ignored;
        }
    };
    let percent = match (state, params.get(1).copied()) {
        (ProgressState::Indeterminate, _) => None,
        (ProgressState::Normal, None) => Some(0),
        (_, digits) => digits.and_then(parse_percent),
    };
    ProgramOsc::Progress(Some(ProgressReport { state, percent }))
}

/// `digits` as a percent clamped to 100; `None` when empty, not all ASCII digits, or past `u64`.
fn parse_percent(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        // When: digits is empty or holds a sign, point or other byte, it is unparsable.
        return None;
    }
    let value = digits.iter().try_fold(0u64, |value, digit| {
        value.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
    })?;
    Some(value.min(100) as u8)
}

/// The parameters rejoined with `;` as UTF-8 text; `None` when not UTF-8, blank or past
/// [`MAX_NOTIFY_BYTES`].
fn joined_text(params: &[&[u8]]) -> Option<String> {
    let joined = params.join(b";".as_slice());
    if joined.len() > MAX_NOTIFY_BYTES {
        // When: joined passes MAX_NOTIFY_BYTES, the sequence is refused whole, never cut.
        return None;
    }
    let text = std::str::from_utf8(&joined).ok()?;
    if text.trim().is_empty() {
        // When: text is blank, there is nothing to show.
        return None;
    }
    Some(text.to_owned())
}
