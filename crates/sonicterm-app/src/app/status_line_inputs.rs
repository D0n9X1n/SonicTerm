//! Pure status-line projections from retained pane, selection and health state.

use super::*;

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
    let fresh_for = super::super::retention::RETENTION_SAMPLE_INTERVAL * 2;
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

/// The `mem` figure: the process footprint minus the graphics memory the OS charges to it.
///
/// Only macOS separates that part: its `task_info` graphics ledger names the bytes charged to
/// the physical footprint, which are subtracted, flooring at zero because the two figures are
/// read apart. DXGI reports Windows video memory by segment group, not as a share of private
/// commit, and Linux has no graphics figure, so those footprints are kept. An unsupported
/// footprint stays unsupported.
pub(crate) fn mem_without_graphics(
    footprint: MemoryMetric,
    graphics: &GraphicsMemory,
) -> MemoryMetric {
    match (footprint, graphics.source, graphics.footprint) {
        (MemoryMetric::Bytes(bytes), GraphicsSource::TaskVmInfo, MemoryMetric::Bytes(charged)) => {
            MemoryMetric::Bytes(bytes.saturating_sub(charged))
        }
        _ => footprint,
    }
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

/// The identity of the selection the segment counts in `pane_id`'s `grid`:
/// copy mode's range while it has one, else the window selection.
pub(super) fn sel_identity(
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
pub(super) fn sel_input(
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
pub(super) fn ime_input(
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
pub(super) fn size_input(
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
pub(super) fn wall_now_ms() -> u64 {
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

/// `path` for display, with `~` for the home directory.
pub(super) fn display_path(path: &std::path::Path) -> String {
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
/// The tabs of `window` that carry a mark, numbered from 1.
pub(super) fn marked_tabs(window: &WindowState) -> Vec<(usize, sonicterm_ui::tabs::TabMarks)> {
    window
        .tabs
        .tabs()
        .iter()
        .enumerate()
        .filter(|(_, tab)| tab.marks.is_marked())
        .map(|(index, tab)| (index + 1, tab.marks))
        .collect()
}

/// `window`'s health: reload failures, how it renders, and whether it waits on GPU recovery.
pub(super) fn window_health(
    state: &StatusLineState,
    window: &WindowState,
    gpu_recovering: bool,
) -> HealthInput {
    let renderer = window.renderer.as_ref();
    let render = renderer.map(render_health).unwrap_or_default();
    #[cfg(test)]
    let render = state.render_override.unwrap_or(render);
    HealthInput {
        config: state.config_error,
        theme: state.theme_error,
        keymap: state.keymap_error,
        render,
        gpu_recovering: waits_on_gpu_recovery(
            gpu_recovering,
            window.redraw.stopped_generation.is_some(),
            renderer.is_some_and(GpuRenderer::device_accepts_gpu_work),
        ),
    }
}

pub(super) fn render_health(renderer: &GpuRenderer) -> RenderHealth {
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

/// The link segment's input for `window`: nothing unless the platform's open modifier (Cmd on
/// macOS, Ctrl elsewhere) is held, so ordinary hovering adds no strip noise; the last built
/// `previous` while an event has cleared the hover and no frame has re-detected it, so a clear
/// the next frame undoes never redraws the strip; otherwise [`link_input`].
pub(crate) fn strip_link(
    window: &WindowState,
    previous: Option<&Option<LinkInput>>,
    i18n: &sonicterm_ui::i18n::I18n,
) -> Option<LinkInput> {
    if !open_modifier_in(window.modifiers) {
        // When: open_modifier_in(window.modifiers) is false, the segment shows nothing.
        return None;
    }
    match previous {
        // While hover_unresolved is set, the redraw its clear requested re-detects the target.
        Some(previous) if window.hover_unresolved => previous.clone(),
        _ => link_input(window, i18n),
    }
}

/// The link segment's input for `window`: a hovered URL, or a local path only when detection
/// failed. A path that resolved is already underlined, and one still being checked has nothing
/// to report yet, so neither adds strip text.
pub(super) fn link_input(
    window: &WindowState,
    i18n: &sonicterm_ui::i18n::I18n,
) -> Option<LinkInput> {
    let Some(status) = window.path_probe.status() else {
        // When: `path_probe` has no status, no local path is under the pointer; a hovered URL is the only link.
        return window.hovered_url.as_ref().map(|hovered| {
            let label = hovered.label.as_ref().map(|label| (label.text.as_str(), label.complete));
            LinkInput::from_hover(&hovered.url, label)
        });
    };
    path_failure(status, i18n)
}

/// The strip text for a path whose detection failed, or `None` when it resolved or is still
/// being checked.
pub(super) fn path_failure(
    status: ProbeStatus<'_>,
    i18n: &sonicterm_ui::i18n::I18n,
) -> Option<LinkInput> {
    let blocked = |path: &std::path::Path, key: &str| LinkInput::Blocked {
        path: display_path(path),
        reason: i18n.translate(key),
    };
    match status {
        // A check still running, or a path that resolved to an action, is not a failure.
        ProbeStatus::Pending(_) => None,
        ProbeStatus::Selected(path, decision) => match decision {
            PathOpenDecision::Missing => Some(LinkInput::NotFound { path: display_path(path) }),
            PathOpenDecision::Blocked => Some(blocked(path, "path-error-blocked")),
            // An openable or revealable path is already underlined, so it adds no strip text.
            _ => None,
        },
        ProbeStatus::Failed(path, "path-error-missing") => {
            Some(LinkInput::NotFound { path: display_path(path) })
        }
        ProbeStatus::Failed(path, reason) => Some(blocked(path, reason)),
        ProbeStatus::NoDirectory(path) => Some(blocked(path, "status-reason-no-directory")),
        ProbeStatus::AltScreen(path) => Some(blocked(path, "status-reason-alt-screen")),
    }
}

#[cfg(test)]
#[path = "status_line_inputs_tests.rs"]
mod status_line_inputs_tests;
