//! App-owned status line state: session toggles, the memory sampler,
//! per-component reload errors, per-window pane facts, and the sync that
//! turns them into each window's strip.
//!
//! The strip never wakes the loop by itself: memory is sampled only while a
//! visible window configures `mem`, a pending path check wakes once when it
//! becomes worth showing, and a window is redrawn only when the pixels of its
//! strip change.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sonicterm_cfg::status_line::CpuScale;
use sonicterm_cfg::status_line::{
    PasteWarning, ResolvedStatusLine, StatusLineConfig, StatusSegmentId,
};
use sonicterm_cfg::status_overrides::{
    MoveDirection, StatusLineOverrides, StatusLineSave, WrittenStatusLine,
};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_logging::MemoryMetric;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::selection::{
    selection_extent_counted, Selection, SelectionExtent, SELECTION_EXTENT_MAX_ROWS,
};
use sonicterm_ui::status_line::{
    build_view, sanitize_link_text, AlertsInput, ClipInput, ClipOutcome, ClipSource, FontInput,
    GlyphsInput, HealthInput, ImeInput, InputState, LinkInput, MemHistory, MemInput, ModeInput,
    NotSentReason, NumberStyle, PathAction, ReloadFailure, RenderHealth, SegmentClick,
    SegmentHandler, SelInput, StatusLineInputs, StatusLineView,
};
use sonicterm_ui::status_line::{
    byte_rate, cpu_percent, FrameInput, Hysteresis, PathCheckInput, PresenterKind, TabMemInput,
};
use winit::window::WindowId;

use super::path_target::{PathKind, PathOpenDecision, ProbeStatus};
use super::redraw::RedrawCause;
use super::window_keyboard::{ime_route_for, ImeRoute};
use super::window_state::WindowState;
use super::CommandPhase;
use super::{resize_panes_to_rects, App};

/// A path check still running after this long is shown as `checking`;
/// faster checks never flicker through it.
const PENDING_SHOW_AFTER: Duration = Duration::from_millis(300);

/// Focused-pane facts copied in the frame pass, which already holds the
/// pane's parser guard, so the status line timer never reads a parser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StatusPaneFacts {
    /// A program enabled DEC mouse reporting.
    pub(crate) mouse_captured: bool,
    /// Lines the view sits above the live screen.
    pub(crate) scroll_lines: u64,
    /// The program enabled bracketed paste.
    pub(crate) bracketed_paste: bool,
    /// The pane shows its alternate screen.
    pub(crate) alt_screen: bool,
    /// The pane these facts describe.
    pub(crate) pane_id: u64,
    /// Rows the grid's byte budget has trimmed from history; see `Grid::budget_trimmed_rows`.
    pub(crate) budget_trimmed: u64,
    /// `budget_trimmed` at the last scrollback clear, which acknowledges every trim before it.
    pub(crate) trim_cleared: u64,
    /// Resizes that changed the grid; see `Grid::size_generation`.
    pub(crate) size_generation: u64,
    /// Grid columns.
    pub(crate) cols: u16,
    /// Grid rows.
    pub(crate) rows: u16,
}

impl StatusPaneFacts {
    /// Facts of pane `pane_id`, whose parser is `parser`, viewed from `viewport_top_abs`.
    pub(crate) fn of(
        parser: &sonicterm_vt::vt::Parser,
        viewport_top_abs: Option<u64>,
        pane_id: u64,
    ) -> Self {
        let grid = parser.grid();
        let live_top = GpuRenderer::resolved_view_top_abs_legacy(grid, None);
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        Self {
            mouse_captured: parser.mouse_tracking() != sonicterm_vt::vt::MouseTracking::Off,
            scroll_lines: live_top.saturating_sub(view_top),
            bracketed_paste: parser.bracketed_paste_enabled(),
            alt_screen: grid.is_alt(),
            pane_id,
            budget_trimmed: grid.budget_trimmed_rows(),
            trim_cleared: grid.budget_trimmed_at_clear(),
            size_generation: grid.size_generation(),
            cols: grid.cols,
            rows: grid.rows,
        }
    }
}

/// The strip inputs a window last built from, so an unchanged loop pass
/// formats nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltFrom {
    generation: u64,
    inputs: StatusLineInputs,
}

/// Segments that first appeared in a window during one strip pass, for the status line counters.
#[derive(Debug, Default)]
struct StripShows {
    /// Windows where `History: trimmed` appeared.
    history: u64,
    /// Windows where `Input: held` appeared.
    held: u64,
    /// Windows where `Glyphs:` appeared.
    glyphs: u64,
}

/// The status line state the App owns, one per process.
#[derive(Debug, Default)]
pub(crate) struct StatusLineState {
    /// Whether the strip is shown this session: the file's value or its override.
    visible: bool,
    /// Whether sensitive segments are hidden this session: the file's value or its override.
    hide_sensitive: bool,
    /// The `[status_line]` table last applied, so an unchanged table reconciles nothing.
    applied: Option<StatusLineConfig>,
    /// The last good resolved file: the baseline overrides apply to and reconcile against.
    file: Option<ResolvedStatusLine>,
    /// The session's choices on top of `file`; one entry per segment at most.
    overrides: StatusLineOverrides,
    /// The resolved table the strip draws: `file` with `overrides` applied.
    resolved: ResolvedStatusLine,
    /// Bumped when anything other than the inputs changes the strip
    /// (toggles, config, locale), so every window rebuilds once.
    generation: u64,
    /// When memory is next sampled; `None` while nothing samples.
    next_sample: Option<Instant>,
    /// The latest memory sample and the one before it.
    mem: MemInput,
    /// Last reload failure of the config file.
    config_error: Option<ReloadFailure>,
    /// Last reload failure of the theme.
    theme_error: Option<ReloadFailure>,
    /// Last reload failure of the keymap.
    keymap_error: Option<ReloadFailure>,
    /// Focused-pane facts per window, from the last frame pass.
    pane_facts: HashMap<WindowId, StatusPaneFacts>,
    /// The pending path under each window's pointer and when it was first seen.
    pending_since: HashMap<WindowId, (std::path::PathBuf, Instant)>,
    /// What each window's strip was last built from.
    built: HashMap<WindowId, BuiltFrom>,
    /// Whether startup load failures were already recorded.
    startup_checked: bool,
    /// The wake the last sync armed, so reaching it counts as a status line wake.
    armed_wake: Option<Instant>,
    /// The sparkline's wall-clock buckets.
    history: MemHistory,
    /// When the last sample was taken, on the monotonic and the wall clock.
    last_sample: Option<(Instant, u64)>,
    /// Per pane, the budget-trimmed row count a `history` click acknowledged.
    history_ack: HashMap<u64, u64>,
    /// Per pane, the grid size generation first seen in a shown frame, then the latest.
    size_seen: HashMap<u64, u64>,
    /// Per window, the pane whose resize `size` shows and when that hold ends.
    size_shown: HashMap<WindowId, (u64, Instant)>,
    /// The latest OSC 52 batch, shown for `hold_secs`.
    clip: Option<ClipRecord>,
    /// Per pane, the native result of its latest accepted write, until its batch summary arrives.
    clip_write: HashMap<u64, ClipOutcome>,
    /// Per pane, when a refusal was last logged and how many batches were suppressed since.
    clip_log: HashMap<u64, (Instant, u64)>,
    /// Per window, the first missing codepoints its `glyphs` count shows, for the click.
    glyph_sample: HashMap<WindowId, Vec<char>>,
    /// The previous `cpu` sample: when, and the process CPU time then.
    cpu_last: Option<(Instant, Duration)>,
    /// `cpu`'s show-and-hide state.
    cpu_hysteresis: Hysteresis,
    /// The `cpu` figure shown, while its hysteresis shows it.
    cpu_shown: Option<u64>,
    /// Per pane, its output byte total at the previous `out` sample.
    out_prev: HashMap<u64, u64>,
    /// When `out` last sampled.
    out_last: Option<Instant>,
    /// Per window, `out`'s show-and-hide state.
    out_hysteresis: HashMap<WindowId, Hysteresis>,
    /// Per window, the `out` rate shown, while its hysteresis shows it.
    out_shown: HashMap<WindowId, Option<u64>>,
    /// `path_check`'s hold rule.
    path_check_hold: super::jobs::PathCheckHold,
    /// The `path_check` display from the last sync.
    path_check_shown: Option<PathCheckInput>,
    /// When `frame` last sampled.
    frame_last: Option<Instant>,
    /// Per window, the `frame` figure of the last sample interval.
    frame_shown: HashMap<WindowId, FrameInput>,
    /// Per window, the latest refused, failed or blocked input, shown for `hold_secs`.
    input: HashMap<WindowId, InputRecord>,
    /// Per pane, the armed outstanding-work deadline and the write it shows as held.
    held: HashMap<u64, HeldWatch>,
    /// Input noted from a shared-borrow path (key encoding), recorded at the next sync.
    deferred_input: std::cell::Cell<Option<(WindowId, InputState)>>,
}

/// One window's latest input refusal, failure or block; it retains no input bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputRecord {
    /// What happened.
    pub(crate) state: InputState,
    /// When it happened; the hold runs from here.
    pub(crate) at: Instant,
}

/// One pane's armed outstanding-work deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeldWatch {
    /// When `input_progress` is read next.
    pub(crate) due: Instant,
    /// Consecutive `Raced` samples since the last judged one.
    pub(crate) raced: u8,
    /// The held write shown: when it was admitted, whether a reply is ahead of it, bytes queued.
    pub(crate) shown: Option<(Instant, bool, u64)>,
}

/// What one read of a pane's input progress does to its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldStep {
    /// Nothing is outstanding: disarm and hide `held`.
    Disarm,
    /// The oldest write is past the threshold: show it and read again at `due`.
    Show {
        /// When that write was admitted.
        admitted_at: Instant,
        /// The writer is busy with a terminal reply ahead of it.
        on_reply: bool,
        /// The next read, at the next whole second of the age.
        due: Instant,
    },
    /// The oldest write is younger than the threshold: hide `held` and read again at `due`.
    Rearm(Instant),
    /// The sample raced a completion: keep what is shown and read again at `due`.
    Raced(Instant),
    /// The writer stopped: disarm, hide `held`, and record `write failed` when this many were lost.
    Terminal(Option<u32>),
}

/// Judge one read of a pane's input progress at `now`, with `raced` consecutive raced samples
/// before it and `held_after` as the threshold. Every returned deadline is after `now`.
pub(crate) fn held_step(
    progress: sonicterm_io::pty::PtyInputProgress,
    raced: u8,
    now: Instant,
    held_after: Duration,
) -> HeldStep {
    use sonicterm_io::pty::PtyInputProgress;
    match progress {
        PtyInputProgress::Idle => HeldStep::Disarm,
        PtyInputProgress::Oldest { admitted_at, on_reply, .. } => {
            let age = now.saturating_duration_since(admitted_at);
            if age >= held_after {
                // The next read lands on the next whole second of this write's age.
                let next_secs = age.as_secs() + 1;
                HeldStep::Show {
                    admitted_at,
                    on_reply,
                    due: admitted_at + Duration::from_secs(next_secs),
                }
            } else {
                // When: age is under held_after, the write may still complete before it is worth showing.
                HeldStep::Rearm(admitted_at + held_after)
            }
        }
        PtyInputProgress::Raced => {
            let delay = if raced < 3 {
                Duration::from_millis(50)
            } else {
                // When: raced already reached 3 in a row, wait a whole threshold instead.
                held_after
            };
            HeldStep::Raced(now + delay)
        }
        PtyInputProgress::Terminal { failed, lost } => {
            HeldStep::Terminal((failed && lost > 0).then_some(lost))
        }
    }
}

/// The glyphs segment's input for `missing`, hidden below `min_count`. A capped count has no
/// proven figure to compare, so it always shows.
pub(crate) fn glyphs_input(
    missing: &sonicterm_gpu::missing_glyphs::MissingGlyphs,
    min_count: u64,
) -> Option<GlyphsInput> {
    use sonicterm_gpu::missing_glyphs::MissingGlyphValue;
    match missing.value {
        MissingGlyphValue::Exact(count) if count >= min_count => Some(GlyphsInput::Exact(count)),
        MissingGlyphValue::AtLeast(count) if count >= min_count => {
            Some(GlyphsInput::AtLeast(count))
        }
        MissingGlyphValue::Capped => Some(GlyphsInput::Capped),
        MissingGlyphValue::Exact(_) | MissingGlyphValue::AtLeast(_) => None,
    }
}

/// The tab-memory figure from each pane's cached charge `(bytes, charged_at)`: the plain sum when
/// every pane was charged within two retention intervals, the sum with the oldest charge's age
/// when some is older, and nothing when a pane was never charged, since no sum covers the tab.
pub(crate) fn tab_mem_input(
    panes: impl Iterator<Item = (u64, Option<Instant>)>,
    now: Instant,
) -> Option<TabMemInput> {
    let fresh_for = super::retention::RETENTION_SAMPLE_INTERVAL * 2;
    let mut bytes = 0u64;
    let mut oldest: Option<Instant> = None;
    for (pane_bytes, charged_at) in panes {
        let charged_at = charged_at?;
        bytes = bytes.saturating_add(pane_bytes);
        oldest = Some(oldest.map_or(charged_at, |oldest| oldest.min(charged_at)));
    }
    let age = now.saturating_duration_since(oldest?);
    Some(TabMemInput { bytes, stale_secs: (age > fresh_for).then_some(age.as_secs()) })
}

/// The presenter a window draws through: GDI only on Windows with the software degrade on;
/// otherwise wgpu, on a CPU or a hardware adapter.
pub(crate) fn presenter_kind(
    windows: bool,
    degraded: bool,
    software_adapter: bool,
) -> PresenterKind {
    if windows && degraded {
        PresenterKind::Gdi
    } else if software_adapter {
        // When: the frame is not GDI and software_adapter is set, wgpu runs on a CPU adapter.
        PresenterKind::SoftwareAdapter
    } else {
        // When: neither GDI nor software_adapter, wgpu runs on a hardware adapter.
        PresenterKind::Hardware
    }
}

/// Map a refused PTY input's error text to the reason the strip shows.
pub(crate) fn not_sent_reason(reason: &str) -> NotSentReason {
    use sonicterm_io::pty::PtyInputError;
    if reason == PtyInputError::QueueFull(Vec::new()).to_string() {
        NotSentReason::Busy
    } else if reason == PtyInputError::MessageTooLarge(Vec::new()).to_string() {
        // When: reason is the MessageTooLarge text, the message exceeded the per-message cap.
        NotSentReason::TooLarge
    } else {
        // When: reason is neither QueueFull nor MessageTooLarge, the writer has stopped.
        NotSentReason::Closed
    }
}

/// What a window's input segment shows: a `write failed` record beats `held`, and `held` (the
/// oldest outstanding write among `held_panes`) beats any other record.
pub(crate) fn input_state(
    record: Option<InputRecord>,
    held_panes: impl Iterator<Item = (Instant, bool, u64)>,
    now: Instant,
) -> Option<InputState> {
    if let Some(record) = record.filter(|record| matches!(record.state, InputState::WriteFailed(_)))
    {
        // When: record is WriteFailed, the writer stopped, so nothing it held is still held.
        return Some(record.state);
    }
    let oldest = held_panes.min_by_key(|(admitted_at, _, _)| *admitted_at);
    if let Some((admitted_at, on_reply, queued_bytes)) = oldest {
        // When: oldest is Some, a pane has a held write, shown with its own age.
        return Some(InputState::Held {
            secs: now.saturating_duration_since(admitted_at).as_secs(),
            on_reply,
            queued_bytes,
        });
    }
    record.map(|record| record.state)
}

/// The latest OSC 52 batch: payload-free, so no clipboard text is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipRecord {
    /// The pane whose program sent it.
    pub(crate) pane_id: u64,
    /// What happened to the batch's last request.
    pub(crate) outcome: ClipOutcome,
    /// Requests since the hold began.
    pub(crate) attempts: u64,
    /// When the batch was serviced; the hold runs from here.
    pub(crate) last_at: Instant,
}

/// What makes two selections count the same: the pane, the normalized
/// endpoints as `(col, abs_row)`, the screen and the eviction epoch. Output
/// elsewhere in the pane is not part of it, so it never forces a recount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelIdentity {
    /// The pane whose grid was counted.
    pane_id: u64,
    /// First selected cell.
    start: (usize, u64),
    /// Last selected cell.
    end: (usize, u64),
    /// The selection was made on the alternate screen.
    on_alt_screen: bool,
    /// Rows evicted from scrollback when the endpoints were taken.
    scrollback_evicted: u64,
    /// Copy mode's range has no content check of its own, so it carries a
    /// hash of its rows' content stamps; a window selection carries 0.
    content_stamp: u64,
}

/// One window's selection segment state: the last counted extent and the
/// last requested copy's result.
#[derive(Debug, Clone, Default)]
pub(crate) struct SelStatus {
    /// The extent of the selection identity counted last.
    counted: Option<(SelIdentity, SelectionExtent)>,
    /// When the last user-requested copy finished, and whether it reached the clipboard.
    last_copy: Option<(Instant, bool)>,
}

/// The identity of the selection the segment counts in `pane_id`'s `grid`:
/// copy mode's range while it has one, else the window selection.
fn sel_identity(
    selection: Option<&Selection>,
    copy_mode: Option<&CopyModeState>,
    pane_id: u64,
    grid: &Grid,
) -> Option<SelIdentity> {
    if let Some((start, end)) = copy_mode.and_then(CopyModeState::selected_range) {
        // When: copy mode's selected_range exists, it is what a yank copies, so it is counted.
        let (start, end) = ((start.0, start.1 as u64), (end.0, end.1 as u64));
        // A range over the row bound is capped without reading rows, so its content cannot matter.
        let content_stamp = if end.1 - start.1 < SELECTION_EXTENT_MAX_ROWS {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for row in start.1..=end.1 {
                grid.row_content_seq_at_abs(row).hash(&mut hasher);
            }
            hasher.finish()
        } else {
            // When: the span exceeds SELECTION_EXTENT_MAX_ROWS, the count is capped either way.
            0
        };
        return Some(SelIdentity {
            pane_id,
            start,
            end,
            on_alt_screen: grid.is_alt(),
            scrollback_evicted: grid.scrollback_evicted(),
            content_stamp,
        });
    }
    let selection = selection.filter(|selection| !selection.is_empty())?;
    if selection.pane_id.is_some_and(|owner| owner != pane_id) {
        // When: the selection's pane_id is another pane, this grid is not its content.
        return None;
    }
    let (start, end) = selection.normalized();
    Some(SelIdentity {
        pane_id,
        start: (usize::from(start.1), start.0),
        end: (usize::from(end.1), end.0),
        on_alt_screen: selection.on_alt_screen,
        scrollback_evicted: selection.scrollback_evicted,
        content_stamp: 0,
    })
}

/// Count the window's selection in the active pane's `grid` unless its
/// identity was counted already. Runs in the frame pass under the guard it
/// holds, after the selection was validated. Returns the cells it read when it counted.
pub(crate) fn refresh_sel_status(
    status: &mut SelStatus,
    selection: Option<&Selection>,
    copy_mode: Option<&CopyModeState>,
    pane_id: u64,
    grid: &Grid,
) -> Option<u64> {
    let Some(identity) = sel_identity(selection, copy_mode, pane_id, grid) else {
        // When: sel_identity finds no selection, there is nothing to count or keep.
        status.counted = None;
        return None;
    };
    if status.counted.is_some_and(|(counted, _)| counted == identity) {
        // When: the identity was counted already, its content is unchanged.
        return None;
    }
    let (extent, cells_read) = selection_extent_counted(grid, identity.start, identity.end);
    status.counted = Some((identity, extent));
    Some(cells_read)
}

/// What the selection segment shows for a window, and when that changes by
/// itself: a copy result for `copied_for`, else the counted extent of a
/// selection that still exists.
fn sel_input(
    status: &mut SelStatus,
    has_selection: bool,
    now: Instant,
    copied_for: Duration,
) -> (Option<SelInput>, Option<Instant>) {
    if let Some((at, copied)) = status.last_copy {
        // When: last_copy holds a result, it shows until `copied_for` has passed.
        let until = at + copied_for;
        if now < until {
            // When: now is before until, the copy result still shows and its end is the wake.
            let input = if copied { SelInput::Copied } else { SelInput::CopyFailed };
            return (Some(input), Some(until));
        }
        status.last_copy = None;
    }
    if !has_selection {
        // When: has_selection is false, the selection was cleared since it was counted.
        return (None, None);
    }
    let input = status.counted.map(|(_, extent)| {
        if extent.capped {
            SelInput::Capped { rows: extent.rows }
        } else {
            // When: extent.capped is false, the counts are exact.
            SelInput::Extent { lines: extent.lines, chars: extent.chars }
        }
    });
    (input, None)
}

/// Whether a paste into the focused pane would be unbracketed at a confirmed prompt on the primary
/// screen. `Unknown` and `Running` are never a prompt, so a pane without OSC 133 never warns.
pub(crate) fn paste_is_raw(
    warning: PasteWarning,
    facts: StatusPaneFacts,
    phase: CommandPhase,
) -> bool {
    warning == PasteWarning::WhenOff
        && !facts.bracketed_paste
        && !facts.alt_screen
        && phase == CommandPhase::Prompt
}

/// What the `ime` segment shows for a window, and when that ends by itself: a commit copy mode
/// dropped within `hold`, else a composition whose commit will not reach the pane.
fn ime_input(
    discarded_at: &mut Option<Instant>,
    composing: bool,
    route: ImeRoute,
    now: Instant,
    hold: Duration,
) -> (Option<ImeInput>, Option<Instant>) {
    if let Some(at) = *discarded_at {
        // When: discarded_at is set, the dropped commit shows until `hold` has passed.
        let until = at + hold;
        if now < until {
            // When: now is before until, the warning still shows and its end is the wake.
            return (Some(ImeInput::DiscardedInCopyMode), Some(until));
        }
        *discarded_at = None;
    }
    let input = match route {
        ImeRoute::Search if composing => Some(ImeInput::ToSearch),
        ImeRoute::Palette if composing => Some(ImeInput::ToPalette),
        // An ordinary composition into the pane, or no composition, is not a problem.
        _ => None,
    };
    (input, None)
}

/// Record `facts`' size generation for its pane. Returns whether the grid was resized since the
/// pane was last seen; the first sighting is the baseline, so spawn and first layout never show.
pub(crate) fn size_changed(seen: &mut HashMap<u64, u64>, facts: StatusPaneFacts) -> bool {
    seen.insert(facts.pane_id, facts.size_generation)
        .is_some_and(|previous| previous != facts.size_generation)
}

/// The rows the byte budget trimmed from the focused pane's history and nobody acknowledged: not
/// by a `history` click (`acked`), not by a scrollback clear.
pub(crate) fn history_input(facts: StatusPaneFacts, acked: u64) -> Option<u64> {
    let seen = acked.max(facts.trim_cleared);
    (facts.budget_trimmed > seen).then(|| facts.budget_trimmed - seen)
}

/// The grid size `size` shows for the focused pane while its resize is held, and when that ends.
/// A hold for another pane shows nothing, so a focus change never shows a size.
fn size_input(
    shown: &mut HashMap<WindowId, (u64, Instant)>,
    window_id: WindowId,
    facts: StatusPaneFacts,
    now: Instant,
) -> (Option<(u16, u16)>, Option<Instant>) {
    let Some(&(pane_id, until)) = shown.get(&window_id) else {
        // When: shown holds nothing for window_id, no resize is being shown.
        return (None, None);
    };
    if now >= until {
        // When: now reached until, the hold is over and the entry is dropped.
        shown.remove(&window_id);
        return (None, None);
    }
    if pane_id != facts.pane_id {
        // When: the held pane_id is not the focused pane, its size is not this pane's.
        return (None, Some(until));
    }
    (Some((facts.cols, facts.rows)), Some(until))
}

/// Milliseconds since the Unix epoch on the wall clock; 0 if the clock reads before it.
fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Whether the wall clock moved more than `interval_ms` beyond the monotonic clock since the last
/// sample `(at, wall_ms)`: the machine slept, so the history clock re-aligns and a sample is due now.
pub(crate) fn wall_clock_jumped(
    last: (Instant, u64),
    now: Instant,
    wall_ms: u64,
    interval_ms: u64,
) -> bool {
    let (at, last_wall_ms) = last;
    let monotonic_ms =
        u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX);
    wall_ms.saturating_sub(last_wall_ms) > monotonic_ms.saturating_add(interval_ms)
}

impl StatusLineState {
    /// Mark the startup load check done; `true` only the first time.
    pub(crate) fn take_startup_check(&mut self) -> bool {
        !std::mem::replace(&mut self.startup_checked, true)
    }
}

/// The reload failure for `error`, with the parse error's line when it names one.
pub(crate) fn reload_failure(error: &anyhow::Error) -> ReloadFailure {
    let text = format!("{error:#}");
    let line = text.match_indices("line ").find_map(|(start, word)| {
        let digits: String =
            text[start + word.len()..].chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    });
    ReloadFailure { line }
}

/// `path` for display, with `~` for the home directory.
fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let shown = match home.as_deref().map(std::path::Path::new) {
        Some(home) if !home.as_os_str().is_empty() && path.starts_with(home) => {
            let relative = path.strip_prefix(home).unwrap_or(path);
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                // When: `relative` is non-empty, the path is shown under `~`.
                format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
            }
        }
        _ => text.into_owned(),
    };
    sanitize_link_text(&shown)
}

/// How a window's frames are rendered, from its presenter and adapter: the Windows GDI
/// presenter (`software_render_mode` degrading on Windows), a CPU adapter (also when
/// `software_render_mode = "off"` declines to degrade), or forced software on real hardware.
pub(crate) fn classify_render(
    on_windows: bool,
    software_adapter: bool,
    degraded: bool,
) -> RenderHealth {
    match (on_windows && degraded, software_adapter, degraded) {
        (true, _, _) => RenderHealth::SoftwareGdi,
        (false, true, _) => RenderHealth::SoftwareAdapter,
        (false, false, true) => RenderHealth::SoftwareForced,
        (false, false, false) => RenderHealth::Hardware,
    }
}

/// How this window's frames are rendered.
fn render_health(renderer: &GpuRenderer) -> RenderHealth {
    classify_render(
        cfg!(windows),
        renderer.is_software_rendering(),
        renderer.is_software_render_degraded(),
    )
}

/// Whether this window waits on GPU recovery: recovery is in progress App-wide (`recovering`)
/// and this window's own renderer stopped (`stopped`) or no longer accepts GPU work
/// (`accepts_work`). A window still drawing on a healthy device is not waiting.
pub(crate) fn waits_on_gpu_recovery(recovering: bool, stopped: bool, accepts_work: bool) -> bool {
    recovering && (stopped || !accepts_work)
}

/// The link segment's input for `window`: the last built `previous` while an event has cleared
/// the hover and no frame has re-detected it, so a clear the next frame undoes never redraws the
/// strip; otherwise [`link_input`].
pub(crate) fn strip_link(
    window: &WindowState,
    window_id: WindowId,
    previous: Option<&Option<LinkInput>>,
    pending_since: &mut HashMap<WindowId, (std::path::PathBuf, Instant)>,
    now: Instant,
    i18n: &sonicterm_ui::i18n::I18n,
) -> (Option<LinkInput>, Option<Instant>) {
    match previous {
        // While hover_unresolved is set, the redraw its clear requested re-detects the target.
        Some(previous) if window.hover_unresolved => (previous.clone(), None),
        _ => link_input(window, window_id, pending_since, now, i18n),
    }
}

/// The link segment's input for `window`, and when a pending check becomes
/// worth showing.
fn link_input(
    window: &WindowState,
    window_id: WindowId,
    pending_since: &mut HashMap<WindowId, (std::path::PathBuf, Instant)>,
    now: Instant,
    i18n: &sonicterm_ui::i18n::I18n,
) -> (Option<LinkInput>, Option<Instant>) {
    let Some(status) = window.path_probe.status() else {
        // When: `path_probe` has no status, no local path is under the pointer; a hovered URL is the only link.
        pending_since.remove(&window_id);
        let link = window.hovered_url.as_ref().map(|hovered| {
            let label = hovered.label.as_ref().map(|label| (label.text.as_str(), label.complete));
            LinkInput::from_hover(&hovered.url, label)
        });
        return (link, None);
    };
    let blocked = |path: &std::path::Path, key: &str| LinkInput::Blocked {
        path: display_path(path),
        reason: i18n.translate(key),
    };
    let link = match status {
        ProbeStatus::Pending(path) => {
            // When: `Pending`, validation is running; it shows only after 300 ms.
            let since = match pending_since.get(&window_id) {
                Some((pending, since)) if pending == path => *since,
                _ => {
                    pending_since.insert(window_id, (path.to_path_buf(), now));
                    now
                }
            };
            let show_at = since + PENDING_SHOW_AFTER;
            if now < show_at {
                // When: `now` is before `show_at`, show nothing yet and wake at `show_at`.
                return (None, Some(show_at));
            }
            return (Some(LinkInput::Checking { path: display_path(path) }), None);
        }
        ProbeStatus::Selected(path, decision) => {
            let action = match decision {
                PathOpenDecision::Openable(PathKind::Directory) => Some(PathAction::OpenFolder),
                PathOpenDecision::Openable(PathKind::File) | PathOpenDecision::SourceReveal => {
                    Some(PathAction::Reveal)
                }
                #[cfg(any(target_os = "macos", test))]
                PathOpenDecision::Revealable(kind) => {
                    // When: `Revealable` on macOS, a folder opens and a file is selected in Finder.
                    Some(match kind {
                        PathKind::Directory => PathAction::OpenFolder,
                        PathKind::File => PathAction::Reveal,
                    })
                }
                PathOpenDecision::Blocked | PathOpenDecision::Missing => None,
            };
            match (action, decision) {
                (Some(action), _) => LinkInput::Path { path: display_path(path), action },
                (None, PathOpenDecision::Missing) => {
                    LinkInput::NotFound { path: display_path(path) }
                }
                (None, _) => blocked(path, "path-error-blocked"),
            }
        }
        ProbeStatus::Failed(path, "path-error-missing") => {
            LinkInput::NotFound { path: display_path(path) }
        }
        ProbeStatus::Failed(path, reason) => blocked(path, reason),
        ProbeStatus::NoDirectory(path) => blocked(path, "status-reason-no-directory"),
        ProbeStatus::AltScreen(path) => blocked(path, "status-reason-alt-screen"),
    };
    pending_since.remove(&window_id);
    (Some(link), None)
}

impl StatusLineState {
    /// Recompute what the strip shows from `file` and the session's choices.
    /// The strip pass's per-window step: build `window_id`'s strip from the session's effective
    /// lists and that window's `inputs`. `None` when neither changed since the window last built,
    /// so an unchanged pass formats nothing; `shows` counts segments that appear here now.
    fn rebuild_window_strip(
        &mut self,
        window_id: WindowId,
        inputs: StatusLineInputs,
        i18n: &sonicterm_ui::i18n::I18n,
        numbers: NumberStyle,
        shows: &mut StripShows,
    ) -> Option<StatusLineView> {
        let built = BuiltFrom { generation: self.generation, inputs };
        let previous = self.built.get(&window_id);
        if previous == Some(&built) {
            // When: `built` matches the last build, the inputs and settings are unchanged.
            return None;
        }
        // `History: trimmed` appears in this window now; later trims only change its count.
        if built.inputs.history_trimmed.is_some()
            && previous.is_none_or(|previous| previous.inputs.history_trimmed.is_none())
        {
            shows.history += 1;
        }
        // `Input: held` appears in this window now; a growing age is the same appearance.
        let held =
            |inputs: &StatusLineInputs| matches!(inputs.input, Some(InputState::Held { .. }));
        if held(&built.inputs) && previous.is_none_or(|previous| !held(&previous.inputs)) {
            shows.held += 1;
        }
        // `Glyphs:` appears in this window now; a changed count is the same appearance.
        if built.inputs.glyphs.is_some()
            && previous.is_none_or(|previous| previous.inputs.glyphs.is_none())
        {
            shows.glyphs += 1;
        }
        let view = build_view(&self.resolved, self.hide_sensitive, &built.inputs, i18n, numbers);
        self.built.insert(window_id, built);
        Some(view)
    }

    fn refresh_effective(&mut self, file: &ResolvedStatusLine) {
        self.resolved = self.overrides.effective(file);
        self.visible = self.resolved.enabled;
        self.hide_sensitive = self.resolved.hide_sensitive;
    }
}

impl App {
    /// Store the focused-pane facts a frame pass copied under its parser guard.
    /// The next loop pass rebuilds the strip if they changed what it shows.
    /// It also keeps the strip the frame presented on the window, for pointer routing.
    pub(super) fn observe_status_pane(&mut self, window_id: WindowId, facts: StatusPaneFacts) {
        if self.status_line.visible {
            self.status_line.pane_facts.insert(window_id, facts);
            if size_changed(&mut self.status_line.size_seen, facts) {
                let hold = Duration::from_millis(self.status_line.resolved.size.hold_ms);
                let until = Instant::now() + hold;
                self.status_line.size_shown.insert(window_id, (facts.pane_id, until));
            }
        }
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.status_strip = window.renderer.as_ref().and_then(GpuRenderer::status_line_hit);
        }
    }

    /// Record a reload result for the config file; only its own success clears it.
    pub(super) fn set_config_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.config_error = error.map(reload_failure);
    }

    /// Record a reload result for the theme; only its own success clears it.
    pub(super) fn set_theme_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.theme_error = error.map(reload_failure);
    }

    /// Record a reload result for the keymap; only its own success clears it.
    pub(super) fn set_keymap_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.keymap_error = error.map(reload_failure);
    }

    /// Rebuild every window's strip on the next pass (locale or theme change).
    pub(super) fn invalidate_status_line(&mut self) {
        self.status_line.generation = self.status_line.generation.wrapping_add(1);
    }

    /// Toggle the strip for this session and refit every window.
    pub(super) fn toggle_status_line(&mut self) {
        let visible = !self.status_line.visible;
        self.change_status_choices(|overrides, file| overrides.set_enabled(file, visible));
        tracing::info!("status line visible -> {}", self.status_line.visible);
    }

    /// Toggle hiding of sensitive segments for this session (screen sharing).
    pub(super) fn toggle_status_line_sensitive(&mut self) {
        let hide = !self.status_line.hide_sensitive;
        self.change_status_choices(|overrides, file| overrides.set_hide_sensitive(file, hide));
        tracing::info!("status line hide_sensitive -> {}", self.status_line.hide_sensitive);
    }

    /// Show segment `id` if it is hidden, hide it if it is shown, for this session.
    pub(super) fn toggle_status_segment(&mut self, id: StatusSegmentId) {
        self.change_status_choices(|overrides, file| overrides.toggle_segment(file, id));
    }

    /// Move a shown segment to the neighbouring list, for this session.
    pub(super) fn move_status_segment(&mut self, id: StatusSegmentId, direction: MoveDirection) {
        self.change_status_choices(|overrides, file| overrides.move_segment(file, id, direction));
    }

    /// Drop every session choice and return to the file's resolved lists.
    pub(super) fn reset_status_line_choices(&mut self) {
        self.change_status_choices(|overrides, _| overrides.reset());
    }

    /// The last good resolved file and the session's choices on it, for the checklist page.
    pub(super) fn status_line_file_and_choices(
        &self,
    ) -> (ResolvedStatusLine, &StatusLineOverrides) {
        let file =
            self.status_line.file.clone().unwrap_or_else(|| self.status_line.resolved.clone());
        (file, &self.status_line.overrides)
    }

    /// The strip the session shows now: the current `[status_line]` table with every choice.
    pub(in crate::app) fn current_status_line(&mut self) -> ResolvedStatusLine {
        self.apply_status_line_config();
        self.status_line.resolved.clone()
    }

    /// Route a right press at raster-px `(pointer_x, pointer_y)` in `window_id`. Inside the
    /// strip it opens the checklist for that window with the pressed segment highlighted, or the
    /// first row between segments, and returns `true`: the press never reaches a pane or a PTY.
    pub(in crate::app) fn right_press_status_line(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> bool {
        let Some(strip) =
            self.windows.get(&window_id).and_then(|window| window.status_strip.as_ref())
        else {
            // When: `status_strip` is None, the window presented no strip.
            return false;
        };
        if !strip.contains(pointer_y) {
            // When: `contains` is false, the press is outside the band, which handles no right press.
            return false;
        }
        let pressed = strip.segment_at(pointer_x, pointer_y).map(|segment| segment.id);
        self.open_status_segments(Some(window_id), pressed);
        true
    }

    /// Route a right-button press or release in `window_id` at its pointer. A press inside the
    /// strip opens the checklist; its release, and any right input outside the strip, does
    /// nothing, as before: SonicTerm reports no right button to a pane.
    pub(in crate::app) fn handle_right_mouse_input(
        &mut self,
        window_id: WindowId,
        state: winit::event::ElementState,
    ) -> bool {
        if state != winit::event::ElementState::Pressed {
            // When: state is a release, the press already decided; nothing else handles it.
            return false;
        }
        let Some((pointer_x, pointer_y)) =
            self.windows.get(&window_id).map(|window| window.cursor_pos)
        else {
            // When: windows lacks window_id, there is no strip to press.
            return false;
        };
        self.right_press_status_line(window_id, pointer_x as f32, pointer_y as f32)
    }

    /// Ask the checklist's window for a frame after a change. Every ask goes through the
    /// dispatch's record, so the strip pass, the checklist and the input path together ask each
    /// terminal window at most once.
    pub(in crate::app) fn request_checklist_redraw(&mut self) {
        let Some(id) = self.palette_attached_window.or(self.main_window_id) else {
            // When: palette_attached_window and main_window_id are both None, nothing hosts it.
            return;
        };
        self.mark_window_redraw(id, RedrawCause::Input);
        self.request_redraw_once(id);
    }

    /// What Save Current Settings writes: the strip the session shows, with every choice applied
    /// to the current `[status_line]` table.
    pub(super) fn status_line_save(&mut self) -> StatusLineSave {
        self.apply_status_line_config();
        StatusLineSave::from_resolved(&self.status_line.resolved)
    }

    /// Apply `change` to the session's choices, then redraw every window's strip once.
    fn change_status_choices(
        &mut self,
        change: impl FnOnce(&mut StatusLineOverrides, &ResolvedStatusLine),
    ) {
        self.apply_status_line_config();
        let (file, _) = self.status_line_file_and_choices();
        change(&mut self.status_line.overrides, &file);
        self.status_line.refresh_effective(&file);
        self.invalidate_status_line();
        let _ = self.sync_status_line(Instant::now());
    }

    /// Adopt `written` after a save wrote it: its table becomes the applied table, so a later
    /// reload of a different file is a real change, and its resolved lists become the baseline,
    /// dropping every choice they now satisfy.
    pub(super) fn adopt_saved_status_line(&mut self, written: WrittenStatusLine) {
        let WrittenStatusLine { table, resolved } = written;
        let previous = self.status_line.file.clone().unwrap_or_else(|| resolved.clone());
        self.status_line.overrides.reconcile(&previous, &resolved);
        self.config.status_line = table.clone();
        self.status_line.applied = Some(table);
        self.status_line.file = Some(resolved.clone());
        self.status_line.refresh_effective(&resolved);
        self.invalidate_status_line();
    }

    /// Apply a changed `[status_line]` table. A reload overrides a session
    /// toggle only when that key's own value in the file changed.
    fn apply_status_line_config(&mut self) {
        let table = &self.config.status_line;
        let state = &mut self.status_line;
        if state.applied.as_ref() == Some(table) {
            // When: the table is unchanged, the session toggles stand.
            return;
        }
        let previous = state.applied.replace(table.clone());
        match table.resolve() {
            Ok(file) => {
                // A reload drops only the choices whose own segment or key the file changed.
                if let Some(before) = state.file.as_ref() {
                    state.overrides.reconcile(before, &file);
                }
                state.refresh_effective(&file);
                state.file = Some(file);
            }
            // A table that reached here was validated at load; keep the last good one regardless.
            Err(error) => tracing::warn!("status line config rejected: {error:#}"),
        }
        if let Some(warning) = previous.is_some().then(|| table.unknown_keys_warning()).flatten() {
            // A reload left unknown keys, log them once; startup logs them from the loader.
            tracing::warn!(target: "sonicterm-cfg", "{warning}");
        }
        state.generation = state.generation.wrapping_add(1);
    }

    /// Apply visibility and content to every terminal window, sampling memory
    /// when due. Returns the next wake: the next sample, or the moment a
    /// pending path check becomes worth showing. Requests a redraw only for a
    /// window whose strip pixels changed.
    pub(super) fn sync_status_line(&mut self, now: Instant) -> Option<Instant> {
        self.apply_status_line_config();
        let woke = self.status_line.armed_wake.take().is_some_and(|armed| now >= armed);
        let visible = self.status_line.visible;
        let any_eligible = self.windows.values().any(|window| {
            window.role == super::WindowRole::Terminal
                && window.renderer.is_some()
                && window.frame_deadlines_allowed()
        });
        let sampled =
            |id| visible && any_eligible && self.status_line.resolved.segment(id).is_some();
        let samples_mem = sampled(StatusSegmentId::Mem);
        let samples_cpu = sampled(StatusSegmentId::Cpu);
        let samples_out = sampled(StatusSegmentId::Out);
        let samples_frame = sampled(StatusSegmentId::Frame);
        // `tab_mem` reads cached charges; sampling only refreshes a stale charge's shown age.
        let samples_any = samples_mem
            || samples_cpu
            || samples_out
            || samples_frame
            || sampled(StatusSegmentId::TabMem);
        let wall_ms = wall_now_ms();
        let interval_ms = self.status_line.resolved.update_interval_ms;
        let slept = self
            .status_line
            .last_sample
            .is_some_and(|last| wall_clock_jumped(last, now, wall_ms, interval_ms));
        if !samples_cpu || slept {
            // A rate spans one interval of a live session; the first sample after this shows nothing.
            self.status_line.cpu_last = None;
            if !samples_cpu {
                self.status_line.cpu_hysteresis = Hysteresis::default();
                self.status_line.cpu_shown = None;
            }
        }
        if !samples_out {
            self.stop_out_sampling();
        }
        if !samples_frame {
            self.stop_frame_sampling();
        }
        if !samples_any {
            // No visible window shows a sampled segment, nothing samples; resume samples at once.
            self.status_line.next_sample = None;
        } else if slept || self.status_line.next_sample.is_none_or(|due| now >= due) {
            // When: a sample is due, read each listed figure once for every window.
            if samples_mem {
                let bytes = match sonicterm_logging::process_report::sample_footprint().current {
                    MemoryMetric::Bytes(bytes) => Some(bytes),
                    MemoryMetric::Unsupported => None,
                };
                let mem = &mut self.status_line.mem;
                *mem = MemInput { previous_bytes: mem.bytes, bytes, history: mem.history };
                if let Some(bytes) = bytes {
                    self.status_line.history.record(wall_ms, bytes);
                }
            }
            if samples_cpu {
                self.sample_cpu(now);
            }
            if samples_out {
                self.sample_out(now);
            }
            if samples_frame {
                self.sample_frames(now);
            }
            self.status_line.last_sample = Some((now, wall_ms));
            self.status_line.next_sample = Some(now + Duration::from_millis(interval_ms));
            self.note_status_line(|counters| counters.samples += 1);
        }
        let sparkline = samples_mem && self.status_line.resolved.mem.sparkline;
        // The drawn cells are the completed buckets, so they change only at a rollover.
        self.status_line.mem.history = if sparkline {
            self.status_line.history.cells(wall_ms)
        } else {
            // When: `sparkline` is off, no cells are drawn and the strip's pixels never depend on them.
            Default::default()
        };
        let mut next_wake = self.status_line.next_sample;
        if sparkline {
            // The rollover is its own wake, so cells shift on time even with a 60 s interval.
            let rollover = now + Duration::from_millis(MemHistory::next_rollover_ms(wall_ms));
            next_wake = Some(next_wake.map_or(rollover, |wake| wake.min(rollover)));
        }
        let broadcast = match self.broadcast {
            super::BroadcastState::On { source_pane, .. } => {
                Some((source_pane, self.broadcast_receivers().len()))
            }
            _ => None,
        };
        let gpu_recovering = self.gpu_recovery_snapshot().is_some_and(|snapshot| {
            !matches!(snapshot.phase, sonicterm_gpu::recovery::RecoveryPhase::Active)
        });
        let locale = self.config.locale.clone();
        let palette_window = self
            .command_palette
            .is_open()
            .then(|| self.palette_attached_window.or(self.main_window_id))
            .flatten();
        let font = (self.config.font.size != self.configured_font_size).then(|| FontInput {
            live_tenths: (self.config.font.size * 10.0).round() as u32,
            configured_tenths: (self.configured_font_size * 10.0).round() as u32,
        });
        let path_check_listed = visible
            && any_eligible
            && self.status_line.resolved.segment(StatusSegmentId::PathCheck).is_some();
        if path_check_listed {
            // Two atomic loads; the hold rule fixes each wake when it arms it.
            let oldest = self.path_workers.as_ref().and_then(|workers| workers.busy().oldest());
            let waiting = self.path_workers.as_ref().map_or(0, |workers| workers.waiting());
            let settings = self.status_line.resolved.path_check;
            let (shown, wake) =
                self.status_line.path_check_hold.step(oldest, waiting, now, settings);
            self.status_line.path_check_shown = shown;
            if let Some(wake) = wake {
                next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
            }
        } else {
            // When: path_check_listed is false, no shown strip lists it, so it keeps no state or wake.
            self.status_line.path_check_hold = Default::default();
            self.status_line.path_check_shown = None;
        }
        if let Some((window_id, input)) = self.status_line.deferred_input.take() {
            self.note_input(window_id, input, now);
        }
        self.service_held_input(now);
        let Self { windows, status_line: state, i18n, dispatch_redraws, .. } = self;
        let listed = |id| state.resolved.segment(id).is_some();
        let input_shown = listed(StatusSegmentId::Input);
        let input_hold = Duration::from_secs(state.resolved.input.hold_secs);
        state.input.retain(|window_id, record| {
            input_shown && windows.contains_key(window_id) && now < record.at + input_hold
        });
        for wake in state
            .input
            .values()
            .map(|record| record.at + input_hold)
            .chain(state.held.values().map(|watch| watch.due))
        {
            next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
        }
        let alerts_shown = listed(StatusSegmentId::Alerts);
        let alert_settings = (visible && alerts_shown).then_some(state.resolved.alerts);
        let marked: HashMap<WindowId, Vec<(usize, sonicterm_ui::tabs::TabMarks)>> = windows
            .iter()
            .map(|(window_id, window)| {
                let tabs = window
                    .tabs
                    .tabs()
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| tab.marks.is_marked())
                    .map(|(index, tab)| (index + 1, tab.marks))
                    .collect();
                (*window_id, tabs)
            })
            .collect();
        let zoom_shown = listed(StatusSegmentId::Zoom);
        let glyphs_shown = listed(StatusSegmentId::Glyphs);
        let frame_listed = listed(StatusSegmentId::Frame);
        let path_check = state.path_check_shown.filter(|_| listed(StatusSegmentId::PathCheck));
        let cpu_value = listed(StatusSegmentId::Cpu).then_some(state.cpu_shown).flatten();
        let tab_mem_shown = listed(StatusSegmentId::TabMem);
        let glyph_settings = state.resolved.glyphs;
        state.glyph_sample.retain(|window_id, _| glyphs_shown && windows.contains_key(window_id));
        let (history_shown, font_shown, size_listed) = (
            listed(StatusSegmentId::History),
            listed(StatusSegmentId::Font),
            listed(StatusSegmentId::Size),
        );
        let font = font.filter(|_| font_shown);
        let live_panes: std::collections::HashSet<u64> =
            windows.values().flat_map(|window| window.panes.keys().copied()).collect();
        state.history_ack.retain(|pane_id, _| live_panes.contains(pane_id));
        state.size_seen.retain(|pane_id, _| live_panes.contains(pane_id));
        state.size_shown.retain(|window_id, _| windows.contains_key(window_id));
        state.clip_write.retain(|pane_id, _| live_panes.contains(pane_id));
        state.clip_log.retain(|pane_id, _| live_panes.contains(pane_id));
        let clip_hold = Duration::from_secs(state.resolved.clip.hold_secs);
        if state.clip.is_some_and(|record| now >= record.last_at + clip_hold) {
            // The hold ended, so the record is dropped and nothing wakes for it again.
            state.clip = None;
        }
        let clip = state.clip.filter(|_| state.resolved.segment(StatusSegmentId::Clip).is_some());
        if let Some(record) = clip {
            let wake = record.last_at + clip_hold;
            next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
        }
        let clip_owner = clip.and_then(|record| {
            windows
                .iter()
                .find(|(_, window)| window.panes.contains_key(&record.pane_id))
                .map(|(window_id, _)| *window_id)
        });
        let ime_shown = state.resolved.segment(StatusSegmentId::Ime).is_some();
        let ime_hold = Duration::from_secs(state.resolved.ime.hold_secs);
        let sel_shown = state.resolved.segment(StatusSegmentId::Sel).is_some();
        let copied_for = Duration::from_secs(state.resolved.sel.copied_secs);
        state.pane_facts.retain(|window_id, _| windows.contains_key(window_id));
        state.pending_since.retain(|window_id, _| windows.contains_key(window_id));
        state.built.retain(|window_id, _| windows.contains_key(window_id));
        let mut minimums_changed = false;
        let mut redraws = 0;
        let mut shows = StripShows::default();
        let numbers = NumberStyle::for_locale(if locale.is_empty() { "en" } else { &locale });
        for (window_id, window) in windows.iter_mut() {
            // Marks are recorded only while a shown strip lists `alerts`, renderer or not.
            window.alert_settings =
                alert_settings.filter(|_| window.role == super::WindowRole::Terminal);
            let Some(renderer) = window.renderer.as_mut() else {
                // When: a test-seeded entry has no renderer, it has no strip.
                continue;
            };
            // Only terminal windows carry the strip; the Memory Usage report never does.
            let shown = visible && window.role == super::WindowRole::Terminal;
            if renderer.set_status_line_visible(shown) {
                // When: `set_status_line_visible` flipped visibility, refit panes to the new bottom inset.
                minimums_changed = true;
                state.built.remove(window_id);
                let rects = App::compute_pane_rects_for(window);
                let Some(renderer) = window.renderer.as_ref() else {
                    // When: `renderer` is gone, the window cannot show a strip.
                    continue;
                };
                let (cell_width, cell_height) = renderer.cell_size();
                let inset = [
                    renderer.padding_left_px(),
                    renderer.padding_right_px(),
                    renderer.padding_top_px(),
                    renderer.padding_bottom_px(),
                ];
                resize_panes_to_rects(&window.panes, &rects, cell_width, cell_height, inset);
                window.mark_redraw(RedrawCause::Topology);
                dispatch_redraws.request(*window_id, window);
            }
            if !shown {
                // When: `shown` is false, the strip is off here: no band to route, nothing to format.
                window.status_strip = None;
                continue;
            }
            let previous_link = state.built.get(window_id).map(|built| &built.inputs.link);
            let (link, link_wake) =
                strip_link(window, *window_id, previous_link, &mut state.pending_since, now, i18n);
            if let Some(wake) = link_wake {
                next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
            }
            let sel = if sel_shown {
                let has_selection =
                    window.copy_mode.as_ref().and_then(CopyModeState::selected_range).is_some()
                        || window.selection.is_some_and(|selection| !selection.is_empty());
                let (sel, sel_wake) =
                    sel_input(&mut window.sel_status, has_selection, now, copied_for);
                if let Some(wake) = sel_wake {
                    next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
                }
                sel
            } else {
                // When: sel_shown is false, no list has `sel`, so nothing is formatted or woken for it.
                None
            };
            let ime = if ime_shown {
                let route = ime_route_for(window, palette_window == Some(*window_id));
                let composing = window.ime.is_composing();
                let (ime, ime_wake) =
                    ime_input(&mut window.ime_discarded_at, composing, route, now, ime_hold);
                if let Some(wake) = ime_wake {
                    next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
                }
                ime
            } else {
                // When: ime_shown is false, no list has `ime`, so nothing is formatted or woken for it.
                None
            };
            let facts = state.pane_facts.get(window_id).copied().unwrap_or_default();
            let phase = window
                .tab_states
                .get(window.tabs.active_index())
                .and_then(|tab| window.panes.get(&tab.active_pane))
                .map_or(CommandPhase::Unknown, |pane| pane.command_state.phase);
            let paste_raw = paste_is_raw(state.resolved.paste, facts, phase);
            let zoomed = zoom_shown
                && window
                    .tab_states
                    .get(window.tabs.active_index())
                    .is_some_and(|tab| tab.tree.zoomed_pane_id().is_some());
            let history_trimmed = if history_shown {
                history_input(facts, state.history_ack.get(&facts.pane_id).copied().unwrap_or(0))
            } else {
                // When: history_shown is false, no list has `history`.
                None
            };
            let size = if size_listed {
                let (size, size_wake) = size_input(&mut state.size_shown, *window_id, facts, now);
                if let Some(wake) = size_wake {
                    next_wake = Some(next_wake.map_or(wake, |next| next.min(wake)));
                }
                size
            } else {
                // When: size_listed is false, no list has `size`, so no hold is shown or woken for.
                None
            };
            let tab_mem = if tab_mem_shown {
                let leaves = window
                    .tab_states
                    .get(window.tabs.active_index())
                    .map(|tab| tab.tree.leaves())
                    .unwrap_or_default();
                let charged =
                    leaves.iter().filter_map(|pane_id| window.panes.get(pane_id)).map(|pane| {
                        let bytes = pane
                            .charges
                            .values()
                            .map(|charge| charge.committed_amount().bytes as u64)
                            .sum();
                        (bytes, pane.charged_at)
                    });
                tab_mem_input(charged, now)
            } else {
                // When: tab_mem_shown is false, no list has `tab_mem`.
                None
            };
            let glyphs = if glyphs_shown {
                // The count reads only the renderer's presented records; it changes only when a
                // frame presents or the fallback worker publishes, and each wake syncs here.
                let missing = window
                    .renderer
                    .as_mut()
                    .map(|renderer| {
                        renderer.presented_missing_glyphs(glyph_settings.ignore_private_use)
                    })
                    .unwrap_or_default();
                let glyphs = glyphs_input(&missing, glyph_settings.min_count);
                if glyphs.is_some() {
                    state.glyph_sample.insert(*window_id, missing.sample);
                } else {
                    // When: glyphs is None, nothing is shown, so there is nothing to list.
                    state.glyph_sample.remove(window_id);
                }
                glyphs
            } else {
                // When: glyphs_shown is false, no list has `glyphs`, so nothing is counted.
                None
            };
            let Some(renderer) = window.renderer.as_ref() else {
                // When: `renderer` is gone, the window cannot show a strip.
                continue;
            };
            let inputs = StatusLineInputs {
                mode: ModeInput {
                    copy: window.copy_mode.is_some(),
                    broadcast_receivers: broadcast
                        .filter(|(source, _)| window.panes.contains_key(source))
                        .map(|(_, receivers)| receivers),
                    mouse_captured: facts.mouse_captured,
                },
                scroll_lines: facts.scroll_lines,
                link,
                health: HealthInput {
                    config: state.config_error,
                    theme: state.theme_error,
                    keymap: state.keymap_error,
                    render: render_health(renderer),
                    gpu_recovering: waits_on_gpu_recovery(
                        gpu_recovering,
                        window.redraw.stopped_generation.is_some(),
                        renderer.device_accepts_gpu_work(),
                    ),
                },
                mem: state.mem,
                sel,
                paste_raw,
                ime,
                history_trimmed,
                font,
                size,
                zoomed,
                cpu_percent: cpu_value,
                frame: state.frame_shown.get(window_id).copied().filter(|_| frame_listed),
                path_check,
                out_rate: state.out_shown.get(window_id).copied().flatten(),
                tab_mem,
                glyphs,
                input: input_state(
                    state.input.get(window_id).copied(),
                    window.panes.keys().filter_map(|pane_id| state.held.get(pane_id)?.shown),
                    now,
                ),
                clip: clip.map(|record| ClipInput {
                    outcome: record.outcome,
                    attempts: record.attempts,
                    source: match clip_owner {
                        Some(owner) if owner == *window_id => window
                            .tab_states
                            .iter()
                            .position(|tab| tab.tree.leaves().contains(&record.pane_id))
                            .map_or(ClipSource::OtherWindow, |index| ClipSource::Tab(index + 1)),
                        Some(_) => ClipSource::OtherWindow,
                        None => ClipSource::Closed,
                    },
                }),
                alerts: if alerts_shown {
                    AlertsInput {
                        tabs: marked.get(window_id).cloned().unwrap_or_default(),
                        other_windows: marked
                            .iter()
                            .filter(|(other, tabs)| *other != window_id && !tabs.is_empty())
                            .count(),
                    }
                } else {
                    // When: alerts_shown is false, no list has `alerts`.
                    AlertsInput::default()
                },
            };
            let Some(view) =
                state.rebuild_window_strip(*window_id, inputs, i18n, numbers, &mut shows)
            else {
                // When: rebuild_window_strip returns None, the inputs and settings are unchanged.
                continue;
            };
            let Some(renderer) = window.renderer.as_mut() else {
                // When: `renderer` is gone, the window cannot show a strip.
                continue;
            };
            if renderer.set_status_line_view(view) {
                // The strip's pixels changed, draw them once.
                window.mark_redraw(RedrawCause::StatusLine);
                redraws += 1;
                dispatch_redraws.request(*window_id, window);
            }
        }
        if minimums_changed {
            self.refresh_all_window_minimums();
        }
        self.note_status_line(|counters| {
            counters.wakes += u64::from(woke);
            counters.redraws += redraws;
            counters.history_shows += shows.history;
            counters.held_shows += shows.held;
            counters.glyph_shows += shows.glyphs;
        });
        // A covered or hidden strip shows nothing a deadline could change, so it arms no wake; the
        // sync that runs when a window turns eligible again reads every deadline that passed.
        let wake = (visible && any_eligible).then_some(next_wake).flatten();
        self.status_line.armed_wake = wake;
        wake
    }

    /// Whether the frame pass counts selections: the strip is shown and lists `sel`.
    pub(super) fn counts_selection(&self) -> bool {
        self.status_line.visible
            && self.status_line.resolved.segment(StatusSegmentId::Sel).is_some()
    }

    /// Record the result of a copy the user requested in `window_id`; the
    /// segment shows it for `copied_secs`, even when the copy cleared the selection.
    pub(super) fn note_copy_result(&mut self, window_id: WindowId, copied: bool) {
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.sel_status.last_copy = Some((Instant::now(), copied));
        }
    }

    /// Route a left press at raster-px `(pointer_x, pointer_y)` in `window_id`. Returns `true`
    /// when the press is inside the strip: it is consumed, so no selection,
    /// mouse report or tab gesture starts there.
    pub(super) fn press_status_line(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> bool {
        let Some(strip) =
            self.windows.get(&window_id).and_then(|window| window.status_strip.as_ref())
        else {
            // When: `status_strip` is None, the window presented no strip.
            return false;
        };
        if !strip.contains(pointer_y) {
            // When: `contains` is false, the press is outside the band and routing continues.
            return false;
        }
        let click =
            strip.segment_at(pointer_x, pointer_y).and_then(|segment| segment.click.clone());
        match click {
            Some(SegmentClick::Action(action)) => {
                // When: the segment's click is an `Action`, run it for the pressed window.
                let _ = self.run_action_for_window(&action, window_id);
            }
            Some(SegmentClick::Handler(SegmentHandler::ScrollToBottom)) => {
                self.scroll_window_to_bottom(window_id);
            }
            Some(SegmentClick::Handler(SegmentHandler::AcknowledgeHistory)) => {
                self.acknowledge_history(window_id);
            }
            Some(SegmentClick::Handler(SegmentHandler::NextAlert)) => {
                self.activate_next_alert(window_id);
            }
            Some(SegmentClick::Handler(SegmentHandler::ClipSource)) => {
                self.activate_clip_source();
            }
            Some(SegmentClick::Handler(SegmentHandler::ListGlyphs)) => {
                self.list_missing_glyphs(window_id);
            }
            None => {
                // When: `click` is None, a gap or inert segment was pressed; consume it and do nothing.
            }
        }
        true
    }

    /// Record the native result of an accepted OSC 52 write from `pane_id`, `bytes` long.
    pub(super) fn note_clip_write(&mut self, pane_id: u64, bytes: usize, written: bool) {
        let outcome = if written {
            ClipOutcome::Written(bytes as u64)
        } else {
            // When: written is false, the native clipboard refused the accepted text.
            ClipOutcome::Failed
        };
        self.status_line.clip_write.insert(pane_id, outcome);
    }

    /// Record one VT batch's OSC 52 requests from `pane_id`: the display shows its last request and
    /// counts every request since the hold began. Refusals log one debug line per pane per second.
    pub(super) fn note_clip_attempts(
        &mut self,
        pane_id: u64,
        attempts: super::spawn_pane::ClipAttempts,
        now: Instant,
    ) {
        use super::spawn_pane::Osc52Refusal;
        let outcome = match attempts.latest {
            None => self.status_line.clip_write.remove(&pane_id),
            Some(Osc52Refusal::Read) => Some(ClipOutcome::ReadRefused),
            Some(Osc52Refusal::Selection(selection)) => {
                Some(ClipOutcome::SelectionRefused(selection))
            }
            Some(Osc52Refusal::TooLarge) => Some(ClipOutcome::TooLarge),
            Some(Osc52Refusal::Invalid) => Some(ClipOutcome::Invalid),
        };
        let Some(outcome) = outcome else {
            // When: outcome is None, the batch's accepted write left no result, so nothing is shown.
            return;
        };
        let hold = Duration::from_secs(self.status_line.resolved.clip.hold_secs);
        let carried = self
            .status_line
            .clip
            .filter(|record| now < record.last_at + hold)
            .map_or(0, |record| record.attempts);
        self.status_line.clip = Some(ClipRecord {
            pane_id,
            outcome,
            attempts: carried.saturating_add(u64::from(attempts.total)),
            last_at: now,
        });
        if attempts.refused == 0 {
            // When: attempts.refused is 0, there is no refusal to log.
            return;
        }
        let log = self.status_line.clip_log.entry(pane_id).or_insert((now, 0));
        if log.1 > 0 && now.duration_since(log.0) < Duration::from_secs(1) {
            // When: log.1 is set and log.0 is under a second old, this batch is only counted as suppressed.
            log.1 += 1;
            return;
        }
        let suppressed = log.1.saturating_sub(1);
        *log = (now, 1);
        tracing::debug!(
            pane_id,
            refused = attempts.refused,
            latest = ?attempts.latest,
            suppressed,
            "OSC 52 clipboard requests refused"
        );
    }

    /// Take one `cpu` sample: CPU time over wall time since the previous one, through the
    /// hysteresis. The first sample after enabling or resuming shows nothing.
    fn sample_cpu(&mut self, now: Instant) {
        let Some(cpu) = sonicterm_logging::process_report::sample_cpu_time() else {
            // When: sample_cpu_time is None, this platform has no CPU-time query.
            self.status_line.cpu_shown = None;
            return;
        };
        let Some((at, previous_cpu)) = self.status_line.cpu_last.replace((now, cpu)) else {
            // When: cpu_last was None, one sample gives no delta yet.
            return;
        };
        let settings = self.status_line.resolved.cpu;
        let total = match settings.scale {
            CpuScale::Total => true,
            CpuScale::Core => false,
            CpuScale::Auto => cfg!(windows),
        };
        let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get() as u64);
        let wall = now.saturating_duration_since(at);
        let percent = cpu_percent(cpu.saturating_sub(previous_cpu), wall, total, cores);
        let shown = self.status_line.cpu_hysteresis.step(
            percent,
            settings.min_percent,
            settings.hide_permille,
            settings.hold_samples,
        );
        self.status_line.cpu_shown = shown.then_some(percent);
    }

    /// Take one `out` sample: each pane's bytes since the previous sample, keyed by pane id so a
    /// pane moved between windows counts once, summed per window (or its focused pane) as a rate.
    // Ordering: output_counting and output_bytes are Relaxed; a rate needs only an eventual total.
    pub(super) fn sample_out(&mut self, now: Instant) {
        let settings = self.status_line.resolved.out;
        let Self { windows, status_line: state, .. } = self;
        let mut deltas: HashMap<u64, u64> = HashMap::new();
        for window in windows.values() {
            for (pane_id, pane) in &window.panes {
                pane.output_counting.store(true, std::sync::atomic::Ordering::Relaxed);
                let total = pane.output_bytes.load(std::sync::atomic::Ordering::Relaxed);
                let previous = state.out_prev.insert(*pane_id, total).unwrap_or(total);
                deltas.insert(*pane_id, total.saturating_sub(previous));
            }
        }
        state.out_prev.retain(|pane_id, _| deltas.contains_key(pane_id));
        state.out_hysteresis.retain(|window_id, _| windows.contains_key(window_id));
        state.out_shown.retain(|window_id, _| windows.contains_key(window_id));
        let Some(at) = state.out_last.replace(now) else {
            // When: out_last was None, this is the first sample since enabling: no rate yet.
            return;
        };
        let wall = now.saturating_duration_since(at);
        for (window_id, window) in windows.iter() {
            let focused =
                window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane);
            let bytes: u64 = window
                .panes
                .keys()
                .filter(|pane_id| !settings.pane_scope || Some(**pane_id) == focused)
                .map(|pane_id| deltas.get(pane_id).copied().unwrap_or(0))
                .sum();
            let rate = byte_rate(bytes, wall);
            let shown = state.out_hysteresis.entry(*window_id).or_default().step(
                rate,
                settings.min_bytes_per_sec,
                settings.hide_permille,
                settings.hold_samples,
            );
            state.out_shown.insert(*window_id, shown.then_some(rate));
        }
    }

    /// Take one `frame` sample per window: the content frames presented since the previous
    /// sample, their mean CPU-side time and the presenter. A window's first sample only starts
    /// its recording.
    fn sample_frames(&mut self, now: Instant) {
        let previous = self.status_line.frame_last.replace(now);
        let Self { windows, status_line: state, .. } = self;
        state.frame_shown.retain(|window_id, _| windows.contains_key(window_id));
        for (window_id, window) in windows.iter_mut() {
            let recorded = window.redraw.presented_frames.replace((0, 0));
            let (Some(at), Some((frames, total_us))) = (previous, recorded) else {
                // When: previous or recorded is None, this window's interval starts now.
                continue;
            };
            let Some(renderer) = window.renderer.as_ref() else {
                // When: renderer is None, the window presents nothing to measure.
                continue;
            };
            let wall_secs = now.saturating_duration_since(at).as_secs_f64().max(0.001);
            let presenter = presenter_kind(
                cfg!(windows),
                renderer.is_software_render_degraded(),
                renderer.is_software_rendering(),
            );
            state.frame_shown.insert(
                *window_id,
                FrameInput {
                    frames,
                    mean_frame_us: total_us.checked_div(frames).unwrap_or(0),
                    fps: (frames as f64 / wall_secs).round() as u64,
                    presenter,
                },
            );
        }
    }

    /// Stop `frame`: windows stop recording and the shown figures are dropped.
    fn stop_frame_sampling(&mut self) {
        if self.status_line.frame_last.take().is_none() && self.status_line.frame_shown.is_empty() {
            // When: frame_last was None and frame_shown is empty, nothing was recording.
            return;
        }
        for window in self.windows.values_mut() {
            window.redraw.presented_frames = None;
        }
        self.status_line.frame_shown.clear();
    }

    /// Note a submitted path probe or open, so `path_check` rechecks once its threshold passes,
    /// even when the worker starts the call later.
    pub(super) fn note_path_submit(&mut self) {
        if !self.status_line.visible
            || self.status_line.resolved.segment(StatusSegmentId::PathCheck).is_none()
        {
            // When: visible is false or segment(StatusSegmentId::PathCheck) is none, nothing reads it.
            return;
        }
        let now = self.dispatch_now();
        let settings = self.status_line.resolved.path_check;
        self.status_line.path_check_hold.note_submit(now, settings);
    }

    /// Stop `out`: every pane stops counting and the per-pane totals and shown rates are dropped.
    // Ordering: output_counting is stored Relaxed; the VT worker only needs to see it eventually.
    pub(super) fn stop_out_sampling(&mut self) {
        if self.status_line.out_last.is_none() && self.status_line.out_prev.is_empty() {
            // When: out_last is None and out_prev is empty, nothing was counting.
            return;
        }
        for window in self.windows.values() {
            for pane in window.panes.values() {
                pane.output_counting.store(false, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let state = &mut self.status_line;
        state.out_prev.clear();
        state.out_last = None;
        state.out_hysteresis.clear();
        state.out_shown.clear();
    }

    /// Show one notification in `window_id` listing the codepoints its `glyphs` count names, up to
    /// 16, with the font hint. Only the codepoints are shown, never the text around them, and the
    /// clipboard is not touched.
    pub(super) fn list_missing_glyphs(&mut self, window_id: WindowId) {
        let Some(sample) =
            self.status_line.glyph_sample.get(&window_id).filter(|sample| !sample.is_empty())
        else {
            // When: glyph_sample has nothing for window_id, its strip names no codepoint.
            return;
        };
        let listed = sample
            .iter()
            .map(|codepoint| format!("U+{:04X} {codepoint}", u32::from(*codepoint)))
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "No font SonicTerm can find covers {listed}. Install fonts covering these \
             characters or choose another [font].family in sonicterm.toml."
        );
        let kind = if Some(window_id) == self.main_window_id {
            super::FrontmostKind::Main
        } else {
            // When: window_id is not the main window, the notification goes to that child.
            super::FrontmostKind::Child(window_id)
        };
        self.show_notification_for_kind(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Info,
            message,
        );
    }

    /// Read each due pane's input progress once and apply it to that pane's deadline; drops the
    /// deadlines of closed panes, and every deadline while no shown strip lists `input`.
    fn service_held_input(&mut self, now: Instant) {
        let listed = self.status_line.visible
            && self.status_line.resolved.segment(StatusSegmentId::Input).is_some();
        if !listed {
            // When: listed is false, nothing shows `held`, so no deadline is kept or read.
            self.status_line.held.clear();
            return;
        }
        let held_after = Duration::from_millis(self.status_line.resolved.input.held_after_ms);
        let Self { windows, status_line: state, .. } = self;
        let mut failed = Vec::new();
        state.held.retain(|pane_id, watch| {
            let found = windows.iter().find_map(|(window_id, window)| {
                Some((*window_id, window.panes.get(pane_id)?.pty.as_ref()?))
            });
            let Some((window_id, pty)) = found else {
                // When: found is None, the pane retired or lost its writer; its deadline goes with it.
                return false;
            };
            if now < watch.due {
                // When: now is before watch.due, this pane's deadline has not fired.
                return true;
            }
            match held_step(pty.input_progress(), watch.raced, now, held_after) {
                HeldStep::Disarm => false,
                HeldStep::Show { admitted_at, on_reply, due } => {
                    let queued_bytes = pty.queued_input_bytes() as u64;
                    *watch = HeldWatch {
                        due,
                        raced: 0,
                        shown: Some((admitted_at, on_reply, queued_bytes)),
                    };
                    true
                }
                HeldStep::Rearm(due) => {
                    *watch = HeldWatch { due, raced: 0, shown: None };
                    true
                }
                HeldStep::Raced(due) => {
                    watch.due = due;
                    watch.raced = watch.raced.saturating_add(1);
                    true
                }
                HeldStep::Terminal(lost) => {
                    if let Some(lost) = lost {
                        failed.push((window_id, lost));
                    }
                    false
                }
            }
        });
        for (window_id, lost) in failed {
            self.note_input(window_id, InputState::WriteFailed(lost), now);
        }
    }

    /// Record `input` as `window_id`'s latest input event, while a shown strip lists `input`.
    pub(super) fn note_input(&mut self, window_id: WindowId, input: InputState, now: Instant) {
        if !self.status_line.visible
            || self.status_line.resolved.segment(StatusSegmentId::Input).is_none()
        {
            // When: visible is false or segment(StatusSegmentId::Input) is none, nothing would show the record.
            return;
        }
        self.status_line.input.insert(window_id, InputRecord { state: input, at: now });
    }

    /// Record `input` for the window that holds `pane_id`; nothing when no window does.
    pub(super) fn note_pane_input(&mut self, pane_id: u64, input: InputState) {
        let window_id = self
            .windows
            .iter()
            .find_map(|(id, window)| window.panes.contains_key(&pane_id).then_some(*id));
        if let Some(window_id) = window_id {
            let now = self.dispatch_now();
            self.note_input(window_id, input, now);
        }
    }

    /// Test seam: what `window_id`'s input segment last recorded.
    #[cfg(test)]
    pub(super) fn __test_input_record(&self, window_id: WindowId) -> Option<InputState> {
        self.status_line.input.get(&window_id).map(|record| record.state)
    }

    /// Record `input` for `pane_id`'s window at the next sync, from a path that cannot borrow
    /// the App mutably. The latest call wins.
    pub(super) fn defer_pane_input(&self, window_id: WindowId, input: InputState) {
        self.status_line.deferred_input.set(Some((window_id, input)));
    }

    /// Arm `pane_id`'s outstanding-work deadline after an accepted discrete write, unless one is
    /// already armed or no shown strip lists `input`.
    pub(super) fn arm_held_input(&mut self, pane_id: u64) {
        if !self.status_line.visible
            || self.status_line.resolved.segment(StatusSegmentId::Input).is_none()
            || self.status_line.held.contains_key(&pane_id)
        {
            // When: the strip cannot show `held`, or pane_id already has a deadline, arm nothing.
            return;
        }
        let held_after = Duration::from_millis(self.status_line.resolved.input.held_after_ms);
        let due = self.dispatch_now() + held_after;
        self.status_line.held.insert(pane_id, HeldWatch { due, raced: 0, shown: None });
    }

    /// Activate the tab whose program sent the latest OSC 52 request, in its own window, and focus
    /// that window; nothing happens once the pane has closed.
    pub(super) fn activate_clip_source(&mut self) {
        let Some(pane_id) = self.status_line.clip.map(|record| record.pane_id) else {
            // When: no clip record is shown, there is no source to go to.
            return;
        };
        let target = self.windows.iter().find_map(|(window_id, window)| {
            window
                .tab_states
                .iter()
                .position(|tab| tab.tree.leaves().contains(&pane_id))
                .map(|index| (*window_id, index))
        });
        let Some((window_id, index)) = target else {
            // When: target is None, no window holds pane_id: the source tab has closed.
            return;
        };
        if Some(window_id) == self.main_window_id {
            self.activate_main_tab(index);
        } else {
            // When: window_id is not the main window, it is a torn-out child.
            self.activate_tab_in_child(window_id, index);
        }
        if let Some(window) = self.windows.get(&window_id).and_then(|state| state.window.as_ref()) {
            window.focus_window();
        }
    }

    /// Activate the next marked tab after `window_id`'s active one, wrapping; with none there, the
    /// first marked tab of another window, which is focused.
    pub(super) fn activate_next_alert(&mut self, window_id: WindowId) {
        let next_marked = |window: &WindowState, from: usize| {
            let count = window.tabs.len();
            (1..=count)
                .map(|step| (from + step) % count)
                .find(|index| window.tabs.tabs()[*index].marks.is_marked())
        };
        let here = self
            .windows
            .get(&window_id)
            .and_then(|window| next_marked(window, window.tabs.active_index()));
        let target = here.map(|index| (window_id, index)).or_else(|| {
            self.windows
                .iter()
                .filter(|(other, window)| {
                    **other != window_id && window.role == super::WindowRole::Terminal
                })
                .find_map(|(other, window)| {
                    next_marked(window, window.tabs.len()).map(|index| (*other, index))
                })
        });
        let Some((target_window, index)) = target else {
            // When: target is None, no window has a marked tab.
            return;
        };
        if Some(target_window) == self.main_window_id {
            self.activate_main_tab(index);
        } else {
            // When: target_window is not the main window, it is a torn-out child.
            self.activate_tab_in_child(target_window, index);
        }
        if target_window != window_id {
            if let Some(window) =
                self.windows.get(&target_window).and_then(|state| state.window.as_ref())
            {
                window.focus_window();
            }
        }
    }

    /// Acknowledge the budget trims of `window_id`'s focused pane, as the last frame saw them.
    fn acknowledge_history(&mut self, window_id: WindowId) {
        if let Some(facts) = self.status_line.pane_facts.get(&window_id).copied() {
            self.status_line.history_ack.insert(facts.pane_id, facts.budget_trimmed);
        }
    }

    /// Return `window_id`'s focused pane to the bottom of its history.
    fn scroll_window_to_bottom(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: the window closed after the press, there is nothing to scroll.
            return;
        };
        let pane_id = window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane);
        let Some(pane) = pane_id.and_then(|pane_id| window.panes.get_mut(&pane_id)) else {
            // When: the active tab has no live pane, there is nothing to scroll.
            return;
        };
        if pane.release_viewport() {
            // The view moved, present the live tail.
            window.mark_redraw(RedrawCause::Input);
            window.request_window_redraw();
        }
    }
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;
