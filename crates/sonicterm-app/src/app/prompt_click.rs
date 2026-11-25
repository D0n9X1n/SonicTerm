//! Prompt click-to-move: a plain left click inside an OSC 133 shell prompt moves the shell's
//! cursor there.
//!
//! A local single-click press latches a [`PromptPress`] from the same parser snapshot as its
//! selection anchor: the screen's semantic generation and anchor row and the view top. The release
//! revalidates the press pane and tab, the screen, size and eviction identity, the semantic
//! generation and anchor, the view top, the modifiers and the input owner, then writes one click
//! report (`click_events`) or cursor keys to the press pane only, as typing. A release the terminal
//! captured, a drag, a selection, Ctrl, Alt or Super, an overlay, READONLY, a contended parser, or
//! any changed identity sends nothing.

use sonicterm_cfg::config::ClickToMove;
use sonicterm_grid::grid::{Cell, CellFlags, ClickEvents, Grid, Row, SemanticMode};
use sonicterm_vt::vt::{MouseFormat, MouseTracking, Parser};
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use super::keyboard_protocol::KeyboardSnapshot;
use super::mouse_report::{encode_mouse_report, MouseReport};
use super::selection_gesture::{PointerGesture, PointerGestureOwner};
use super::{App, PtyInputSource};

/// Most grid cells one click may visit while counting its keys.
pub(super) const MAX_VISITED_CELLS: usize = 16_384;
/// Most cursor keys one click may send.
pub(super) const MAX_MOVE_KEYS: usize = 4_096;
/// Most bytes one click may send; they go in one write.
pub(super) const MAX_MOVE_BYTES: usize = 12 * 1024;

/// The prompt a local single-click press landed on, latched from its selection anchor's snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptPress {
    /// The screen's semantic generation at the press.
    generation: u64,
    /// The semantic anchor row at the press.
    anchor_row: u64,
    /// The absolute row at the top of the view at the press.
    view_top: u64,
}

impl PromptPress {
    /// The prompt under a press with `modifiers` on `parser`'s grid showing `view_top`, or `None`
    /// on the alternate screen, before any prompt, or when the modifiers bar a prompt click.
    pub(super) fn latch(parser: &Parser, view_top: u64, modifiers: ModifiersState) -> Option<Self> {
        let grid = parser.grid();
        if grid.is_alt() || !modifiers_admit(modifiers, parser.mouse_tracking()) {
            // When: the alternate screen shows, or `modifiers` bar a prompt click.
            return None;
        }
        let semantic = grid.semantic();
        Some(Self { generation: semantic.generation, anchor_row: semantic.anchor?, view_top })
    }

    /// Whether `grid`, showing `view_top`, still shows the prompt this press latched.
    fn holds(self, grid: &Grid, view_top: u64) -> bool {
        let semantic = grid.semantic();
        !grid.is_alt()
            && semantic.generation == self.generation
            && semantic.anchor == Some(self.anchor_row)
            && view_top == self.view_top
    }
}

/// Whether a click with `modifiers` may move a prompt cursor under mouse `tracking`: no modifier
/// while the program tracks nothing, or Shift alone, which claims a click from a tracking program.
fn modifiers_admit(modifiers: ModifiersState, tracking: MouseTracking) -> bool {
    modifiers == ModifiersState::SHIFT || (modifiers.is_empty() && tracking == MouseTracking::Off)
}

impl App {
    /// Turn a released local single click into prompt click-to-move for its press pane; returns
    /// whether bytes were queued. `gesture` is the window's left-button gesture as it stood before
    /// the release consumed it, so a terminal-owned release is never intercepted.
    pub(super) fn try_prompt_click(
        &mut self,
        window_id: WindowId,
        gesture: Option<PointerGesture>,
    ) -> bool {
        let Some(gesture) = gesture.filter(|gesture| gesture.owner == PointerGestureOwner::Local)
        else {
            // When: no gesture, or the terminal owns it, the release is the program's or nobody's.
            return false;
        };
        let (Some(anchor), Some(press)) = (gesture.anchor, gesture.prompt) else {
            // When: the press latched no prompt: a drag, a multi-click, a modifier or no anchor.
            return false;
        };
        let mode = self.config.terminal.click_to_move;
        if mode == ClickToMove::Off || !self.terminal_owns_keys(window_id) {
            // When: click-to-move is `Off`, or a palette, IME, search or copy mode owns input.
            return false;
        }
        let Some(window) = self.registry.windows.get(&window_id) else {
            // When: the window closed before its release.
            return false;
        };
        if window.selection.is_some() || !window.anchor_topology_holds(anchor) {
            // When: a selection remains, or the press pane or its tab is gone or was switched.
            return false;
        }
        let Some(pane) = window.panes.get(&anchor.pane_id()) else {
            // When: the press pane is gone; the topology check already refused it.
            return false;
        };
        let Some(parser) = pane.parser.try_lock() else {
            // When: the VT worker holds the parser, skip the click rather than wait for it.
            return false;
        };
        let grid = parser.grid();
        let view_top = pane.resolved_view_top(grid);
        if !modifiers_admit(window.modifiers, parser.mouse_tracking())
            || !anchor.unmoved(grid)
            || !press.holds(grid, view_top)
        {
            // When: the release modifiers, the program's tracking, the layout or the prompt changed.
            return false;
        }
        let (abs_row, col) = anchor.cell();
        let keyboard = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
        let bytes = abs_row
            .checked_sub(view_top)
            .and_then(|row| u16::try_from(row).ok())
            .and_then(|row| prompt_click_bytes(grid, (abs_row, col), row, mode, keyboard));
        drop(parser);
        let Some(bytes) = bytes else {
            // When: no `bytes`: the click is outside the prompt, moves nothing, or passes a bound.
            return false;
        };
        // Typing to the press pane only: never broadcast, and MCP sees it as user input.
        self.write_to_pane(anchor.pane_id(), bytes, PtyInputSource::Keyboard)
    }
}

/// The bytes a click on absolute `cell` (on `viewport_row`) of `grid` sends under `mode` and the
/// pane's `keyboard` state, or `None` when it sends nothing: no prompt, the alternate screen, a
/// click above the prompt, an undeclared prompt under `Declared`, no motion, or a passed bound.
pub(super) fn prompt_click_bytes(
    grid: &Grid,
    cell: (u64, u16),
    viewport_row: u16,
    mode: ClickToMove,
    keyboard: KeyboardSnapshot,
) -> Option<Vec<u8>> {
    let semantic = grid.semantic();
    let at_prompt = matches!(semantic.mode, SemanticMode::Prompt(_) | SemanticMode::Input { .. });
    if mode == ClickToMove::Off || grid.is_alt() || !at_prompt {
        // When: `mode` is Off, `is_alt` (the alternate screen) shows, or not `at_prompt`.
        return None;
    }
    let anchor = semantic.anchor?;
    // A secondary prompt at or above the click anchors it (kitty); otherwise the prompt does.
    let prompt_row = semantic.last_secondary_row.filter(|row| *row <= cell.0).unwrap_or(anchor);
    if cell.0 < prompt_row {
        // When: the click is above `prompt_row`, it is not in this prompt.
        return None;
    }
    let click = semantic.options.click;
    if let Some(events) = click.events {
        // When: the prompt set `events`, they win over keys (Ghostty): one cell-based SGR press.
        let row = match events {
            ClickEvents::Absolute => viewport_row,
            ClickEvents::Relative => u16::try_from(cell.0 - prompt_row).ok()?,
        };
        let report = MouseReport {
            button: 0,
            modifiers: 0,
            release: false,
            cell: (row, cell.1),
            pixel: (0, 0),
        };
        return Some(encode_mouse_report(MouseFormat::Sgr, report));
    }
    if mode == ClickToMove::Declared && click.line_motion.is_none() {
        // When: `Declared` moves only at prompts that declare `cl` (or `click_events`, above).
        return None;
    }
    let mut budget = MAX_VISITED_CELLS;
    let click_cell = (cell.0, lead_col(grid, cell));
    let (count, rightward) = if semantic.input_seen {
        input_move(grid, click_cell, &mut budget)?
    } else {
        // When: no `input_seen`: this prompt marked no input, so count text cells as kitty does.
        text_move(grid, click_cell, &mut budget)?
    };
    move_keys(count, rightward, click.special_key, keyboard)
}

/// `cell`'s column, moved onto the lead of a wide character whose right half it names.
fn lead_col(grid: &Grid, cell: (u64, u16)) -> u16 {
    let right_half = grid
        .row_at_abs(cell.0)
        .and_then(|row| row.get(usize::from(cell.1)))
        .is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT));
    cell.1 - u16::from(right_half && cell.1 > 0)
}

/// The cursor's column; a pending wrap puts it just past the row's last cell.
fn cursor_col(grid: &Grid) -> usize {
    usize::from(grid.cursor.col) + usize::from(grid.pending_wrap())
}

/// Whether `cell` was typed at the prompt; a wide character's right half is never counted.
fn is_input(cell: &Cell) -> bool {
    cell.flags.contains(CellFlags::PROMPT_INPUT) && !cell.flags.contains(CellFlags::WIDE_CONT)
}

/// A space with nothing combined into it and not a wide character's right half.
fn is_blank(cell: &Cell) -> bool {
    cell.ch == ' ' && cell.extras().is_none() && !cell.flags.contains(CellFlags::WIDE_CONT)
}

/// Spend one cell visit from `budget`; `None` once [`MAX_VISITED_CELLS`] are spent.
fn visit(budget: &mut usize) -> Option<()> {
    *budget = budget.checked_sub(1)?;
    Some(())
}

/// Keys from the cursor to `click` counting typed input cells along the soft-wrap chain
/// (Ghostty): rightward, a click past the input goes to its end, plus one when the cursor sits on
/// an input cell; leftward, the count clamps at the input's start. `None` past the visit bound.
fn input_move(grid: &Grid, click: (u64, u16), budget: &mut usize) -> Option<(usize, bool)> {
    let cursor = (grid.cursor_absolute_row(), cursor_col(grid));
    let click = (click.0, usize::from(click.1));
    if click == cursor {
        // When: the click is on the cursor's cell, nothing moves.
        return Some((0, true));
    }
    let mut count = 0;
    if cursor < click {
        // When: the click is after the cursor, count rightward from the cell after it.
        let mut row = cursor.0;
        let mut start = cursor.1 + 1;
        loop {
            let line = grid.row_at_abs(row)?;
            for (col, cell) in line.iter().enumerate().skip(start) {
                visit(budget)?;
                if is_input(cell) {
                    // When: `is_input` says the shell's input holds this cell, it is one key.
                    count += 1;
                    if (row, col) == click {
                        // When: the click is on an input cell, the keys reach it exactly.
                        return Some((count, true));
                    }
                }
            }
            let wraps = grid.row_at_abs(row + 1).is_some_and(Row::soft_wrapped_from_previous);
            if row == click.0 || !wraps {
                // When: the click row or the chain's end is reached without the click: go to the
                // input's end, one further when the chain ends and the cursor is on an input cell.
                let on_input = grid
                    .row_at_abs(cursor.0)
                    .and_then(|line| line.get(cursor.1))
                    .is_some_and(is_input);
                return Some((count + usize::from(!wraps && on_input), true));
            }
            row += 1;
            start = 0;
        }
    }
    let mut row = cursor.0;
    let mut end = cursor.1;
    loop {
        let line = grid.row_at_abs(row)?;
        for col in (0..end.min(line.len())).rev() {
            visit(budget)?;
            if line.get(col).is_some_and(is_input) {
                // When: `is_input` says the shell's input holds this cell, it is one key.
                count += 1;
                if (row, col) == click {
                    // When: the click is on an input cell, the keys reach it exactly.
                    return Some((count, false));
                }
            }
        }
        if row <= click.0 || !line.soft_wrapped_from_previous() {
            // When: the click row or the chain's start is reached: clamp at the input's start.
            return Some((count, false));
        }
        row -= 1;
        end = usize::MAX;
    }
}

/// The columns of `line`'s text: from its first non-blank cell to past its last, reaching at
/// least `cursor_end`, the cursor's column on its own row, since blanks before it were typed.
fn text_run(line: &Row, cursor_end: usize, budget: &mut usize) -> Option<std::ops::Range<usize>> {
    let mut first = None;
    let mut end = 0;
    for (col, cell) in line.iter().enumerate() {
        visit(budget)?;
        if !is_blank(cell) {
            first.get_or_insert(col);
            end = col + 1;
        }
    }
    let end = end.max(cursor_end);
    Some(first.unwrap_or(end)..end)
}

/// Keys from the cursor to `click` counting text cells (kitty): per row, leading blanks are
/// skipped, each text cell counts one, the first blank after the row's text counts one and ends
/// the row, and a row with no text in the span counts one. `None` past the visit bound.
///
/// The grid stores an erased cell and a typed space alike, so a blank between a row's text, or
/// before the cursor on its row, counts as typed text.
fn text_move(grid: &Grid, click: (u64, u16), budget: &mut usize) -> Option<(usize, bool)> {
    let cursor = (grid.cursor_absolute_row(), cursor_col(grid));
    let click = (click.0, usize::from(click.1));
    if click == cursor {
        // When: the click is on the cursor's cell, nothing moves.
        return Some((0, true));
    }
    let (start, end) = (cursor.min(click), cursor.max(click));
    let mut count = 0;
    for row in start.0..=end.0 {
        let line = grid.row_at_abs(row)?;
        // Only the cursor's own row stretches its text to the cursor column.
        let run = text_run(line, cursor.1 * usize::from(row == cursor.0), budget)?;
        let mut row_keys = 0;
        let mut ended_in_blank = false;
        let span = line
            .iter()
            .enumerate()
            .skip_while(|(col, _)| (row, *col) < start)
            .take_while(|(col, _)| (row, *col) < end);
        for (col, cell) in span {
            visit(budget)?;
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: `WIDE_CONT` marks a wide character's right half; its lead counted one key.
                continue;
            }
            if run.contains(&col) {
                // A cell inside the row's text counts one key.
                row_keys += 1;
            } else if col >= run.end {
                // When: `col` is past `run`'s end, the first blank counts one and ends the row.
                ended_in_blank = true;
                break;
            }
        }
        // A row with no text in the span counts one key; a blank after text counts one more.
        count += row_keys.max(1) + usize::from(row_keys > 0 && ended_in_blank);
    }
    Some((count, cursor < click))
}

/// `count` cursor keys rightward or leftward in the pane's `keyboard` state, or kitty's special
/// key (`CSI 1u` right, `CSI 1;1u` left) when the prompt asked for it; `None` for no motion or past
/// [`MAX_MOVE_KEYS`] or [`MAX_MOVE_BYTES`], so a click is sent whole or not at all.
fn move_keys(
    count: usize,
    rightward: bool,
    special_key: bool,
    keyboard: KeyboardSnapshot,
) -> Option<Vec<u8>> {
    if count == 0 || count > MAX_MOVE_KEYS {
        // When: `count` is zero (nothing moves) or above `MAX_MOVE_KEYS` for one click.
        return None;
    }
    let key = match (special_key, rightward) {
        (true, true) => b"\x1b[1u".to_vec(),
        (true, false) => b"\x1b[1;1u".to_vec(),
        (false, _) => {
            // The pane's own key encoder: DECCKM, kitty flags (with releases) and Win32 input.
            let spec = super::mcp::keys::parse_key(if rightward { "right" } else { "left" })?;
            super::mcp::keys::encode_keys(&[spec], keyboard, cfg!(windows)).ok()?
        }
    };
    if key.len().checked_mul(count)? > MAX_MOVE_BYTES {
        // When: `count` copies of `key` would pass `MAX_MOVE_BYTES`, one write's bound.
        return None;
    }
    Some(key.repeat(count))
}

#[cfg(test)]
#[path = "prompt_click_tests.rs"]
mod prompt_click_tests;
