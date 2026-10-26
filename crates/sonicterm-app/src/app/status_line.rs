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
use sonicterm_logging::graphics_memory::{GraphicsMemory, GraphicsSource};
use sonicterm_logging::MemoryMetric;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::selection::{
    selection_extent_counted, Selection, SelectionExtent, SELECTION_EXTENT_MAX_ROWS,
};
use sonicterm_ui::status_line::{
    build_view, sanitize_link_text, AlertsInput, ClipInput, ClipOutcome, ClipSource, FontInput,
    GlyphsInput, HealthInput, ImeInput, InputState, LinkInput, MemHistory, MemInput, ModeInput,
    NotSentReason, NumberStyle, ReloadFailure, RenderHealth, SegmentClick, SegmentHandler,
    SelInput, StatusLineInputs, StatusLineView,
};
use sonicterm_ui::status_line::{
    byte_rate, cpu_percent, FrameInput, Hysteresis, PathCheckInput, PresenterKind, TabMemInput,
};
use winit::window::WindowId;

use super::path_target::{open_modifier_in, PathOpenDecision, ProbeStatus};
use super::redraw::RedrawCause;
use super::window_keyboard::{ime_route_for, ImeRoute};
use super::window_state::WindowState;
use super::CommandPhase;
use super::{resize_panes_to_rects, App};

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
    /// Tests: the render health every window reports, as a renderer would.
    #[cfg(test)]
    pub(super) render_override: Option<RenderHealth>,
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
    /// The last full pass showed the strip or served a reader, so a hidden, unheard sync still
    /// owes one pass to take that down. `false` at start: nothing has been set up yet.
    full_sync_owed: bool,
    /// A pane retired since the last sync, so the per-pane records are pruned once.
    pub(super) panes_retired: bool,
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
            if let Some(renderer) = window.renderer.as_ref() {
                // The renderer commits a strip layout only when its frame presented; routing and
                // the next accessibility flush read it. No tree is built on this redraw path.
                window.status_strip = renderer.status_line_hit();
            }
        }
    }

    /// Record a reload result for the config file; only its own success clears it.
    pub(super) fn set_config_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.config_error = error.map(reload_failure);
        // The About window's Config row shows the message itself.
        self.about.config_error = error.map(|error| format!("{error:#}"));
    }

    /// Record a reload result for the theme; only its own success clears it.
    pub(super) fn set_theme_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.theme_error = error.map(reload_failure);
        self.about.theme_error = error.map(|error| format!("{error:#}"));
    }

    /// Record a reload result for the keymap; only its own success clears it.
    pub(super) fn set_keymap_reload_error(&mut self, error: Option<&anyhow::Error>) {
        self.status_line.keymap_error = error.map(reload_failure);
        self.about.keymap_error = error.map(|error| format!("{error:#}"));
    }

    /// Rebuild every window's strip on the next pass (locale or theme change).
    /// Whether the strip is shown this session.
    pub(super) fn status_line_shown(&self) -> bool {
        self.status_line.visible
    }

    /// Whether sensitive segments are hidden this session.
    pub(super) fn status_hide_sensitive(&self) -> bool {
        self.status_line.hide_sensitive
    }

    /// `[status_line.accessibility]` as resolved.
    pub(super) fn status_accessibility(
        &self,
    ) -> &sonicterm_cfg::status_line::ResolvedAccessibility {
        &self.status_line.resolved.accessibility
    }

    /// The inputs `window_id`'s accessibility tree is named from: the strip pass's last inputs,
    /// with the App-owned values (memory, CPU, health, alert marks) read now, so they stay current
    /// while the strip is off or the window is covered, parked or device-stopped.
    pub(super) fn accessible_inputs(&self, window_id: WindowId) -> Option<StatusLineInputs> {
        let window = self.windows.get(&window_id)?;
        let state = &self.status_line;
        let mut inputs =
            state.built.get(&window_id).map(|built| built.inputs.clone()).unwrap_or_default();
        inputs.mem = state.mem;
        inputs.cpu_percent = state.cpu_shown;
        let gpu_recovering = self.gpu_recovery_snapshot().is_some_and(|snapshot| {
            !matches!(snapshot.phase, sonicterm_gpu::recovery::RecoveryPhase::Active)
        });
        inputs.health = window_health(state, window, gpu_recovering);
        inputs.alerts = AlertsInput {
            tabs: marked_tabs(window),
            other_windows: self
                .windows
                .iter()
                .filter(|(other, other_window)| {
                    **other != window_id && !marked_tabs(other_window).is_empty()
                })
                .count(),
        };
        Some(inputs)
    }

    /// Format `ids` over `inputs` with each segment's own formatter, whether or not the strip
    /// lists it, so a spoken name never depends on the visual list.
    pub(super) fn accessible_view(
        &self,
        ids: &[StatusSegmentId],
        inputs: &StatusLineInputs,
    ) -> StatusLineView {
        let state = &self.status_line;
        let mut resolved = state.resolved.clone();
        for id in ids {
            if resolved.segment(*id).is_none() {
                if let Some(unlisted) =
                    state.resolved.unlisted.iter().find(|segment| segment.id == *id)
                {
                    resolved.segments.push(unlisted.clone());
                }
            }
        }
        let locale = self.config.locale.as_str();
        let numbers = NumberStyle::for_locale(if locale.is_empty() { "en" } else { locale });
        build_view(&resolved, state.hide_sensitive, inputs, &self.i18n, numbers)
    }

    /// The strip settings generation, bumped by every toggle, reload and locale change.
    pub(super) fn status_generation(&self) -> u64 {
        self.status_line.generation
    }

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

    /// Move the strip to `position` in every window for this session; the panes refit once.
    pub(super) fn move_status_line(
        &mut self,
        position: sonicterm_cfg::status_line::StatusPosition,
    ) {
        self.change_status_choices(|overrides, file| overrides.set_position(file, position));
        tracing::info!("status line position -> {}", position.as_str());
    }

    /// Make the strip's text `steps` steps of 0.05 larger (negative: smaller) in every window for
    /// this session, within 0.3..=1.0.
    pub(super) fn step_status_line_font_scale(&mut self, steps: i16) {
        let scale = self.status_line.resolved.font_scale.stepped(steps);
        self.change_status_choices(|overrides, file| overrides.set_font_scale(file, scale));
        tracing::info!("status line font_scale -> {}", scale.as_f32());
    }

    /// Return the strip's text to the default 0.8 in every window for this session.
    pub(super) fn reset_status_line_font_scale(&mut self) {
        let scale = sonicterm_cfg::status_line::StatusFontScale::DEFAULT;
        self.change_status_choices(|overrides, file| overrides.set_font_scale(file, scale));
        tracing::info!("status line font_scale -> {}", scale.as_f32());
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

    /// Whether a sync has nothing to do: the strip is hidden, no assistive technology listens, no
    /// input is held, and the last full pass already took down whatever a shown strip set up.
    fn status_line_settled_hidden(&self) -> bool {
        let state = &self.status_line;
        !state.visible
            && !state.full_sync_owed
            && state.held.is_empty()
            && !self.accessibility.values().any(|access| access.active)
    }

    /// Drop the per-pane records of panes no window holds any more, after a retirement.
    fn forget_retired_panes(&mut self) {
        let Self { windows, status_line: state, .. } = self;
        let live =
            |pane_id: &u64| windows.values().any(|window| window.panes.contains_key(pane_id));
        state.history_ack.retain(|pane_id, _| live(pane_id));
        state.size_seen.retain(|pane_id, _| live(pane_id));
        state.clip_write.retain(|pane_id, _| live(pane_id));
        state.clip_log.retain(|pane_id, _| live(pane_id));
        state.held.retain(|pane_id, _| live(pane_id));
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
}

/// The strip setters of `window`'s renderer, or of a headless test window's stand-in.
fn strip_settings(window: &mut WindowState) -> Option<&mut dyn StripSettings> {
    if let Some(renderer) = window.renderer.as_mut() {
        // When: the window has a renderer, its own setters change the strip.
        return Some(renderer);
    }
    #[cfg(test)]
    if let Some(recorder) = window.test_strip_settings.as_mut() {
        // When: a headless test window holds a stand-in, it records what the sync handed it.
        return Some(recorder);
    }
    None
}

/// The strip setters the sync pass hands each window's renderer. Each returns `true` when it
/// changed what the strip draws, as the renderer's own setters do.
trait StripSettings {
    /// Move the strip; `true` when a shown strip moved.
    fn set_position(&mut self, position: sonicterm_cfg::status_line::StatusPosition) -> bool;
    /// Size the strip's text; `true` when the size changed.
    fn set_font_scale(&mut self, font_scale: f32) -> bool;
    /// Show or hide the strip; `true` when its visibility changed.
    fn set_visible(&mut self, visible: bool) -> bool;
}

impl StripSettings for GpuRenderer {
    fn set_position(&mut self, position: sonicterm_cfg::status_line::StatusPosition) -> bool {
        self.set_status_line_position(position)
    }

    fn set_font_scale(&mut self, font_scale: f32) -> bool {
        self.set_status_line_font_scale(font_scale)
    }

    fn set_visible(&mut self, visible: bool) -> bool {
        self.set_status_line_visible(visible)
    }
}

/// Test-only stand-in for a headless window's renderer strip: it keeps what the sync pass handed
/// it, with the renderer's change rules, so a test sees each window's update without a GPU.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TestStripSettings {
    pub(crate) position: sonicterm_cfg::status_line::StatusPosition,
    pub(crate) font_scale: f32,
    pub(crate) visible: bool,
}

#[cfg(test)]
impl Default for TestStripSettings {
    fn default() -> Self {
        Self {
            position: sonicterm_cfg::status_line::StatusPosition::default(),
            font_scale: sonicterm_gpu::status_strip::default_font_scale(),
            visible: false,
        }
    }
}

#[cfg(test)]
impl StripSettings for TestStripSettings {
    fn set_position(&mut self, position: sonicterm_cfg::status_line::StatusPosition) -> bool {
        let moved = self.position != position;
        self.position = position;
        // As the renderer: a hidden strip reserves nothing, so moving it changes no inset.
        moved && self.visible
    }

    fn set_font_scale(&mut self, font_scale: f32) -> bool {
        let changed = (self.font_scale - font_scale).abs() >= f32::EPSILON;
        self.font_scale = font_scale;
        changed
    }

    fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;
        self.visible = visible;
        changed
    }
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;

#[path = "status_line_inputs.rs"]
mod inputs;
pub(crate) use inputs::{
    classify_render, glyphs_input, held_step, history_input, input_state, mem_without_graphics,
    not_sent_reason, paste_is_raw, presenter_kind, refresh_sel_status, size_changed, strip_link,
    tab_mem_input, waits_on_gpu_recovery, wall_clock_jumped,
};
use inputs::{
    ime_input, marked_tabs, render_health, sel_input, size_input, wall_now_ms, window_health,
};
#[path = "status_line_actions.rs"]
mod actions;
#[path = "status_line_sync.rs"]
mod sync;

#[cfg(test)]
use inputs::display_path;

#[cfg(test)]
use inputs::path_failure;
