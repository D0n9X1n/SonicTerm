//! Prompt, sentinel, parked-row and wide-token delivery evidence.

use super::*;

/// The character typed at `index`.
pub(crate) fn typed_character(index: u32) -> char {
    char::from(TYPED_SYMBOLS[index as usize % TYPED_SYMBOLS.len()])
}

/// The scrollback-absolute row and the column just past `prompt`, when the cursor sits right
/// after it at the start of the cursor's row; `None` otherwise.
pub(crate) fn prompt_origin(grid: &Grid, prompt: &str) -> Option<(u64, u16)> {
    let prompt_cols = u16::try_from(prompt.chars().count()).ok()?;
    if grid.cursor.col != prompt_cols {
        // When: the cursor is not just past a prompt-sized prefix, typing has not begun here.
        return None;
    }
    let row = grid.row(grid.cursor.row);
    let shows_prompt = prompt
        .chars()
        .enumerate()
        .all(|(index, expected)| row.get(index).is_some_and(|cell| cell.ch == expected));
    if !shows_prompt {
        // When: the row does not start with the prompt, the cursor is after other text.
        return None;
    }
    Some((grid.scrollback_len() as u64 + u64::from(grid.cursor.row), prompt_cols))
}

/// Where typed character `index` echoes when typing starts at `origin` in a grid `cols` wide.
pub(crate) fn echo_target(origin: (u64, u16), cols: u16, index: u32) -> EchoTarget {
    let linear = u64::from(origin.1) + u64::from(index);
    let width = u64::from(cols.max(1));
    EchoTarget {
        abs_row: origin.0 + linear / width,
        col: (linear % width) as u16,
        character: typed_character(index),
    }
}

/// The identity `grid` holds now; the harness reads it with the prompt origin, under one guard.
pub(crate) fn prompt_identity(grid: &Grid) -> RowIdentity {
    RowIdentity {
        scrollback_evicted: grid.scrollback_evicted(),
        screen_epoch: grid.screen_epoch(),
        size_generation: grid.size_generation(),
    }
}

/// Whether `target`'s character is in `grid`, and the revision and identity it was read under.
pub(crate) fn snapshot_echo(grid: &Grid, target: &EchoTarget) -> EchoSnapshot {
    let present = grid
        .row_at_abs(target.abs_row)
        .and_then(|row| row.get(usize::from(target.col)))
        .is_some_and(|cell| cell.ch == target.character);
    EchoSnapshot::Read { present, revision: grid.revision(), identity: prompt_identity(grid) }
}

/// Whether either snapshot of a dispatch read a row identity other than `armed`; the harness's own
/// check, which catches a change the worker never saw (an event-loop resize, for one).
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn snapshot_identity_changed(
    observation: &DispatchObservation,
    armed: RowIdentity,
) -> bool {
    [observation.before, observation.after].iter().any(
        |snapshot| matches!(snapshot, EchoSnapshot::Read { identity, .. } if *identity != armed),
    )
}

/// The arm token's number when the sample was armed; never invented otherwise.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn armed_token(arm: ArmState) -> Option<u64> {
    match arm {
        ArmState::Armed(token) => Some(echo_api::token_number(token)),
        ArmState::OutOfScope | ArmState::Unsupported | ArmState::Failed(_) => None,
    }
}

/// Whether a visible row from the cursor's row up to `rows_above` rows above it starts with `text`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn line_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> bool {
    line_row_near_cursor(grid, text, rows_above).is_some()
}

/// The rows lines of these display `widths` fill in a grid `cols` wide: a line wraps onto
/// ceil(width / cols) rows, and an empty line or one exactly `cols` wide takes one, because the
/// terminal defers the wrap until another character arrives.
pub(crate) fn planned_rows(widths: impl IntoIterator<Item = usize>, cols: u16) -> u64 {
    let cols = usize::from(cols.max(1));
    widths.into_iter().map(|width| width.div_ceil(cols).max(1) as u64).sum()
}

/// The `result.json` note for a failed LockSetForegroundWindow, naming its `error`. The run goes on,
/// and the comparison's foreground rule still judges whether the window took the foreground.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn foreground_lock_note(error: &str) -> String {
    format!(
        "LockSetForegroundWindow failed: {error}; the window may take the foreground as it opens, \
         and the comparison's foreground rule still judges the run"
    )
}

/// Rows above the cursor scanned for a READY line or a sentinel. zsh's prompt adds one row; on
/// Windows cmd.exe prints its two-line banner and a blank line before its first prompt, so the scan
/// reaches 8 rows. The sentinel carries the run's nonce and READY is all a role prints before GO,
/// so the wider scan matches no other line.
pub(crate) fn protocol_rows(host: Host) -> u16 {
    match host {
        Host::Posix => 3,
        Host::Windows => 8,
    }
}

/// The lifetime-absolute number of the first visible row, from `rows_above` rows above the
/// cursor's row down to it, that starts with `text`: rows the grid ever evicted, plus its
/// scrollback, plus the visible row. The number never changes once the row is printed.
pub(crate) fn line_row_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> Option<u64> {
    let cursor_row = grid.cursor.row;
    let found = (cursor_row.saturating_sub(rows_above)..=cursor_row).find(|row| {
        let line = grid.row(*row);
        text.chars()
            .enumerate()
            .all(|(index, expected)| line.get(index).is_some_and(|cell| cell.ch == expected))
    })?;
    Some(grid.scrollback_evicted() + grid.scrollback_len() as u64 + u64::from(found))
}

/// The lifetime-absolute number of the first displayed row that starts with `text`, searched over
/// the whole screen rather than near the cursor.
pub(crate) fn line_row_on_screen(grid: &Grid, text: &str) -> Option<u64> {
    let found = (0..grid.rows).find(|row| {
        let line = grid.row(*row);
        text.chars()
            .enumerate()
            .all(|(index, expected)| line.get(index).is_some_and(|cell| cell.ch == expected))
    })?;
    Some(grid.scrollback_evicted() + grid.scrollback_len() as u64 + u64::from(found))
}

/// Where `workload`'s completion sentinel `text` is, on `host`. Most workloads print the sentinel
/// last, so it lies a few rows above the cursor. S6/rest prints it on a fixed fixture row and then
/// moves the cursor to the bottom of its streaming region, possibly in the same parser batch, so its
/// sentinel is searched over the whole screen; the run's nonce makes that line unique.
pub(crate) fn sentinel_row(grid: &Grid, text: &str, workload: Workload, host: Host) -> Option<u64> {
    if workload == Workload::RestStream {
        // When: `workload` is RestStream, its sentinel row is fixed while the cursor has moved away.
        return line_row_on_screen(grid, text);
    }
    line_row_near_cursor(grid, text, protocol_rows(host))
}

impl ParkSnapshot {
    /// The evidence a parked phase leaves when it started at `self` and ended at `end`.
    pub(crate) fn evidence(
        &self,
        end: &Self,
        presented_frames: u64,
        hover: Option<HoverDeltas>,
    ) -> ParkEvidence {
        ParkEvidence {
            presented_frames,
            streamed_rows_advanced: end.content_seq > self.content_seq,
            screen_unchanged: (self.identity, self.scrollback_len, self.alt_screen, self.viewport)
                == (end.identity, end.scrollback_len, end.alt_screen, end.viewport),
            fixture_unchanged: self.fixture_stamps == end.fixture_stamps,
            pointed_row_changed: self.pointed_stamp != end.pointed_stamp,
            hover,
        }
    }
}

/// Snapshot role 0's `grid`, shown at `viewport`, with the pointer resting on displayed row `pointed_row`.
pub(crate) fn park_snapshot(grid: &Grid, viewport: Option<u64>, pointed_row: u16) -> ParkSnapshot {
    let live_top = grid.scrollback_len() as u64;
    let stamp = |row: u16| grid.row_content_seq_at_abs(live_top + u64::from(row));
    ParkSnapshot {
        identity: prompt_identity(grid),
        scrollback_len: grid.scrollback_len(),
        alt_screen: grid.is_alt(),
        viewport,
        content_seq: grid.content_seq(),
        fixture_stamps: (0..REST_REGION_TOP - 1).map(stamp).collect(),
        pointed_row,
        pointed_stamp: stamp(pointed_row),
    }
}

/// Displayed row `row` of `grid` as text, trailing blanks removed; empty past the screen.
pub(super) fn displayed_row_text(grid: &Grid, row: u16) -> String {
    if row >= grid.rows {
        // When: `row` is past the screen, there is no text to read.
        return String::new();
    }
    let text: String = grid.row(row).iter().map(|cell| cell.ch).collect();
    text.trim_end().to_owned()
}

/// Why a parked S6/rest phase cannot measure what it claims, checked as it starts; `None` when it
/// can. The fixture must be displayed on the live primary screen (the URL, the target's path, a
/// blank row, then `sentinel`), and the pointer must resolve to `expected`, the phase's cell:
/// a non-blank path cell for `warmup` and `rest`, a streaming-region row for `tail`. `pointed` is
/// the cell of role 0's pane the parked point resolves to, `None` when it resolves to none.
pub(crate) fn park_entry_problem(
    phase: &str,
    grid: &Grid,
    viewport: Option<u64>,
    sentinel: &str,
    pointed: Option<(u16, u16)>,
    expected: (u16, u16),
) -> Option<String> {
    if grid.is_alt() {
        // When: the alternate screen is up, the fixture's primary-screen rows are not displayed.
        return Some(format!("S6/rest phase {phase} started on the alternate screen"));
    }
    if viewport.is_some_and(|top| top < grid.scrollback_len() as u64) {
        // When: the view is scrolled back, the displayed rows are history, not the fixture.
        return Some(format!("S6/rest phase {phase} started scrolled back"));
    }
    let fixture = [
        displayed_row_text(grid, 0) == REST_URL,
        displayed_row_text(grid, REST_PATH_ROW).ends_with(REST_TARGET_NAME),
        displayed_row_text(grid, REST_PATH_ROW + 1).is_empty(),
        displayed_row_text(grid, REST_PATH_ROW + 2) == sentinel,
    ];
    if fixture.contains(&false) {
        // When: any fixture row differs from what the workload printed, the scene is not the fixture.
        return Some(format!("S6/rest phase {phase} does not display its fixture"));
    }
    let (expected_row, expected_col) = expected;
    if expected_row >= grid.rows || expected_col >= grid.cols {
        // When: the phase's cell lies outside the grid, no pointer can rest on it.
        return Some(format!("S6/rest phase {phase} parks outside the grid at {expected:?}"));
    }
    if pointed != Some(expected) {
        // When: `pointed` is not the phase's cell, the pointer rests somewhere else, or nowhere.
        return Some(format!(
            "S6/rest phase {phase} pointer resolves to {pointed:?}, not role 0's cell {expected:?}"
        ));
    }
    let on_target = if phase == "tail" {
        expected_row >= REST_REGION_TOP - 1
    } else {
        // When: the phase is warmup or rest, the pointer must rest on the path's text.
        expected_row == REST_PATH_ROW
            && grid
                .row(expected_row)
                .get(usize::from(expected_col))
                .is_some_and(|cell| cell.ch != ' ')
    };
    // When: `on_target` is false, the pointed cell is not the one the phase measures.
    (!on_target)
        .then(|| format!("S6/rest phase {phase} pointer cell {expected:?} is not its target"))
}

/// Why the rows strictly between the READY row and the sentinel's row are not the `planned_rows`
/// the workload's lines fill at the pane's width, naming both counts; `None` when they match.
pub(crate) fn row_count_mismatch(
    ready_row: u64,
    sentinel_row: u64,
    planned_rows: u64,
) -> Option<String> {
    let delivered = sentinel_row.saturating_sub(ready_row).saturating_sub(1);
    (delivered != planned_rows).then(|| {
        format!(
            "{delivered} rows lie between READY (row {ready_row}) and the sentinel (row {sentinel_row}), \
             but the workload's lines fill {planned_rows} rows"
        )
    })
}

/// The lifetime-absolute row where the logical line ending on `last` starts, walking up across
/// soft wraps; `None` when any of its rows has left the retained history.
pub(super) fn logical_line_start(grid: &Grid, last: u64) -> Option<u64> {
    let evicted = grid.scrollback_evicted();
    let mut first = last;
    loop {
        let row = grid.row_at_abs(first.checked_sub(evicted)?)?;
        if !row.soft_wrapped_from_previous() {
            // When: this row starts its line, the walk ends here.
            return Some(first);
        }
        first = first.checked_sub(1)?;
    }
}

/// The logical line on lifetime-absolute rows `first..=last`, joined across soft wraps as a copy
/// joins them, trailing blanks trimmed.
pub(super) fn logical_line_text(grid: &Grid, first: u64, last: u64) -> String {
    let evicted = grid.scrollback_evicted();
    let end = usize::from(grid.cols).saturating_sub(1);
    let range = ((0, first.saturating_sub(evicted)), (end, last.saturating_sub(evicted)));
    plain_text_from_grid_range(grid, range.0, range.1).trim_end().to_owned()
}

/// Why the retained lines above the sentinel's row are not the end of `tail`, the last lines of the
/// workload's fixture, naming the first line that differs; `None` when every retained line matches.
/// A line that wrapped is read whole, so the check holds at any grid width.
pub(crate) fn bulk_tail_mismatch(grid: &Grid, sentinel_row: u64, tail: &[&str]) -> Option<String> {
    // The first row of the line checked last; the walk starts at the sentinel's own row.
    let mut below = sentinel_row;
    for (offset, expected) in tail.iter().rev().enumerate() {
        // A line whose rows have left the retained history ends the check: every line before matched.
        let last = below.checked_sub(1)?;
        let first = logical_line_start(grid, last)?;
        let actual = logical_line_text(grid, first, last);
        if actual != expected.trim_end() {
            return Some(format!(
                "the line on rows {first} to {last}, {} above the sentinel, reads {actual:?}, not the fixture's {expected:?}",
                offset + 1
            ));
        }
        below = first;
    }
    None
}

/// Every retained row, scrollback first, as the selection copy path reads them.
pub(crate) fn retained_text(grid: &Grid) -> String {
    let last_row = grid.scrollback_len() as u64 + u64::from(grid.rows).saturating_sub(1);
    let end = usize::from(grid.cols).saturating_sub(1);
    plain_text_from_grid_range(grid, (0, 0), (end, last_row))
}

/// The distinct whitespace-separated tokens of `text` that hold a non-ASCII character, in order.
pub(crate) fn wide_tokens(text: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = Vec::new();
    for token in text.split_whitespace() {
        if !token.is_ascii() && !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

/// The `tokens` that `text` does not contain, in order.
pub(crate) fn missing_wide_tokens<'token>(text: &str, tokens: &[&'token str]) -> Vec<&'token str> {
    tokens.iter().copied().filter(|token| !text.contains(token)).collect()
}

#[cfg(test)]
#[path = "record_grid_tests.rs"]
mod record_grid_tests;
