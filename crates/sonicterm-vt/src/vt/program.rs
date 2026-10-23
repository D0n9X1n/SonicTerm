//! Program-state OSC grammar: ConEmu `OSC 9` (progress subcommand and plain
//! notifications) and `OSC 777 ; notify`.
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
    /// Percent complete, `0..=100`; 0 for indeterminate.
    pub percent: u8,
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
    /// Malformed, a ConEmu subcommand other than `4`, or another OSC 777 subcommand.
    Ignored,
}

/// Parse the parameters after `9`.
pub(super) fn parse_osc9(params: &[&[u8]]) -> ProgramOsc {
    let Some(first) = params.first() else {
        // When: params has no first parameter, there is neither a subcommand nor text.
        return ProgramOsc::Ignored;
    };
    if let Some(subcommand) = conemu_subcommand(first) {
        // When: conemu_subcommand names 1..=12, it is never a notification; only 4 is implemented.
        return if subcommand == 4 {
            parse_progress(&params[1..])
        } else {
            // When: subcommand is not 4, that ConEmu command is not implemented.
            ProgramOsc::Ignored
        };
    }
    let Some(body) = joined_text(params) else {
        // When: joined_text finds no UTF-8 text, nothing can be shown.
        return ProgramOsc::Ignored;
    };
    ProgramOsc::Notify { title: None, body }
}

/// Parse the parameters after `777`: only `notify ; <title> ; <body…>` is accepted, and further
/// `;`-separated parameters rejoin the body.
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
    let [title, body @ ..] = parts.as_slice() else {
        // When: parts is empty, `notify` has neither a title nor a body separator.
        return ProgramOsc::Ignored;
    };
    if body.is_empty() {
        // When: body is empty, the title has no `;` after it, so the sequence is malformed.
        return ProgramOsc::Ignored;
    }
    let title = (*title).to_owned();
    let body = body.join(";");
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

/// `9;4;<st>[;<pr>][;…]`: `st` is one digit `0`–`4`, `pr` one to three digits clamped to 100.
fn parse_progress(params: &[&[u8]]) -> ProgramOsc {
    let state = match params.first().copied() {
        Some(b"0") => None,
        Some(b"1") => Some(ProgressState::Normal),
        Some(b"2") => Some(ProgressState::Error),
        Some(b"3") => Some(ProgressState::Indeterminate),
        Some(b"4") => Some(ProgressState::Paused),
        _ => {
            // When: params has no state, or it is not one digit 0-4, the report is malformed.
            return ProgramOsc::Ignored;
        }
    };
    let percent = match params.get(1).copied() {
        None => 0,
        Some(digits)
            if (1..=3).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit) =>
        {
            let value =
                digits.iter().fold(0u16, |value, digit| value * 10 + u16::from(digit - b'0'));
            value.min(100) as u8
        }
        Some(_) => {
            // When: params holds a fractional, signed, empty or four-digit percent, it is malformed.
            return ProgramOsc::Ignored;
        }
    };
    ProgramOsc::Progress(state.map(|state| ProgressReport {
        state,
        percent: if state == ProgressState::Indeterminate { 0 } else { percent },
    }))
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
