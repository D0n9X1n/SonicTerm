//! `search_pane`: a regex search back through a pane's retained history; `command_history`: the
//! pane's recent shell commands with their exit codes and durations.
//!
//! One call copies at most [`MAX_CELLS_PER_READ`] cells under one bounded parser attempt, with a
//! [`SEARCH_BYTES`] text budget and the history reader's whole-row exception, then matches after
//! the guard drops. A long logical line is matched on copied parts with one reread row of newer
//! context; match starts belong before the cursor, and incomplete lines are reported `cut`.

use parking_lot::Mutex;
use serde_json::{json, Value};
use sonicterm_grid::grid::{CommandRegion, Grid, RowToken};
use sonicterm_vt::vt::Parser;

use super::super::jsonrpc::{tool_error, tool_result};
use super::super::read::{
    read_pane, row_cells_text_from, ReadLimits, ReadMode, MAX_CELLS_PER_READ, PARSER_WAIT,
};
use super::super::{CallScope, McpShared, PaneEntry};
use super::{decode_token, encode_token, regex_arg, resolve};

/// The text budget under the parser; an otherwise empty page may exceed it by one whole row.
pub(crate) const SEARCH_BYTES: usize = 256 * 1024;
/// The matches a call returns when `max_results` is absent.
pub(crate) const DEFAULT_RESULTS: usize = 20;
/// The most matches one call returns.
pub(crate) const MAX_RESULTS: usize = 100;
/// The longest matched line returned; a longer one is shortened around the match and marked cut.
pub(crate) const MAX_MATCH_LINE_BYTES: usize = 1024;

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchMatch {
    /// The first row of the matched line; as `before_row` it reads the lines before the match.
    pub(crate) row: RowToken,
    /// The line, or a window of it around the match.
    pub(crate) line: String,
    /// `line` is only part of the logical line.
    pub(crate) cut: bool,
}

/// What one search call found and where the next one ends.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SearchPage {
    /// Matches, newest first.
    pub(crate) matches: Vec<SearchMatch>,
    /// The first row of the oldest line examined; pass as `before_row`. A split logical line
    /// rereads this row as context, without accepting match starts in it.
    pub(crate) next_before_row: Option<RowToken>,
    /// Every retained row before the cursor was examined.
    pub(crate) scan_complete: bool,
    /// The copy reached the oldest retained row.
    pub(crate) at_start: bool,
    /// The parser could not be taken; nothing was examined and the cursor is unchanged.
    pub(crate) busy: bool,
    /// The cursor could not start a search.
    pub(crate) refused: Option<super::super::read::HistoryRefusal>,
}

/// Search the lines before `before` (the newest lines when absent), newest first, for at most
/// `max_results` matches. The parser is held only for the copy; the regex runs after it drops.
pub(crate) fn search_history(
    parser: &std::sync::Arc<Mutex<Parser>>,
    regex: &regex::Regex,
    max_results: usize,
    before: Option<RowToken>,
) -> SearchPage {
    // Each line costs at least one byte, so the byte cap bounds the line count.
    let limits = ReadLimits { lines: usize::MAX, bytes: SEARCH_BYTES };
    let snapshot = read_pane(parser, ReadMode::Search { before }, limits);
    if snapshot.busy || snapshot.refused.is_some() {
        // When: the parser was `busy` or the cursor `refused`, nothing was examined.
        return SearchPage {
            next_before_row: before,
            busy: snapshot.busy,
            refused: snapshot.refused,
            ..SearchPage::default()
        };
    }
    let mut page = SearchPage {
        at_start: snapshot.at_start,
        next_before_row: snapshot.first_row,
        ..SearchPage::default()
    };
    for line in snapshot.lines.iter().rev() {
        if line.text.is_empty() {
            // When: the line's text is empty, it is never matched, as in `wait_for`.
            continue;
        }
        let Some(found) = regex.find(&line.text) else {
            // When: the regex finds nothing on this copied line, move to the older one.
            continue;
        };
        // shortcut: one-row context covers boundary matches, not regexes needing still newer rows.
        if before == Some(line.row) && found.start() >= line.newest_row_start {
            // When: the match starts in the reread row, its owning call already examined it.
            continue;
        }
        // The reread row is context only: include its prefix only when the match reaches into it.
        let overlap = before == Some(line.row);
        let end = if overlap {
            line.newest_row_start.max(found.end())
        } else {
            // When: no overlap row was reread, the entire copied line belongs to this call.
            line.text.len()
        };
        let (text, clipped) = clip_match(&line.text[..end], found.start());
        page.matches.push(SearchMatch {
            row: line.first_row,
            line: text,
            cut: line.cut || clipped || overlap,
        });
        if page.matches.len() >= max_results {
            // When: `max_results` is reached, resume before this line's owned rows, retaining any
            // overlap so older matches are neither skipped nor duplicated.
            page.next_before_row = Some(line.first_row);
            page.scan_complete = false;
            return page;
        }
    }
    page.scan_complete = snapshot.at_start;
    page
}

/// `text` shortened to at most [`MAX_MATCH_LINE_BYTES`] around `match_start`, on character
/// boundaries, and whether it was shortened.
pub(crate) fn clip_match(text: &str, match_start: usize) -> (String, bool) {
    if text.len() <= MAX_MATCH_LINE_BYTES {
        // When: the whole `text` fits, it is returned unshortened.
        return (text.to_owned(), false);
    }
    // A quarter of the window precedes the match, as context; the window stays full near the end.
    let lead = MAX_MATCH_LINE_BYTES / 4;
    let mut start =
        match_start.saturating_sub(lead).min(text.len().saturating_sub(MAX_MATCH_LINE_BYTES));
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + MAX_MATCH_LINE_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[start..end].to_owned(), true)
}

/// The `max_results` argument, defaulted and clamped to 1 through [`MAX_RESULTS`].
pub(crate) fn max_results_arg(arguments: &Value) -> usize {
    arguments.get("max_results").and_then(Value::as_u64).map_or(DEFAULT_RESULTS, |count| {
        usize::try_from(count).unwrap_or(MAX_RESULTS).clamp(1, MAX_RESULTS)
    })
}

/// The `before_row` argument decoded, `None` when absent.
pub(crate) fn before_row_arg(arguments: &Value) -> Result<Option<RowToken>, Value> {
    let Some(text) = arguments.get("before_row").and_then(Value::as_str) else {
        // When: no before_row `text` was given, the page or search starts at the newest line.
        return Ok(None);
    };
    decode_token(text)
        .map(Some)
        .ok_or_else(|| tool_error("invalid_arguments", "before_row is not a row token"))
}

/// `search_pane`: resolves the pane under the read scope, then searches it.
pub(crate) fn search_pane_tool(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    match resolve(shared, arguments, scope) {
        Ok(entry) => search_reply(&entry, arguments),
        // Resolving the pane failed: its tool error is the result.
        Err(error) => error,
    }
}

/// Search the resolved `entry` as `arguments` ask and build the tool result.
pub(crate) fn search_reply(entry: &PaneEntry, arguments: &Value) -> Value {
    let regex = match regex_arg(arguments) {
        Ok(Some(regex)) => regex,
        Ok(None) => {
            // When: `regex_arg` returned `None`, no regex was given and there is nothing to find.
            return tool_error("invalid_arguments", "search_pane needs a regex");
        }
        Err(error) => {
            // When: `regex_arg` refused the pattern, its tool `error` is the result.
            return error;
        }
    };
    let before = match before_row_arg(arguments) {
        Ok(before) => before,
        Err(error) => {
            // When: `before_row_arg` could not decode the token, its tool `error` is the result.
            return error;
        }
    };
    let page = search_history(&entry.parser, &regex, max_results_arg(arguments), before);
    if let Some(refusal) = page.refused {
        // When: the cursor was `refused` (other screen or evicted), its code is the result.
        return tool_error(refusal.as_str(), refusal.message());
    }
    let matches: Vec<Value> = page
        .matches
        .iter()
        .map(
            |found| json!({ "row": encode_token(found.row), "line": found.line, "cut": found.cut }),
        )
        .collect();
    let summary = format!("{} match(es){}", matches.len(), if page.busy { ", busy" } else { "" });
    tool_result(
        &summary,
        json!({
            "pane_id": entry.pane_id.to_string(),
            "matches": matches,
            "next_before_row": page.next_before_row.map(encode_token),
            "scan_complete": page.scan_complete,
            "at_start": page.at_start,
            "busy": page.busy,
        }),
    )
}

/// The commands `command_history` lists when `limit` is absent.
pub(crate) const DEFAULT_COMMANDS: usize = 10;
/// The most commands one call lists.
pub(crate) const MAX_COMMANDS: usize = 50;

/// The longest command text returned; a longer one is cut on a character boundary.
pub(crate) const MAX_COMMAND_BYTES: usize = 1024;

/// One listed command, copied out of the pane before serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandEntry {
    /// The region's identity; `read_pane` `last_command` with it reads the command's output.
    pub(crate) seq: u64,
    /// The typed text from B to C; `None` without a B, once B's row left history, or when the
    /// shared cell budget ran out first.
    pub(crate) command: Option<String>,
    /// The code the latest D reported.
    pub(crate) exit_code: Option<i32>,
    /// The latest D carried the pane's shell token.
    pub(crate) exit_authenticated: bool,
    /// Milliseconds from C to the latest D; `None` when either time is missing.
    pub(crate) duration_ms: Option<u64>,
    /// The output's first row left history.
    pub(crate) output_truncated: bool,
}

/// What one `command_history` call listed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CommandList {
    /// Settled commands, newest first.
    pub(crate) commands: Vec<CommandEntry>,
    /// Some entry's text was left out because the shared cell budget ran out, or the parser was
    /// busy and nothing was listed.
    pub(crate) partial: bool,
    /// The parser could not be taken within [`PARSER_WAIT`].
    pub(crate) busy: bool,
}

/// List the primary screen's newest `limit` settled commands. The parser is taken once, for at
/// most [`PARSER_WAIT`]; every entry's text spends one budget of [`MAX_CELLS_PER_READ`] cells.
// Only the pane parser is taken, once; no image store or other lock is held with it.
pub(crate) fn list_commands(parser: &std::sync::Arc<Mutex<Parser>>, limit: usize) -> CommandList {
    let Some(guard) = parser.try_lock_for(PARSER_WAIT) else {
        // When: `try_lock_for` misses within `PARSER_WAIT`, nothing is listed and the call is busy.
        return CommandList { commands: Vec::new(), partial: true, busy: true };
    };
    let grid = guard.grid();
    // Regions and rows both belong to the primary screen, saved away while the alternate is up.
    let primary = grid.primary_screen();
    let mut cells_left = MAX_CELLS_PER_READ;
    let mut list = CommandList::default();
    for region in grid.command_regions(limit) {
        let command = match command_text(primary, &region, &mut cells_left) {
            Ok(command) => command,
            Err(BudgetSpent) => {
                // The shared budget is spent: this and every older entry lists without its text.
                list.partial = true;
                None
            }
        };
        let duration_ms = region
            .end_at
            .zip(region.output_start_at)
            .map(|(end, start)| end.saturating_duration_since(start).as_millis())
            .map(|millis| u64::try_from(millis).unwrap_or(u64::MAX));
        list.commands.push(CommandEntry {
            seq: region.seq,
            command,
            exit_code: region.exit_code,
            exit_authenticated: region.end_authenticated,
            duration_ms,
            output_truncated: region.output_start_row.is_none(),
        });
    }
    list
}

/// The shared cell budget ran out before a command's rows were copied.
struct BudgetSpent;

/// The typed text of `region` on `primary`: from B's column up to C's column on C's row (B's row
/// alone once C's row left history), soft wraps joined with their blanks, hard breaks kept as line
/// feeds, trailing blanks trimmed, at most [`MAX_COMMAND_BYTES`]. Each row's cells are charged to
/// `cells_left` before it is copied; `Ok(None)` when B is missing or its row left history.
fn command_text(
    primary: &Grid,
    region: &CommandRegion,
    cells_left: &mut usize,
) -> Result<Option<String>, BudgetSpent> {
    let Some((first_row, start_col)) = region.input_start else {
        // When: the region has no `input_start`, there is no typed text to read.
        return Ok(None);
    };
    // The span ends at C: on C's row only the cells before C's column are typed text. A C at
    // column 0 of a later row leaves that row out, so it adds no line break.
    let (last_row, last_row_end) = match region.output_start_row {
        Some(row) if row > first_row && region.output_start_col == 0 => (row - 1, None),
        Some(row) => (row.max(first_row), Some(usize::from(region.output_start_col))),
        None => (first_row, None),
    };
    let mut text = String::new();
    // Blank cells after the previous row's text, kept only when the next row wraps from it.
    let mut pending_blanks = 0usize;
    for abs in first_row..=last_row {
        let Some(row) = primary.row_at_abs(abs) else {
            // When: `row_at_abs` has no row at `abs`, the span ended early.
            break;
        };
        if row.len() > *cells_left {
            // When: the row's cells exceed `cells_left`, the shared budget is spent.
            *cells_left = 0;
            return Err(BudgetSpent);
        }
        *cells_left -= row.len();
        if abs > first_row {
            if row.soft_wrapped_from_previous() {
                text.extend(std::iter::repeat_n(' ', pending_blanks));
            } else {
                // When: the row does not wrap from the previous one, a hard break separates them.
                text.push('\n');
            }
        }
        let start = if abs == first_row {
            usize::from(start_col)
        } else {
            // When: `abs` is a row after B's, the typed text continues from its first column.
            0
        };
        let room = MAX_COMMAND_BYTES.saturating_sub(text.len());
        let end = if let Some(c_col) = last_row_end.filter(|_| abs == last_row) {
            c_col.min(row.len())
        } else {
            // When: `abs` is not C's row, or C's column is not known, the whole row is typed text.
            row.len()
        };
        let copy = row_cells_text_from(row, start, end, room);
        text.push_str(&copy.text);
        pending_blanks = copy.trailing_blanks;
        if copy.cut || text.len() >= MAX_COMMAND_BYTES {
            // When: the copy was `cut` at the byte cap, the rest of the command is left out.
            break;
        }
    }
    text.truncate(text.trim_end().len());
    // Padding or a line feed may have passed the cap; cut back to a character boundary.
    let mut end = text.len().min(MAX_COMMAND_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    Ok(Some(text))
}

/// The `limit` argument, defaulted and clamped to 1 through [`MAX_COMMANDS`].
pub(crate) fn limit_arg(arguments: &Value) -> usize {
    arguments.get("limit").and_then(Value::as_u64).map_or(DEFAULT_COMMANDS, |count| {
        usize::try_from(count).unwrap_or(MAX_COMMANDS).clamp(1, MAX_COMMANDS)
    })
}

/// The `seq` argument of `read_pane` `last_command`, `None` when absent.
pub(crate) fn seq_arg(arguments: &Value) -> Result<Option<u64>, Value> {
    match arguments.get("seq") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| tool_error("invalid_arguments", "seq is not a non-negative integer")),
    }
}

/// `command_history`: resolves the pane under the read scope, then lists its commands.
pub(crate) fn command_history_tool(
    shared: &McpShared,
    arguments: &Value,
    scope: &CallScope,
) -> Value {
    match resolve(shared, arguments, scope) {
        Ok(entry) => commands_reply(&entry, arguments),
        // Resolving the pane failed: its tool error is the result.
        Err(error) => error,
    }
}

/// List the resolved `entry`'s recent commands as `arguments` ask and build the tool result,
/// serialized after the parser was released.
pub(crate) fn commands_reply(entry: &PaneEntry, arguments: &Value) -> Value {
    let list = list_commands(&entry.parser, limit_arg(arguments));
    let commands: Vec<Value> = list
        .commands
        .iter()
        .map(|command| {
            json!({
                "seq": command.seq,
                "command": command.command,
                "exit_code": command.exit_code,
                "exit_authenticated": command.exit_authenticated,
                "duration_ms": command.duration_ms,
                "output_truncated": command.output_truncated,
            })
        })
        .collect();
    let summary = format!("{} command(s){}", commands.len(), if list.busy { ", busy" } else { "" });
    tool_result(
        &summary,
        json!({
            "pane_id": entry.pane_id.to_string(),
            "commands": commands,
            "partial": list.partial,
            "busy": list.busy,
        }),
    )
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

#[cfg(test)]
#[path = "history_commands_tests.rs"]
mod history_commands_tests;
