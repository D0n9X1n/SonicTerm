//! Status-line click, held-input, clipboard and alert actions.

use super::*;

impl App {
    /// Route a left press at raster-px `(pointer_x, pointer_y)` in `window_id`. Returns `true`
    /// when the press is inside the strip: it is consumed, so no selection,
    /// mouse report or tab gesture starts there.
    pub(in crate::app) fn press_status_line(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> bool {
        let hit = self.windows.get(&window_id).and_then(|window| window.status_strip.as_ref()).map(
            |strip| {
                let click = strip
                    .segment_at(pointer_x, pointer_y)
                    .and_then(|segment| segment.click.clone());
                (strip.contains(pointer_y), click)
            },
        );
        let Some((true, click)) = hit else {
            // When: `hit` is None or outside the band, the press is outside the strip: strip focus
            // ends and routing continues.
            self.end_status_focus(window_id);
            return false;
        };
        if let Some(click) = click {
            self.run_status_click(window_id, click);
        }
        true
    }

    /// Run a presented segment's click for `window_id`: a mouse press, Enter or Space in strip
    /// focus, or an assistive technology's `Click`, through one dispatcher.
    pub(in crate::app) fn run_status_click(&mut self, window_id: WindowId, click: SegmentClick) {
        // Focus leaves the strip first, so an editor, palette or overlay the click opens owns the
        // keyboard and IME at once, whichever input clicked.
        self.end_status_focus(window_id);
        match click {
            SegmentClick::Action(action) => {
                // When: the segment's click is an `Action`, run it for the pressed window.
                let _ = self.run_action_for_window(&action, window_id);
            }
            SegmentClick::Handler(SegmentHandler::ScrollToBottom) => {
                self.scroll_window_to_bottom(window_id);
            }
            SegmentClick::Handler(SegmentHandler::AcknowledgeHistory) => {
                self.acknowledge_history(window_id);
            }
            SegmentClick::Handler(SegmentHandler::NextAlert) => {
                self.activate_next_alert(window_id);
            }
            SegmentClick::Handler(SegmentHandler::ClipSource) => {
                self.activate_clip_source();
            }
            SegmentClick::Handler(SegmentHandler::ListGlyphs) => {
                self.list_missing_glyphs(window_id);
            }
        }
    }

    /// Record the native result of an accepted OSC 52 write from `pane_id`, `bytes` long.
    pub(in crate::app) fn note_clip_write(&mut self, pane_id: u64, bytes: usize, written: bool) {
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
    pub(in crate::app) fn note_clip_attempts(
        &mut self,
        pane_id: u64,
        attempts: super::super::spawn_pane::ClipAttempts,
        now: Instant,
    ) {
        use super::super::spawn_pane::Osc52Refusal;
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
    pub(super) fn sample_cpu(&mut self, now: Instant) {
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
    pub(in crate::app) fn sample_out(&mut self, now: Instant) {
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
    pub(super) fn sample_frames(&mut self, now: Instant) {
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
    pub(super) fn stop_frame_sampling(&mut self) {
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
    pub(in crate::app) fn note_path_submit(&mut self) {
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
    pub(in crate::app) fn stop_out_sampling(&mut self) {
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
    pub(in crate::app) fn list_missing_glyphs(&mut self, window_id: WindowId) {
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
            super::super::FrontmostKind::Main
        } else {
            // When: window_id is not the main window, the notification goes to that child.
            super::super::FrontmostKind::Child(window_id)
        };
        self.show_notification_for_kind(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Info,
            message,
        );
    }

    /// Read each due pane's input progress once and apply it to that pane's deadline; drops the
    /// deadlines of closed panes, and every deadline while no shown strip lists `input`.
    pub(super) fn service_held_input(&mut self, now: Instant) {
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
    pub(in crate::app) fn note_input(
        &mut self,
        window_id: WindowId,
        input: InputState,
        now: Instant,
    ) {
        if !self.status_line.visible
            || self.status_line.resolved.segment(StatusSegmentId::Input).is_none()
        {
            // When: visible is false or segment(StatusSegmentId::Input) is none, nothing would show the record.
            return;
        }
        self.status_line.input.insert(window_id, InputRecord { state: input, at: now });
    }

    /// Record `input` for the window that holds `pane_id`; nothing when no window does.
    pub(in crate::app) fn note_pane_input(&mut self, pane_id: u64, input: InputState) {
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
    pub(in crate::app) fn __test_input_record(&self, window_id: WindowId) -> Option<InputState> {
        self.status_line.input.get(&window_id).map(|record| record.state)
    }

    /// Record `input` for `pane_id`'s window at the next sync, from a path that cannot borrow
    /// the App mutably. The latest call wins.
    pub(in crate::app) fn defer_pane_input(&self, window_id: WindowId, input: InputState) {
        self.status_line.deferred_input.set(Some((window_id, input)));
    }

    /// Arm `pane_id`'s outstanding-work deadline after an accepted discrete write, unless one is
    /// already armed or no shown strip lists `input`.
    pub(in crate::app) fn arm_held_input(&mut self, pane_id: u64) {
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
    pub(in crate::app) fn activate_clip_source(&mut self) {
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
    pub(in crate::app) fn activate_next_alert(&mut self, window_id: WindowId) {
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
                    **other != window_id && window.role == super::super::WindowRole::Terminal
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
    pub(super) fn acknowledge_history(&mut self, window_id: WindowId) {
        if let Some(facts) = self.status_line.pane_facts.get(&window_id).copied() {
            self.status_line.history_ack.insert(facts.pane_id, facts.budget_trimmed);
        }
    }

    /// Return `window_id`'s focused pane to the bottom of its history.
    pub(super) fn scroll_window_to_bottom(&mut self, window_id: WindowId) {
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
#[path = "status_line_actions_tests.rs"]
mod status_line_actions_tests;
