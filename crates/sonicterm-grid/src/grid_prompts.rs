//! OSC 133 prompt regions, stable row tokens, and the pinned command run an agent waits on.
//!
//! A region opens at A and records B (where input begins), C (where output begins) and D (the
//! command's end and exit code). Regions live in a bounded deque that moves with the
//! alternate-screen swap; their `seq` counter and the one pinned run live on the outer grid, so a
//! waiter keeps its identity across eviction and screen changes.

use std::time::Instant;

use super::*;

/// A stable name for a row: survives scrolling, a full history and history clears.
///
/// `line` is the row's position in the screen's lifetime count of rows,
/// `lines_scrolled() - scrollback_len() + absolute_row`. Each screen keeps its own count and
/// an alternate screen starts past every earlier one, so tokens never collide across screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowToken {
    /// The row's lifetime line number on its screen.
    pub line: u64,
    /// Whether the row belongs to the alternate screen.
    pub alt: bool,
}

/// The run an agent is waiting on, recorded independently of the evictable region deque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedRun {
    /// The `seq` of the region the command was typed at.
    pub seq: u64,
    /// The screen the run was pinned on; marks on the other screen never touch it.
    pub alt: bool,
    /// Whether C arrived for the region.
    pub output_seen: bool,
    /// Where the command's output begins.
    pub output_start: Option<RowToken>,
    /// When C arrived; typing admitted before it may have reached the command line.
    pub output_at: Option<Instant>,
    /// Where the command ended.
    pub end: Option<RowToken>,
    /// The column on `end`'s row where D arrived.
    pub end_col: u16,
    /// `Some(exit_code)` once D arrived for the region.
    pub exit: Option<Option<i32>>,
    /// A newer prompt opened before the run finished, so its completion can no longer arrive.
    pub evicted: bool,
    /// A prompt opened on the run's screen after its D. The command's own output can print D, so
    /// only a later prompt says the shell took control back; a forged D alone never ends a run.
    pub prompt_after_end: bool,
    /// The run's C came from an authenticated shell marker (one carrying the pane's token).
    pub output_authenticated: bool,
    /// The run's latest D came from an authenticated shell marker.
    pub end_authenticated: bool,
}

/// A completed command's output span, for reading what the last command printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpan {
    /// The region's identity.
    pub seq: u64,
    /// Absolute row where output began; `None` once it left history.
    pub output_start_row: Option<u64>,
    /// Absolute row where the command ended; `None` once it left history.
    pub end_row: Option<u64>,
    /// The column on `end_row` where D arrived; output on that row ends before it.
    pub end_col: u16,
    /// The exit code D reported.
    pub exit_code: Option<i32>,
    /// The output's first row already left history, so the span is incomplete.
    pub output_truncated: bool,
}

impl Grid {
    /// Record an OSC 133 `A` (prompt-start) marker at the cursor row.
    /// Coalesces consecutive markers on the same row so a shell that emits
    /// the marker more than once per prompt doesn't bloat the buffer.
    pub fn record_prompt_start(&mut self) {
        let row = self.cursor_absolute_row();
        if matches!(self.prompts.back(), Some(prompt) if prompt.start_row == Some(row) && !prompt.output_seen && !prompt.completed)
        {
            // When: `matches!` finds a prompt at `row` whose command has not started, coalesce
            // the marker; once C was seen, an A on the same row is a new prompt.
            return;
        }
        let is_alt = self.is_alt();
        // On the pinned screen, a newer prompt after D closes the run; before D, it means the run's
        // completion can no longer arrive. Both are decided before the deque changes.
        if let Some(run) = self.pinned_run.as_mut().filter(|run| run.alt == is_alt) {
            if run.exit.is_some() {
                run.prompt_after_end = true;
            } else {
                // When: the pinned run has no `exit` yet, a newer prompt means its D cannot arrive.
                run.evicted = true;
            }
        }
        if self.prompts.len() >= PROMPT_REGION_LIMIT {
            self.prompts.pop_front();
        }
        let seq = self.prompt_seq_next;
        self.prompt_seq_next = self.prompt_seq_next.saturating_add(1);
        self.prompts.push_back(PromptRegion {
            seq,
            start_row: Some(row),
            input_start: None,
            input_start_at: None,
            output_start_row: None,
            output_start_at: None,
            output_seen: false,
            end_row: None,
            end_col: 0,
            completed: false,
            exit_code: None,
        });
    }

    /// Record an OSC 133 `B` (input-start) marker at the cursor in the newest region; a duplicate
    /// keeps the first position.
    pub fn record_input_start(&mut self) {
        let at = (self.cursor_absolute_row(), self.cursor.col.min(self.cols.saturating_sub(1)));
        if let Some(last) = self.prompts.back_mut().filter(|last| last.input_start.is_none()) {
            last.input_start = Some(at);
            last.input_start_at = Some(Instant::now());
        }
    }

    /// Record an OSC 133 `C` (output-start) marker in the newest region; a duplicate keeps the
    /// first row. Updates the pinned run when that region is the one it waits on.
    pub fn record_output_start(&mut self) {
        self.record_output_start_marked(false);
    }

    /// [`Self::record_output_start`], noting whether the marker was `authenticated` by the pane's
    /// shell token; the pinned run keeps that for its own C.
    pub fn record_output_start_marked(&mut self, authenticated: bool) {
        let row = self.cursor_absolute_row();
        let token = self.row_token(row);
        let Some(last) = self.prompts.back_mut().filter(|last| !last.output_seen) else {
            // When: `back_mut` finds no region, or its `output_seen` is set, the first C stands.
            return;
        };
        let now = Instant::now();
        last.output_seen = true;
        last.output_start_row = Some(row);
        last.output_start_at = Some(now);
        let seq = last.seq;
        let is_alt = self.is_alt();
        if let Some(run) =
            self.pinned_run.as_mut().filter(|run| run.seq == seq && run.alt == is_alt)
        {
            run.output_seen = true;
            run.output_start = Some(token);
            run.output_at = Some(now);
            run.output_authenticated = authenticated;
        }
    }

    /// Record an OSC 133 `D` (command-end) marker. Updates the most recent
    /// prompt region in place; a stray `D` without a prior `A` is ignored.
    pub fn record_prompt_end(&mut self, exit_code: Option<i32>) {
        self.record_prompt_end_marked(exit_code, false);
    }

    /// [`Self::record_prompt_end`], noting whether the marker was `authenticated`. A later D
    /// replaces an earlier one's exit and trust, so the shell's real D outranks a forged one.
    pub fn record_prompt_end_marked(&mut self, exit_code: Option<i32>, authenticated: bool) {
        let row = self.cursor_absolute_row();
        let col = self.cursor.col;
        let token = self.row_token(row);
        let Some(last) = self.prompts.back_mut() else {
            // When: `back_mut` finds no region, the stray D names nothing.
            return;
        };
        last.end_row = Some(row);
        last.end_col = col;
        last.completed = true;
        last.exit_code = exit_code;
        let seq = last.seq;
        let is_alt = self.is_alt();
        if let Some(run) =
            self.pinned_run.as_mut().filter(|run| run.seq == seq && run.alt == is_alt)
        {
            run.end = Some(token);
            run.end_col = col;
            run.exit = Some(exit_code);
            run.end_authenticated = authenticated;
        }
    }

    /// All recorded prompt regions in chronological order.
    pub fn prompts(&self) -> impl Iterator<Item = &PromptRegion> {
        self.prompts.iter()
    }

    /// Number of recorded prompt regions.
    pub fn prompts_len(&self) -> usize {
        self.prompts.len()
    }

    /// The primary screen's regions, also while the alternate screen is active, when they sit
    /// in the saved primary state.
    fn primary_prompts(&self) -> &VecDeque<PromptRegion> {
        self.alt_screen.as_ref().map_or(&self.prompts, |saved| &saved.prompts)
    }

    /// The primary screen: the live grid, or the saved one while the alternate screen is active.
    /// Command spans index its rows.
    pub fn primary_screen(&self) -> &Grid {
        self.alt_screen.as_deref().unwrap_or(self)
    }

    /// The newest primary-screen region that has both C and D, as a command output span.
    pub fn last_completed_command(&self) -> Option<CommandSpan> {
        let region = self
            .primary_prompts()
            .iter()
            .rev()
            .find(|region| region.output_seen && region.completed)?;
        Some(CommandSpan {
            seq: region.seq,
            output_start_row: region.output_start_row,
            end_row: region.end_row,
            end_col: region.end_col,
            exit_code: region.exit_code,
            output_truncated: region.output_start_row.is_none(),
        })
    }

    /// The newest region on the active screen, the one a command typed now would run in.
    pub fn newest_prompt(&self) -> Option<&PromptRegion> {
        self.prompts.back()
    }

    /// Pin the run waiting on region `seq`; refused when a run is already pinned or `seq` is not
    /// the newest region on the active screen.
    pub fn pin_run(&mut self, seq: u64) -> bool {
        if self.pinned_run.is_some() || self.prompts.back().map(|region| region.seq) != Some(seq) {
            // When: a run is already pinned or `seq` is not the newest region, nothing is pinned.
            return false;
        }
        self.pinned_run = Some(PinnedRun {
            seq,
            alt: self.is_alt(),
            output_seen: false,
            output_start: None,
            output_at: None,
            end: None,
            end_col: 0,
            exit: None,
            evicted: false,
            prompt_after_end: false,
            output_authenticated: false,
            end_authenticated: false,
        });
        true
    }

    /// The pinned run's progress, if a run is pinned.
    pub fn pinned_run(&self) -> Option<PinnedRun> {
        self.pinned_run
    }

    /// Release the pinned run, returning its final progress.
    pub fn unpin_run(&mut self) -> Option<PinnedRun> {
        self.pinned_run.take()
    }

    /// The stable token naming absolute row `abs` on the active screen.
    pub fn row_token(&self, abs: u64) -> RowToken {
        let base = self.lines_scrolled.saturating_sub(self.scrollback.len() as u64);
        RowToken { line: base.saturating_add(abs), alt: self.is_alt() }
    }

    /// The absolute row `token` names, or `None` when it left history or belongs to the other
    /// screen.
    pub fn resolve_row(&self, token: RowToken) -> Option<u64> {
        if token.alt != self.is_alt() {
            // When: `token.alt` differs from the active screen, the row is not on this screen.
            return None;
        }
        let base = self.lines_scrolled.saturating_sub(self.scrollback.len() as u64);
        let abs = token.line.checked_sub(base)?;
        (abs < self.scrollback.len() as u64 + u64::from(self.rows)).then_some(abs)
    }

    /// Visible-region row of a prompt region, if it currently lies inside
    /// the visible window. Used by the renderer to draw the gutter caret.
    pub fn prompt_visible_row(&self, prompt: &PromptRegion) -> Option<u16> {
        let scrollback = self.scrollback.len() as u64;
        let rel = prompt.start_row?.checked_sub(scrollback)?;
        if rel < self.rows as u64 {
            Some(rel as u16)
        } else {
            // When: `rel >= self.rows`, the prompt has no visible row.
            None
        }
    }

    /// Find the prompt whose absolute start row is the largest one strictly
    /// less than `from_absolute_row`. Used by the "scroll to previous
    /// prompt" action; regions whose start left history are skipped.
    pub fn prompt_before(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts
            .iter()
            .rev()
            .find(|prompt| prompt.start_row.is_some_and(|row| row < from_absolute_row))
    }

    /// Find the prompt whose absolute start row is the smallest one strictly
    /// greater than `from_absolute_row`. Used by the "scroll to next
    /// prompt" action; regions whose start left history are skipped.
    pub fn prompt_after(&self, from_absolute_row: u64) -> Option<&PromptRegion> {
        self.prompts
            .iter()
            .find(|prompt| prompt.start_row.is_some_and(|row| row > from_absolute_row))
    }

    /// Rebase every region after `removed` rows left the front of history: a coordinate on a
    /// removed row becomes `None`. A region whose start left is kept while its command is running
    /// (C seen, no D) or while it is the pinned run's, so a long command's completion is not
    /// lost; any other such region can no longer name a prompt and is dropped.
    pub(super) fn rebase_prompts_after_drain(&mut self, removed: u64) {
        let rebase = |row: Option<u64>| row.and_then(|row| row.checked_sub(removed));
        // The pinned run's region is kept until the run ends, so its C and D still reach the pin.
        let is_alt = self.is_alt();
        let pinned = self.pinned_run.filter(|run| run.alt == is_alt).map(|run| run.seq);
        self.prompts.retain_mut(|prompt| {
            let start = rebase(prompt.start_row);
            let running = prompt.output_seen && !prompt.completed;
            if start.is_none() && !running && Some(prompt.seq) != pinned {
                // When: the start left history and the region is neither `running` nor `pinned`,
                // nothing can still complete it.
                return false;
            }
            prompt.start_row = start;
            prompt.input_start =
                prompt.input_start.and_then(|(row, col)| Some((row.checked_sub(removed)?, col)));
            prompt.output_start_row = rebase(prompt.output_start_row);
            prompt.end_row = rebase(prompt.end_row);
            true
        });
    }
}

#[cfg(test)]
#[path = "grid_prompts_tests.rs"]
mod grid_prompts_tests;

#[cfg(test)]
#[path = "grid_prompts_trust_tests.rs"]
mod grid_prompts_trust_tests;
