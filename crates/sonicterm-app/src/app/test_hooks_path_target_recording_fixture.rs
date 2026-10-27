//! Recorded multiplexer target reconstruction and visible span contracts.

use super::*;

/// One recording in `test-fixtures/url-wrap`, mirroring a case of
/// `scripts/capture-multiplexer-url-wrap.py`. Every recording prints [`CLAUDE_OSC8_URL`] as text.
pub(super) struct RecordedCase {
    /// File stem: `<multiplexer>-<case>`.
    name: &'static str,
    /// The bytes the multiplexer wrote to its outer terminal.
    bytes: &'static [u8],
    /// How the program laid the URL out.
    layout: RecordedLayout,
    /// Drawn in the right pane of an 81-column split (border in column 40) instead of full width.
    split: bool,
    /// Blank cells the program left before the pane's right edge on every non-last row.
    margin: usize,
}

/// How a recorded program laid the URL out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordedLayout {
    /// A shell printed the bare URL and the pane wrapped it.
    Plain,
    /// Claude Code: `⏺ ` then the URL broken by the program, continuation rows indented two.
    Claude,
    /// Control: `See (` + URL + `) for details` broken at the pane edge.
    Paren,
}

/// One table row: the recording embedded with `include_bytes!`, so replay needs no multiplexer.
macro_rules! recorded_case {
    ($name:literal, $layout:ident, $split:literal, $margin:literal) => {
        RecordedCase {
            name: $name,
            bytes: include_bytes!(concat!("../../test-fixtures/url-wrap/", $name, ".bin")),
            layout: RecordedLayout::$layout,
            split: $split,
            margin: $margin,
        }
    };
}

/// Every committed recording, tmux and rmux.
pub(super) const RECORDED_CASES: &[RecordedCase] = &[
    recorded_case!("tmux-plain-full", Plain, false, 0),
    recorded_case!("tmux-plain-split", Plain, true, 0),
    recorded_case!("tmux-claude-edge-full", Claude, false, 0),
    recorded_case!("tmux-claude-edge-split", Claude, true, 0),
    recorded_case!("tmux-claude-margin1-full", Claude, false, 1),
    recorded_case!("tmux-claude-margin2-full", Claude, false, 2),
    recorded_case!("tmux-claude-margin3-full", Claude, false, 3),
    recorded_case!("tmux-paren-full", Paren, false, 0),
    recorded_case!("rmux-plain-full", Plain, false, 0),
    recorded_case!("rmux-plain-split", Plain, true, 0),
    recorded_case!("rmux-claude-edge-full", Claude, false, 0),
    recorded_case!("rmux-claude-edge-split", Claude, true, 0),
    recorded_case!("rmux-claude-margin1-full", Claude, false, 1),
    recorded_case!("rmux-claude-margin2-full", Claude, false, 2),
    recorded_case!("rmux-claude-margin3-full", Claude, false, 3),
    recorded_case!("rmux-paren-full", Paren, false, 0),
];

impl RecordedCase {
    /// Columns of the outer terminal the multiplexer drew into.
    pub(super) fn screen_columns(&self) -> u16 {
        if self.split {
            81
        } else {
            60
        }
    }

    /// Columns of the pane the program printed into.
    pub(super) fn pane_columns(&self) -> usize {
        if self.split {
            40
        } else {
            60
        }
    }

    /// The URL's text on each screen row, in order.
    pub(super) fn url_pieces(&self) -> Vec<String> {
        let width = self.pane_columns();
        let url: Vec<char> = CLAUDE_OSC8_URL.chars().collect();
        let collect = |pieces: std::slice::Chunks<'_, char>| {
            pieces.map(|piece| piece.iter().collect::<String>()).collect::<Vec<_>>()
        };
        match self.layout {
            RecordedLayout::Plain => collect(url.chunks(width)),
            RecordedLayout::Claude => collect(url.chunks(width - self.margin - 2)),
            RecordedLayout::Paren => {
                // The URL starts after `See (`; each printed line holds `width` characters.
                let prefix_chars = "See (".chars().count();
                let url_end = prefix_chars + url.len();
                (0..)
                    .map(|line| (line * width, (line + 1) * width))
                    .take_while(|(line_start, _)| *line_start < url_end)
                    .filter_map(|(line_start, line_end)| {
                        let start = line_start.max(prefix_chars);
                        let end = line_end.min(url_end);
                        (start < end)
                            .then(|| url[start - prefix_chars..end - prefix_chars].iter().collect())
                    })
                    .collect()
            }
        }
    }
}

/// Feed `recording` into a fresh child pane `columns` wide, the size it was recorded at.
pub(super) fn replay_recording(columns: u16, recording: &[u8]) -> (App, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["multiplexer"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(columns, 12);
    assert!(app.__test_advance_child_pane_parser(window, pane, recording));
    (app, window, pane)
}

/// Where each URL piece landed on screen, searching rows top to bottom in order; wide-cell
/// continuations are skipped so columns are real cell columns.
pub(super) fn locate_url_pieces(
    app: &App,
    window: WindowId,
    pane: u64,
    pieces: &[String],
) -> Vec<sonicterm_render_model::inputs::HoveredUrlSpan> {
    let parser = app.windows[&window].panes[&pane].parser.lock();
    let grid = parser.grid();
    let mut found = Vec::new();
    let mut next_row = 0;
    for piece in pieces {
        let wanted: Vec<char> = piece.chars().collect();
        let located = (next_row..grid.rows).find_map(|row| {
            let cells: Vec<(u16, char)> = grid
                .row(row)
                .iter()
                .enumerate()
                .filter(|(_, cell)| !cell.flags.contains(CellFlags::WIDE_CONT))
                .map(|(column, cell)| (column as u16, cell.ch))
                .collect();
            cells.windows(wanted.len()).find_map(|window_cells| {
                window_cells
                    .iter()
                    .map(|(_, character)| *character)
                    .eq(wanted.iter().copied())
                    .then(|| (row, window_cells[0].0, window_cells[wanted.len() - 1].0 + 1))
            })
        });
        let (row, start_col, end_col) =
            located.unwrap_or_else(|| panic!("recording does not show {piece:?}"));
        found.push(hover_span(row, start_col, end_col));
        next_row = row + 1;
    }
    found
}

/// One line describing what detection resolved at a cell, for failure messages.
pub(super) fn describe_target(target: Option<&CellTargetSnapshot>) -> String {
    let Some(target) = target else {
        return "nothing".to_string();
    };
    let kind = match &target.target {
        ResolvedCellTarget::Uri(uri) => format!("Uri({uri})"),
        ResolvedCellTarget::Rejected(reason) => format!("Rejected({reason})"),
        ResolvedCellTarget::Path(key) => format!("Path({})", key.candidates.len()),
    };
    let spans = target.hover_cells.map(|cells| cells.spans().to_vec());
    format!("{kind} display={:?} spans={spans:?}", target.display)
}

/// The rows real Claude Code draws for `url` in a pane `columns` wide on the alternate screen:
/// `⏺ ` and the URL from column 2, every non-last row ending exactly at the edge, continuation
/// rows indented two, as measured with Claude Code 2.1.306 in tmux.
pub(super) fn claude_reply_rows(
    columns: usize,
    url: &str,
) -> (String, Vec<sonicterm_render_model::inputs::HoveredUrlSpan>) {
    let chars: Vec<char> = url.chars().collect();
    let mut output = String::from("\x1b[?1049h");
    let mut spans = Vec::new();
    for (index, piece) in chars.chunks(columns - 2).enumerate() {
        let prefix = if index == 0 { "⏺ " } else { "  " };
        output.push_str(&format!(
            "\x1b[{};1H{prefix}{}",
            index + 3,
            piece.iter().collect::<String>()
        ));
        spans.push(hover_span(index as u16 + 2, 2, 2 + piece.len() as u16));
    }
    (output, spans)
}

/// Resolve every fragment of a Claude Code reply and require the complete URL and its spans.
pub(super) fn assert_claude_reply_joins(columns: usize, url: &str) {
    let (output, spans) = claude_reply_rows(columns, url);
    assert!(spans.len() >= 2, "{url:?} fits on one row at {columns} columns");
    for span in &spans {
        let target = target_after(columns as u16, 12, &output, span.row, span.start_col)
            .unwrap_or_else(|| panic!("{columns} columns, row {}: nothing", span.row));
        assert_eq!(
            target.target,
            ResolvedCellTarget::Uri(url.to_string()),
            "{columns} columns, row {}",
            span.row
        );
        assert_eq!(target.hover_cells.unwrap().spans(), spans.as_slice(), "{columns} columns");
    }
}

/// The URL Claude Code was asked to print when its tmux output was recorded.
pub(super) const CLAUDE_OSC8_URL: &str = "https://github.com/D0n9X1n/SonicTerm/issues/1669#hard-wrapped-bare-urls-in-tmux-and-rmux-are-joined-across-rows-when-a-program-breaks-them?from=claude-code&case=multiplexer-panes";

/// One wrapped OSC 8 link as tmux redraws Claude Code's reply when the client has the
/// `hyperlinks` feature: each row is positioned with a cursor move, re-opens the link with the
/// same `tmuxN` id and closes it at the row's end. `first_col` is the pane's first column
/// (1-based for the cursor move); every row is `width` cells, the first after `⏺ `.
pub(super) fn claude_osc8_rows(
    start_row: u16,
    first_col: u16,
    width: usize,
) -> (String, Vec<sonicterm_render_model::inputs::HoveredUrlSpan>) {
    let open = format!("\x1b]8;id=tmux5;{CLAUDE_OSC8_URL}\x1b\\");
    let close = "\x1b]8;;\x1b\\";
    let url: Vec<char> = CLAUDE_OSC8_URL.chars().collect();
    let mut output = String::new();
    let mut spans = Vec::new();
    for (index, piece) in url.chunks(width - 2).enumerate() {
        let row = start_row + index as u16;
        let prefix = if index == 0 { "⏺ " } else { "  " };
        let text: String = piece.iter().collect();
        output.push_str(&format!(
            "\x1b[{};{first_col}H{prefix}\x1b[94m{open}{text}\x1b[39m{close}",
            row + 1
        ));
        let start_col = first_col - 1 + 2;
        spans.push(hover_span(row, start_col, start_col + piece.len() as u16));
    }
    (output, spans)
}

/// Whether `target` resolved to a path whose key carries a joined (hard-wrap) candidate.
pub(super) fn has_joined_path(target: Option<&CellTargetSnapshot>) -> bool {
    target.is_some_and(|target| {
        matches!(&target.target, ResolvedCellTarget::Path(key) if key.candidates.iter().any(|candidate| candidate.joined))
    })
}

/// Seed a child window, size its pane to `cols` x `rows`, write `output`, and return the label
/// captured for the cell at `(row, col)`.
pub(super) fn captured_label(
    output: &str,
    cols: u16,
    rows: u16,
    row: u16,
    col: u16,
) -> Option<LinkLabel> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["label"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.cell_target_at(window, pane, row, col).unwrap().label
}

#[cfg(test)]
#[path = "test_hooks_path_target_recording_fixture_tests.rs"]
mod test_hooks_path_target_recording_fixture_tests;
