//! Status-strip synchronization and renderer propagation for one dispatch.

use super::*;

impl StatusLineState {
    /// Recompute what the strip shows from `file` and the session's choices.
    /// The strip pass's per-window step: build `window_id`'s strip from the session's effective
    /// lists and that window's `inputs`. `None` when neither changed since the window last built,
    /// so an unchanged pass formats nothing; `shows` counts segments that appear here now.
    pub(super) fn rebuild_window_strip(
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

    pub(super) fn refresh_effective(&mut self, file: &ResolvedStatusLine) {
        self.resolved = self.overrides.effective(file);
        self.visible = self.resolved.enabled;
        self.hide_sensitive = self.resolved.hide_sensitive;
    }
}

impl App {
    /// Apply visibility and content to every terminal window, sampling memory
    /// when due. Returns the next wake: the next sample, or the moment a
    /// pending path check becomes worth showing. Requests a redraw only for a
    /// window whose strip pixels changed.
    pub(in crate::app) fn sync_status_line(&mut self, now: Instant) -> Option<Instant> {
        self.apply_status_line_config();
        if std::mem::take(&mut self.status_line.panes_retired) {
            // A pane retired since the last sync, so its per-pane records go now, shown or not.
            self.forget_retired_panes();
        }
        if self.status_line_settled_hidden() {
            // When: `status_line_settled_hidden()` holds, the sync has nothing to change; hidden input
            // is never recorded, so a deferred one is dropped.
            self.status_line.deferred_input.set(None);
            return None;
        }
        self.note_status_line(|counters| counters.full_passes += 1);
        let woke = self.status_line.armed_wake.take().is_some_and(|armed| now >= armed);
        let visible = self.status_line.visible;
        let any_eligible = self.windows.values().any(|window| {
            window.role == super::super::WindowRole::Terminal
                && window.renderer.is_some()
                && window.frame_deadlines_allowed()
        });
        // A connected assistive technology hears memory and CPU while no window can draw.
        let listening = self.accessibility.values().any(|state| state.active);
        let sampled = |id| {
            visible
                && (any_eligible || listening)
                && self.status_line.resolved.segment(id).is_some()
        };
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
                // Only the macOS ledger is subtracted, so no DXGI adapter is queried here.
                let footprint = sonicterm_logging::process_report::sample_footprint().current;
                let graphics = sonicterm_logging::graphics_memory::sample(None);
                let bytes = match mem_without_graphics(footprint, &graphics) {
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
            super::super::BroadcastState::On { source_pane, .. } => {
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
        let font = (self.config.font.size != self.configured_font_size)
            .then(|| FontInput::from_sizes(self.config.font.size, self.configured_font_size));
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
        let Self { windows, status_line: state, i18n, dispatch_redraws, accessibility, .. } = self;
        // An assistive technology hears new alert marks even while no shown strip lists `alerts`.
        let alerts_announced = state
            .resolved
            .accessibility
            .announce
            .contains(&StatusSegmentId::Alerts)
            .then_some(state.resolved.alerts);
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
        // Per-pane records are pruned when a pane retires (`forget_retired_panes`), not here.
        state.size_shown.retain(|window_id, _| windows.contains_key(window_id));
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
        state.built.retain(|window_id, _| windows.contains_key(window_id));
        let mut minimums_changed = false;
        let mut redraws = 0;
        let mut shows = StripShows::default();
        let numbers = NumberStyle::for_locale(if locale.is_empty() { "en" } else { &locale });
        for (window_id, window) in windows.iter_mut() {
            // Marks are recorded while a shown strip lists `alerts`, or a connected assistive
            // technology announces them, renderer or not.
            let listening = accessibility.get(window_id).is_some_and(|access| access.active);
            window.alert_settings = alert_settings
                .or(alerts_announced.filter(|_| listening))
                .filter(|_| window.role == super::super::WindowRole::Terminal);
            // Only terminal windows carry the strip; the Memory Usage report never does.
            let shown = visible && window.role == super::super::WindowRole::Terminal;
            let Some(strip) = strip_settings(window) else {
                // When: strip_settings finds no renderer (a test-seeded entry), there is no strip.
                continue;
            };
            // Both setters run, position first, so a reload that moves and shows the strip
            // refits the panes once.
            let moved = strip.set_position(state.resolved.position);
            // A new text size keeps the strip's height, so it redraws without a refit.
            let rescaled = strip.set_font_scale(state.resolved.font_scale.as_f32());
            let refit = strip.set_visible(shown) | moved;
            if rescaled {
                // The strip's text size changed and its tiles were dropped, so it redraws.
                window.mark_redraw(RedrawCause::StatusLine);
                dispatch_redraws.request(*window_id, window);
            }
            if refit {
                // When: `set_visible` or `moved` changed the insets, refit panes once.
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
            if window.renderer.is_none() {
                // When: the renderer is none, a test stand-in took the setters; nothing is drawn.
                continue;
            }
            let previous_link = state.built.get(window_id).map(|built| &built.inputs.link);
            let link = strip_link(window, previous_link, i18n);
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
        let wake = (visible && (any_eligible || listening)).then_some(next_wake).flatten();
        self.status_line.armed_wake = wake;
        // A shown strip or a served reader leaves state a later hidden, unheard sync must take down.
        self.status_line.full_sync_owed = visible || listening;
        wake
    }
}

#[cfg(test)]
#[path = "status_line_sync_tests.rs"]
mod status_line_sync_tests;
