//! The cursor glide: the drawn cursor moving from where it was presented to a new cell over
//! [`GLIDE_DURATION`], easing out, instead of jumping.
//!
//! The glide works in cell coordinates, as fractional rows and columns of the cursor's pane, so it
//! needs no pixel geometry and lands exactly on the cell's own snapped edges when it ends. State is
//! split in two. The *pending* glide (start, target, start instant) changes at once whenever the
//! target moves or a glide is cancelled, whether or not a frame presents, so a retry never restarts
//! the clock. The *committed* position is the one on screen: only a presented frame updates it,
//! and a new glide starts from it. One position is sampled per frame and quantized to sixteenths of
//! a cell; that one value is keyed, drawn and damaged.

use std::time::{Duration, Instant};

/// How long one glide takes.
pub const GLIDE_DURATION: Duration = Duration::from_millis(90);

/// Steps per cell a sampled position is quantized to, so two samples that draw the same pixels at
/// ordinary cell sizes key the same.
const STEPS_PER_CELL: f32 = 16.0;

/// A cursor position in its pane's cells: row, column and width in columns, each in sixteenths of
/// a cell so it can be compared and keyed exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlideCell {
    /// Row, in sixteenths of a cell.
    pub row: i32,
    /// Column, in sixteenths of a cell.
    pub col: i32,
    /// Width in columns, in sixteenths of a cell.
    pub span: i32,
}

impl GlideCell {
    /// The cell at `row`, `col`, `span` columns wide.
    pub fn at(row: u16, col: u16, span: u16) -> Self {
        let step = STEPS_PER_CELL as i32;
        Self {
            row: i32::from(row) * step,
            col: i32::from(col) * step,
            span: i32::from(span) * step,
        }
    }

    /// The row in cells.
    pub fn row_cells(self) -> f32 {
        self.row as f32 / STEPS_PER_CELL
    }

    /// The column in cells.
    pub fn col_cells(self) -> f32 {
        self.col as f32 / STEPS_PER_CELL
    }

    /// The width in cells.
    pub fn span_cells(self) -> f32 {
        self.span as f32 / STEPS_PER_CELL
    }

    /// The position `progress` of the way from `self` to `to`, quantized.
    fn toward(self, to: Self, progress: f32) -> Self {
        let step =
            |start: i32, end: i32| (start as f32 + (end - start) as f32 * progress).round() as i32;
        Self {
            row: step(self.row, to.row),
            col: step(self.col, to.col),
            span: step(self.span, to.span),
        }
    }

    /// Whether moving from `self` to `to` is too long a jump to glide: more than half the diagonal
    /// of a `cols × rows` pane, as a `clear` or a full-screen redraw makes.
    pub fn long_jump(self, to: Self, cols: u16, rows: u16) -> bool {
        let delta_col = (to.col - self.col) as f32 / STEPS_PER_CELL;
        let delta_row = (to.row - self.row) as f32 / STEPS_PER_CELL;
        let diagonal = (f32::from(cols).powi(2) + f32::from(rows).powi(2)).sqrt();
        (delta_col * delta_col + delta_row * delta_row).sqrt() > 0.5 * diagonal
    }
}

/// A glide in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    /// Where the glide starts: the position on screen when it began.
    from: GlideCell,
    /// Where it ends.
    to: GlideCell,
    /// When it began.
    started: Instant,
}

/// One window's cursor glide.
#[derive(Debug, Clone, Default)]
pub struct CursorGlide {
    /// The glide in progress, if any.
    pending: Option<Pending>,
    /// The target the last sample was for, glided to or not. A glide starts only when the target
    /// changes, so a finished glide whose landing frame failed to present draws the target on the
    /// retry instead of gliding again from an older committed position.
    target: Option<GlideCell>,
    /// The position the last presented frame drew.
    committed: Option<GlideCell>,
    /// The position this frame sampled, committed if the frame presents.
    sampled: Option<GlideCell>,
}

impl CursorGlide {
    /// The position to draw this frame for a cursor at `target`, at `now`. With `allowed` false,
    /// or with `snap` (another pane or screen, new cell metrics, a long jump), the cursor draws at
    /// `target` and any glide is cancelled. Returns the sampled position while a glide is in
    /// progress, `None` when the cursor draws exactly at `target`.
    pub fn sample(
        &mut self,
        target: GlideCell,
        now: Instant,
        allowed: bool,
        snap: bool,
    ) -> Option<GlideCell> {
        let new_target = self.target != Some(target);
        self.target = Some(target);
        if !allowed || snap {
            // When: gliding is not `allowed` or the move must `snap`, cancel and draw the target.
            self.pending = None;
            self.sampled = Some(target);
            return None;
        }
        if new_target {
            // A new target starts a glide from what is on screen, or none if nothing is.
            self.pending = match self.committed {
                Some(from) if from != target => Some(Pending { from, to: target, started: now }),
                _ => None,
            };
        }
        let Some(pending) = self.pending else {
            // When: no glide is pending, the cursor is already at its target.
            self.sampled = Some(target);
            return None;
        };
        let elapsed = now.saturating_duration_since(pending.started);
        if elapsed >= GLIDE_DURATION {
            // When: `elapsed` reached the duration, the glide is done and the cursor rests at its target.
            self.pending = None;
            self.sampled = Some(target);
            return None;
        }
        let progress = ease_out_cubic(elapsed.as_secs_f32() / GLIDE_DURATION.as_secs_f32());
        let position = pending.from.toward(pending.to, progress);
        self.sampled = Some(position);
        Some(position)
    }

    /// The position the last presented frame drew, for the snap rules.
    pub fn committed(&self) -> Option<GlideCell> {
        self.committed
    }

    /// Whether a glide is in progress, so the window schedules the next frame.
    pub fn gliding(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget the cursor: it is not drawn this frame, so it next appears without a glide.
    pub fn hide(&mut self) {
        self.pending = None;
        self.sampled = None;
        self.target = None;
    }

    /// Settle the frame: a presented frame commits what it sampled; any other keeps the last
    /// committed position, so a retry glides from what is really on screen.
    pub fn settle(&mut self, presented: bool) {
        if presented {
            // A presented frame's sampled position is what is on screen now.
            self.committed = self.sampled;
        }
    }
}

/// Ease-out cubic: fast at first, settling into the target. `progress` is clamped to `0..=1`.
pub fn ease_out_cubic(progress: f32) -> f32 {
    let remaining = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - remaining * remaining * remaining
}

#[cfg(test)]
#[path = "cursor_glide_tests.rs"]
mod cursor_glide_tests;
