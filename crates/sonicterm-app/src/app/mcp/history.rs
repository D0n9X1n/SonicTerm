//! `search_pane`: a regex search back through a pane's retained history.
//!
//! One call copies at most [`super::super::read::MAX_CELLS_PER_READ`] cells and [`SEARCH_BYTES`] of text under one
//! bounded parser attempt, the same history page `read_pane` returns, then matches after the guard
//! drops. Each logical line is matched whole; a line longer than one call is matched only on the
//! part a call copied and reported `cut`, never passed off as the whole line.

use parking_lot::Mutex;
use serde_json::{json, Value};
use sonicterm_grid::grid::RowToken;
use sonicterm_vt::vt::Parser;

use super::super::jsonrpc::{tool_error, tool_result};
use super::super::read::{read_pane, ReadLimits, ReadMode};
use super::super::{CallScope, McpShared, PaneEntry};
use super::{decode_token, encode_token, regex_arg, resolve};

/// The most bytes of text one call copies under the parser.
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
    /// The first row of the oldest line examined; the next call passes it as `before_row`.
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
    let snapshot = read_pane(parser, ReadMode::History { before }, limits);
    if snapshot.busy || snapshot.refused.is_some() {
        // When: the parser was `busy` or the cursor `refused`, nothing was examined.
        return SearchPage {
            next_before_row: before,
            busy: snapshot.busy,
            refused: snapshot.refused,
            ..SearchPage::default()
        };
    }
    let mut page = SearchPage { at_start: snapshot.at_start, ..SearchPage::default() };
    for line in snapshot.lines.iter().rev() {
        if line.text.is_empty() {
            // When: the line's text is empty, it is never matched, as in `wait_for`.
            continue;
        }
        let Some(found) = regex.find(&line.text) else {
            // When: the regex `find`s nothing on this line, the search moves to the older one.
            continue;
        };
        let (text, clipped) = clip_match(&line.text, found.start());
        page.matches.push(SearchMatch {
            row: line.first_row,
            line: text,
            cut: line.cut || clipped,
        });
        if page.matches.len() >= max_results {
            // When: `max_results` is reached, the next call ends before this match's line, so no
            // older line is skipped and this one is not examined twice.
            page.next_before_row = Some(line.first_row);
            page.scan_complete = false;
            return page;
        }
    }
    page.next_before_row = snapshot.first_row;
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

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;
