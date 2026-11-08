//! Fixtures the MCP server's tests share: a resolved pane over a fresh parser, and a reply-line
//! reader for the endpoint tests.

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use sonicterm_vt::vt::Parser;

use super::{agents, PaneEntry};

/// A resolved pane over a fresh parser with `scrollback` lines of history, as the loop hands one
/// to a connection.
pub(crate) fn test_entry(cols: u16, rows: u16, scrollback: usize) -> PaneEntry {
    let mut grid = sonicterm_grid::grid::Grid::new(cols, rows);
    grid.set_scrollback_limit(scrollback);
    PaneEntry {
        pane_id: 7,
        metadata: None,
        creator: agents::PaneOwner::User,
        parser: Arc::new(Mutex::new(Parser::new_with_staging_pool(
            grid,
            None,
            sonicterm_vt::vt::CaptureStagingPool::new(),
        ))),
        output_generation: Arc::new(AtomicU64::new(0)),
        exit_status: Arc::new(OnceLock::new()),
        change_signal: Arc::default(),
        run_typed_at: Arc::new(AtomicU64::new(0)),
        agent_run: Arc::new(AtomicBool::new(false)),
    }
}

/// Lines of a `BufRead`, used by the Unix endpoint tests to read replies.
#[cfg(unix)]
pub(crate) fn read_reply_line(reader: &mut impl std::io::BufRead) -> Option<serde_json::Value> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end()).ok()
}
