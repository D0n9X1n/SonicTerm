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

use sonicterm_cfg::status_line::{ResolvedStatusLine, StatusLineConfig, StatusSegmentId};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_logging::MemoryMetric;
use sonicterm_ui::status_line::{
    build_view, sanitize_link_text, HealthInput, LinkInput, MemHistory, MemInput, ModeInput,
    NumberStyle, PathAction, ReloadFailure, RenderHealth, SegmentClick, SegmentHandler,
    StatusLineInputs,
};
use winit::window::WindowId;

use super::path_target::{PathKind, PathOpenDecision, ProbeStatus};
use super::redraw::RedrawCause;
use super::window_state::WindowState;
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
}

impl StatusPaneFacts {
    /// Facts of the pane whose parser is `parser`, viewed from `viewport_top_abs`.
    pub(crate) fn of(parser: &sonicterm_vt::vt::Parser, viewport_top_abs: Option<u64>) -> Self {
        let grid = parser.grid();
        let live_top = GpuRenderer::resolved_view_top_abs_legacy(grid, None);
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        Self {
            mouse_captured: parser.mouse_tracking() != sonicterm_vt::vt::MouseTracking::Off,
            scroll_lines: live_top.saturating_sub(view_top),
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

/// The status line state the App owns, one per process.
#[derive(Debug, Default)]
pub(crate) struct StatusLineState {
    /// Whether the strip is shown this session.
    visible: bool,
    /// Whether sensitive segments are hidden this session.
    hide_sensitive: bool,
    /// The `[status_line]` table last applied; a reload overrides a session
    /// toggle only when the file's own value changed.
    applied: Option<StatusLineConfig>,
    /// The resolved table the strip draws.
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

impl App {
    /// Store the focused-pane facts a frame pass copied under its parser guard.
    /// The next loop pass rebuilds the strip if they changed what it shows.
    /// It also keeps the strip the frame presented on the window, for pointer routing.
    pub(super) fn observe_status_pane(&mut self, window_id: WindowId, facts: StatusPaneFacts) {
        if self.status_line.visible {
            self.status_line.pane_facts.insert(window_id, facts);
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
        self.status_line.visible = !self.status_line.visible;
        self.invalidate_status_line();
        tracing::info!("status line visible -> {}", self.status_line.visible);
        let _ = self.sync_status_line(Instant::now());
    }

    /// Toggle hiding of sensitive segments for this session (screen sharing).
    pub(super) fn toggle_status_line_sensitive(&mut self) {
        self.status_line.hide_sensitive = !self.status_line.hide_sensitive;
        self.invalidate_status_line();
        tracing::info!("status line hide_sensitive -> {}", self.status_line.hide_sensitive);
        let _ = self.sync_status_line(Instant::now());
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
            Ok(resolved) => state.resolved = resolved,
            // A table that reached here was validated at load; keep the last good one regardless.
            Err(error) => tracing::warn!("status line config rejected: {error:#}"),
        }
        if previous.as_ref().is_none_or(|previous| previous.enabled != table.enabled) {
            state.visible = table.enabled;
        }
        if previous.as_ref().is_none_or(|previous| previous.hide_sensitive != table.hide_sensitive)
        {
            state.hide_sensitive = table.hide_sensitive;
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
        let samples_mem = visible
            && any_eligible
            && self.status_line.resolved.segment(StatusSegmentId::Mem).is_some();
        let wall_ms = wall_now_ms();
        let interval_ms = self.status_line.resolved.update_interval_ms;
        let slept = self
            .status_line
            .last_sample
            .is_some_and(|last| wall_clock_jumped(last, now, wall_ms, interval_ms));
        if !samples_mem {
            // No visible window shows `mem`, nothing samples; resume samples at once.
            self.status_line.next_sample = None;
        } else if slept || self.status_line.next_sample.is_none_or(|due| now >= due) {
            // When: a sample is due, read SonicTerm's own figure once for every window.
            let bytes = match sonicterm_logging::process_report::sample_footprint().current {
                MemoryMetric::Bytes(bytes) => Some(bytes),
                MemoryMetric::Unsupported => None,
            };
            let mem = &mut self.status_line.mem;
            *mem = MemInput { previous_bytes: mem.bytes, bytes, history: mem.history };
            if let Some(bytes) = bytes {
                self.status_line.history.record(wall_ms, bytes);
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
        let Self { windows, status_line: state, i18n, .. } = self;
        state.pane_facts.retain(|window_id, _| windows.contains_key(window_id));
        state.pending_since.retain(|window_id, _| windows.contains_key(window_id));
        state.built.retain(|window_id, _| windows.contains_key(window_id));
        let mut minimums_changed = false;
        let mut redraws = 0;
        for (window_id, window) in windows.iter_mut() {
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
                window.request_window_redraw();
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
            let facts = state.pane_facts.get(window_id).copied().unwrap_or_default();
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
            };
            let built = BuiltFrom { generation: state.generation, inputs };
            if state.built.get(window_id) == Some(&built) {
                // When: `built` matches the last build, the inputs and settings are unchanged.
                continue;
            }
            let view = build_view(
                &state.resolved,
                state.hide_sensitive,
                &built.inputs,
                i18n,
                NumberStyle::for_locale(if locale.is_empty() { "en" } else { &locale }),
            );
            state.built.insert(*window_id, built);
            let Some(renderer) = window.renderer.as_mut() else {
                // When: `renderer` is gone, the window cannot show a strip.
                continue;
            };
            if renderer.set_status_line_view(view) {
                // The strip's pixels changed, draw them once.
                window.mark_redraw(RedrawCause::StatusLine);
                redraws += 1;
                window.request_window_redraw();
            }
        }
        if minimums_changed {
            self.refresh_all_window_minimums();
        }
        self.note_status_line(|counters| {
            counters.wakes += u64::from(woke);
            counters.redraws += redraws;
        });
        let wake = visible.then_some(next_wake).flatten();
        self.status_line.armed_wake = wake;
        wake
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
            None => {
                // When: `click` is None, a gap or inert segment was pressed; consume it and do nothing.
            }
        }
        true
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
