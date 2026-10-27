//! Synthetic driver setup, source-window input and deterministic motion.

use super::*;

impl Probe {
    /// The driver a phase starts with; `None` when the phase injects nothing.
    pub(super) fn start_driver(
        &mut self,
        event_loop: &ActiveEventLoop,
        phase: &PhaseSpec,
    ) -> DriverState {
        match phase.driver {
            Driver::None => DriverState::None,
            Driver::Typing { role, chars, per_second, settle_ms } => {
                let active = self.active_pane();
                let Some(pane) =
                    self.role_panes.get(role).copied().filter(|pane| Some(*pane) == active)
                else {
                    // When: the role's pane is not active, an Ime::Commit would type into another pane.
                    self.invalidate(
                        event_loop,
                        format!("role {role}'s pane is not the active pane for typing"),
                    );
                    return DriverState::None;
                };
                DriverState::Typing(Typing {
                    role,
                    pane,
                    chars,
                    interval: Duration::from_secs(1) / per_second.max(1),
                    settle: Duration::from_millis(settle_ms),
                    origin: None,
                    identity: RowIdentity::default(),
                    cols: 0,
                    first_at: None,
                    typed: 0,
                    done_at: None,
                })
            }
            Driver::Sweep { hertz } => {
                let Some((lanes, left, right)) = self.sweep_geometry() else {
                    self.invalidate(event_loop, "no pane layout for the sweep".to_owned());
                    return DriverState::None;
                };
                let interval = Duration::from_secs(1) / hertz.max(1);
                DriverState::Sweep(Sweep {
                    interval,
                    hertz,
                    started: Instant::now(),
                    ticks: 0,
                    lanes,
                    left,
                    right,
                })
            }
            Driver::Drag { hertz } => {
                let Some((start, end)) = self.drag_geometry() else {
                    self.invalidate(event_loop, "no pane layout for the drag".to_owned());
                    return DriverState::None;
                };
                // A 0.6 s cycle: press, about 0.5 s of motion, release, then idle until the next
                // press, so consecutive presses are farther apart than a double click.
                let cycle_ticks = (hertz.max(2) * 3 / 5).max(3);
                let deadline = self.phase_deadline(phase).unwrap_or(self.run_deadline);
                DriverState::Drag(Drag {
                    interval: Duration::from_secs(1) / hertz.max(1),
                    started: Instant::now(),
                    ticks: 0,
                    cycle_ticks,
                    move_ticks: (cycle_ticks * 5 / 6).clamp(1, cycle_ticks - 2),
                    start,
                    end,
                    pressed: false,
                    deadline,
                    // Only the status-line plan checks what its frames count; S6 keeps its schedule.
                    quiet_from: self
                        .plan
                        .status_line
                        .then(|| deadline.checked_sub(STATUS_LINE_DRAG_SETTLE))
                        .flatten(),
                    finished: false,
                })
            }
            Driver::AtlasRetry => self.start_atlas_retry(event_loop),
            Driver::Park { row, col, hertz } => {
                let Some(point) = self.cell_point(row, col) else {
                    self.invalidate(event_loop, "no pane layout for the parked pointer".to_owned());
                    return DriverState::None;
                };
                let pointed = self.pointed_cell(point);
                let entry = self.role_pane(0).map(|pane| {
                    let parser = pane.parser.lock();
                    let viewport = pane.viewport_top_abs;
                    let problem = crate::record::park_entry_problem(
                        phase.name,
                        parser.grid(),
                        viewport,
                        &self.sentinels[0],
                        pointed,
                        (row, col),
                    );
                    (problem, crate::record::park_snapshot(parser.grid(), viewport, row))
                });
                let snapshot = match entry {
                    Some((None, snapshot)) => snapshot,
                    Some((Some(reason), _)) => {
                        // When: park_entry_problem names a reason, the phase cannot measure its claim.
                        self.invalidate(event_loop, reason);
                        return DriverState::None;
                    }
                    None => {
                        self.invalidate(event_loop, "no pane for the parked pointer".to_owned());
                        return DriverState::None;
                    }
                };
                self.park_entry = Some(snapshot);
                DriverState::Park(Park {
                    interval: Duration::from_secs(1) / hertz.max(1),
                    started: Instant::now(),
                    ticks: 0,
                    point,
                })
            }
            Driver::Commit { steps, settle_ms } => DriverState::Commit(Commit {
                steps,
                sent: 0,
                started: Instant::now(),
                settle: Duration::from_millis(settle_ms),
            }),
            Driver::Wheel { hertz, role } => {
                let Some((retained, center)) = self.wheel_setup(role) else {
                    self.invalidate(event_loop, format!("no pane layout for role {role}'s wheel"));
                    return DriverState::None;
                };
                self.scrollback_rows_retained = Some(retained as u64);
                // The wheel scrolls the pane under the pointer.
                self.pointer(event_loop, center);
                // Three lines a tick, so this many ticks reach the top of the retained history.
                let ticks_each_way =
                    u32::try_from(retained.div_ceil(3)).unwrap_or(u32::MAX / 2).max(1);
                DriverState::Wheel(Wheel {
                    interval: Duration::from_secs(1) / hertz.max(1),
                    started: Instant::now(),
                    sent: 0,
                    total: ticks_each_way * 2,
                })
            }
        }
    }
}

impl Probe {
    /// Hover-only sweep lanes: the middle of the tab bar and three grid rows, and the x range.
    pub(super) fn sweep_geometry(&self) -> Option<([f64; 4], f64, f64)> {
        let pane = self.active_pane()?;
        let renderer = self.app.main_renderer()?;
        let layout = renderer.pane_layout(pane)?;
        let size = self.app.main_window()?.inner_size();
        let (width, height) = (f64::from(size.width), f64::from(size.height));
        // The tab bar is pinned to the bottom. The sweep only hovers there; it never presses.
        let tab_bar = height - f64::from(renderer.tab_bar_logical_height()) / 2.0;
        let (top, span) = (f64::from(layout.origin_y_logical), f64::from(layout.h_logical));
        Some((
            [tab_bar, top + span * 0.2, top + span * 0.5, top + span * 0.8],
            width * 0.1,
            width * 0.9,
        ))
    }
}

impl Probe {
    /// The center of the active pane's cell (`row`, `col`), in window pixels.
    pub(super) fn cell_point(&self, row: u16, col: u16) -> Option<(f64, f64)> {
        let pane = self.active_pane()?;
        let layout = self.app.main_renderer()?.pane_layout(pane)?;
        Some((
            f64::from(layout.origin_x_logical)
                + (f64::from(col) + 0.5) * f64::from(layout.cell_w_logical),
            f64::from(layout.origin_y_logical)
                + (f64::from(row) + 0.5) * f64::from(layout.cell_h_logical),
        ))
    }
}

impl Probe {
    /// The cell of role 0's pane that window point `point` resolves to; `None` when it resolves to
    /// another pane or to no cell.
    pub(super) fn pointed_cell(&self, point: (f64, f64)) -> Option<(u16, u16)> {
        let role_pane = *self.role_panes.first()?;
        let (pane, row, col) =
            self.app.main_renderer()?.pixel_to_pane_cell(point.0 as f32, point.1 as f32)?;
        // A renderer that names no pane (0) leaves the pointer in the active pane.
        let in_role_pane =
            pane == role_pane || (pane == 0 && self.active_pane() == Some(role_pane));
        in_role_pane.then_some((row, col))
    }
}

impl Probe {
    /// Why a parked S6/rest phase did not measure what it claims, if it did not: see
    /// [`scenarios::park_progress`]. The phase's start snapshot is compared with its end.
    pub(super) fn park_problem(&mut self, phase: &PhaseSpec) -> Option<String> {
        let entry = self.park_entry.take()?;
        let presented_frames = self.meter.as_ref().map_or(0, |meter| meter.presented_frames);
        let end = self.role_pane(0).map(|pane| {
            let parser = pane.parser.lock();
            crate::record::park_snapshot(parser.grid(), pane.viewport_top_abs, entry.pointed_row)
        });
        let Some(end) = end else {
            // When: role 0's pane is gone, the phase has no end to compare.
            return Some(format!("S6/rest phase {} lost its pane", phase.name));
        };
        let counters_start = self.meter.as_ref().and_then(|meter| meter.counters_start.clone());
        let hover = counters_start.zip(self.counter_totals()).and_then(|(start, totals)| {
            let delta = totals.delta_since(&start);
            let count = |name: &str| match delta.get(name) {
                Some(crate::counters::FieldValue::Count(count)) => Some(*count),
                _ => None,
            };
            Some(scenarios::HoverDeltas {
                detections: count("hover_detections")?,
                cache_hits: count("hover_cache_hits")?,
                probes_submitted: count("path_probes_submitted")?,
            })
        });
        let evidence = entry.evidence(&end, presented_frames, hover);
        scenarios::park_progress(phase.name, &evidence).err()
    }
}

impl Probe {
    /// Why a `status-line` phase broke the strip's idle contract, if it did: see
    /// [`scenarios::status_line_progress`]. Needs counters; a side without the status line counters
    /// (a base older than the status line) reads them as absent and is not checked.
    pub(super) fn status_line_problem(&mut self, phase: &PhaseSpec) -> Option<String> {
        if !self.plan.status_line {
            // When: `status_line` is off for this plan, there is no strip to check.
            return None;
        }
        if phase.name == "print" && !self.plan.prints_linked_history() {
            // When: the plan prints a history that never passes the byte budget (S12), nothing
            // is trimmed, so `print` has no `History: trimmed` to count.
            return None;
        }
        let hold_ms = match phase.end {
            PhaseEnd::Hold(hold_ms) => hold_ms,
            // `print` ends on its sentinel; its checks count appearances, not a cadence.
            // `print` ends on its sentinel and `input` when its typing settles; neither checks a cadence.
            _ if matches!(phase.name, "print" | "input" | "glyphs" | "held" | "program") => 0,
            // When: the phase neither holds nor prints, it has no cadence to check.
            _ => return None,
        };
        let counters_start = self.meter.as_ref().and_then(|meter| meter.counters_start.clone());
        // Every present in the phase, from any path; `accessibility` bounds it by strip redraws.
        let presented =
            counters_start.clone().zip(self.counter_totals()).and_then(|(start, totals)| {
                match totals.delta_since(&start).get("presented") {
                    Some(crate::counters::FieldValue::Count(count)) => Some(*count),
                    _ => None,
                }
            });
        // `program` reads its own counters from the same start below.
        let deltas =
            counters_start.clone().zip(self.counter_totals()).and_then(|(start, totals)| {
                let delta = totals.delta_since(&start);
                let count = |name: &str| match delta.get(name) {
                    Some(crate::counters::FieldValue::Count(count)) => Some(*count),
                    _ => None,
                };
                Some(scenarios::StatusLineDeltas {
                    samples: count("status_line_samples")?,
                    wakes: count("status_line_wakes")?,
                    redraws: count("status_line_redraws")?,
                    sel_counts: count("status_line_sel_counts")?,
                    sel_cells: count("status_line_sel_cells")?,
                    history_shows: count("status_line_history_shows")?,
                    // A side whose App predates the counter shows no `held` to count.
                    held_shows: count("status_line_held_shows").unwrap_or(0),
                    glyph_shows: count("status_line_glyph_shows").unwrap_or(0),
                    // A side whose App predates accessibility builds no tree to count.
                    tree_builds: count("status_line_tree_builds").unwrap_or(0),
                })
            })?;
        if phase.name == "program" {
            // When: the phase is `program`, the program counters decide it; a base whose App
            // predates them has none, and only the strip is checked.
            let program = counters_start.zip(self.counter_totals()).and_then(|(start, totals)| {
                let delta = totals.delta_since(&start);
                let count = |name: &str| match delta.get(name) {
                    Some(crate::counters::FieldValue::Count(count)) => Some(*count),
                    _ => None,
                };
                Some(scenarios::ProgramDeltas {
                    progress: count("program_progress_events")?,
                    notifies: count("program_notify_events")?,
                    toast_redraws: count("program_toast_redraws")?,
                    deliveries: count("taskbar_progress_deliveries")?,
                    presented: count("presented")?,
                })
            });
            let delivered = take_taskbar_deliveries(&mut self.app);
            // The phase's committed steps, each of which may present its echo, output and prompt.
            let steps = match phase.driver {
                scenarios::Driver::Commit { steps, .. } => steps.len() as u64,
                _ => 0,
            };
            return scenarios::program_phase_progress(
                deltas,
                program,
                delivered.as_deref(),
                !cfg!(windows),
                steps,
            )
            .err();
        }
        let occluded = self.occlusion.delivered == Some(true);
        scenarios::status_line_progress(phase.name, hold_ms, occluded, deltas)
            .and_then(|()| {
                if phase.name == "accessibility" {
                    scenarios::accessibility_phase_presents(deltas.redraws, presented)
                } else {
                    // When: any other phase, presents are bounded by its own checks.
                    Ok(())
                }
            })
            .and_then(|()| {
                if cfg!(windows) {
                    // A Windows role's shell cannot stall its reader, so `held` need not appear.
                    Ok(())
                } else {
                    // When: the role shell is POSIX, the stalled paste must show `held` once.
                    scenarios::status_line_posix_progress(phase.name, deltas)
                }
            })
            .err()
    }
}

impl Probe {
    /// Drag endpoints at cell centers inside the grid.
    pub(super) fn drag_geometry(&self) -> Option<((f64, f64), (f64, f64))> {
        let pane = self.active_pane()?;
        let layout = self.app.main_renderer()?.pane_layout(pane)?;
        // Startup never activates the app: its only activation call is `focus_window` when a tab
        // drops into another window, which winit implements with `activateIgnoringOtherApps`. So
        // no button is ever pressed on the tab bar; press, motion and release stay inside the grid,
        // away from the tab bar and the window edges.
        let cell = |row: u16, col: u16| {
            (
                f64::from(layout.origin_x_logical)
                    + (f64::from(col) + 0.5) * f64::from(layout.cell_w_logical),
                f64::from(layout.origin_y_logical)
                    + (f64::from(row) + 0.5) * f64::from(layout.cell_h_logical),
            )
        };
        let (rows, cols) = (layout.rows.max(4), layout.cols.max(4));
        Some((cell(rows / 8 + 1, cols / 10 + 1), cell(rows * 6 / 8, cols * 8 / 10)))
    }
}

impl Probe {
    /// The history rows `role`'s pane kept, and its center for the wheel's pointer.
    pub(super) fn wheel_setup(&self, role: usize) -> Option<(usize, (f64, f64))> {
        let pane_id = *self.role_panes.get(role)?;
        let pane = self.app.main_panes()?.get(&pane_id)?;
        // The print phase saw the sentinel, so the worker is idle and this short lock does not wait.
        let retained = pane.parser.lock().grid().scrollback_len();
        let layout = self.app.main_renderer()?.pane_layout(pane_id)?;
        let center = (
            f64::from(layout.origin_x_logical) + f64::from(layout.w_logical) / 2.0,
            f64::from(layout.origin_y_logical) + f64::from(layout.h_logical) / 2.0,
        );
        Some((retained, center))
    }
}

impl Probe {
    /// When the driver's next input is due; for typing and the wheel, also when they end.
    pub(super) fn driver_due(&self) -> Option<Instant> {
        match &self.driver {
            DriverState::None => None,
            DriverState::Typing(typing) => typing.next_due(),
            DriverState::Sweep(sweep) => Some(sweep.started + sweep.interval * sweep.ticks),
            DriverState::Drag(drag) => {
                (!drag.finished).then(|| drag.started + drag.interval * drag.ticks)
            }
            DriverState::Wheel(wheel) => {
                Some(wheel.started + wheel.interval * wheel.sent.min(wheel.total))
            }
            DriverState::AtlasRetry(retry) => retry.due(),
            DriverState::Park(park) => Some(park.started + park.interval * park.ticks),
            DriverState::Commit(commit) => Some(commit.next_due()),
        }
    }
}

impl Probe {
    /// Inject at most one due input, so a late wake catches up one input per pass.
    pub(super) fn drive(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        if self.driver_due().is_none_or(|due| now < due) {
            return;
        }
        match self.driver {
            DriverState::None => {}
            DriverState::Typing(_) => self.inject_typing(event_loop),
            DriverState::Sweep(_) => self.inject_sweep(event_loop),
            DriverState::Drag(_) => self.inject_drag(event_loop, now),
            DriverState::Wheel(_) => self.inject_wheel(event_loop),
            DriverState::AtlasRetry(_) => self.drive_atlas_retry(event_loop, now),
            DriverState::Park(_) => self.inject_park(event_loop),
            DriverState::Commit(_) => self.inject_commit(event_loop),
        }
    }
}

impl Probe {
    /// Commit the next due step's text into the active pane.
    pub(super) fn inject_commit(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Commit(commit) = &mut self.driver else {
            // When: driver is not Commit, there is no step to send.
            return;
        };
        let steps = commit.steps;
        let Some(step) = steps.get(commit.sent) else {
            // When: every step is sent, the settle runs out with nothing to inject.
            return;
        };
        commit.sent += 1;
        let text = match step.text {
            scenarios::CommitText::Literal(text) => text.to_owned(),
            scenarios::CommitText::Repeat(character, count) => {
                std::iter::repeat_n(character, count).collect()
            }
        };
        self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(text)));
    }
}

impl Probe {
    /// Repeat the parked pointer's `CursorMoved` on the same cell.
    pub(super) fn inject_park(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Park(park) = &mut self.driver else {
            return;
        };
        park.ticks += 1;
        let point = park.point;
        self.pointer(event_loop, point);
    }
}

impl Probe {
    /// Type the next character: the previous sample closes and this character's sample opens.
    pub(super) fn inject_typing(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(character) = self.open_typing_sample(Instant::now) {
            self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(character.to_string())));
        }
    }
}

impl Probe {
    /// Close the previous sample and open the next character's: arm its dispatch timeline, then read the
    /// injection instant from `clock`, then arm its echo watch, only for the variants `split_in_scope`
    /// names (S2/default and S2/flood); returns the character to type, or `None` when typing is done or
    /// the prompt is not yet found. Needs no event loop.
    pub(super) fn open_typing_sample(&mut self, clock: impl FnOnce() -> Instant) -> Option<char> {
        let DriverState::Typing(typing) = &mut self.driver else {
            return None;
        };
        let origin = typing.origin?;
        if typing.typed >= typing.chars {
            return None;
        }
        let target = echo_target(origin, typing.cols, typing.typed);
        let (pane, identity) = (typing.pane, typing.identity);
        typing.typed += 1;
        let last = typing.typed == typing.chars;
        // A sample with no candidate by the next injection is unattributed.
        self.close_sample(UnattributedReason::NoCandidate);
        // The timeline is armed before the injection instant is read, so the arm is outside the sample.
        let timeline = SampleTimeline::arm(
            &mut self.app,
            pane,
            self.running_outer,
            &mut self.timeline_storage,
        );
        let injected = clock();
        if let (true, DriverState::Typing(typing)) = (last, &mut self.driver) {
            // When: this is the last character, typing is done once it is injected.
            typing.done_at = Some(injected);
        }
        // Every parser guard is released here, and the character is not yet injected.
        let arm = if split_in_scope(self.plan.scenario, self.plan.variant) {
            echo_api::arm(&mut self.app, pane, &target, identity)
        } else {
            // When: the plan is outside `split_in_scope`, its samples are never armed and read unsupported.
            ArmState::OutOfScope
        };
        let inject_unix_s = unix_now();
        self.open_sample =
            Some(OpenSample { pane, target, injected, inject_unix_s, arm, identity, timeline });
        Some(target.character)
    }
}

impl Probe {
    pub(super) fn inject_sweep(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Sweep(sweep) = &mut self.driver else {
            return;
        };
        let position = sweep.position();
        sweep.ticks += 1;
        self.pointer(event_loop, position);
    }
}

impl Probe {
    /// Inject the drag's next step at `now`, if it has one.
    pub(super) fn inject_drag(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        let DriverState::Drag(drag) = &mut self.driver else {
            return;
        };
        let Some(motion) = drag.next_motion(now) else {
            // When: next_motion has nothing, the drag finished or went quiet with no press held.
            return;
        };
        match motion {
            DragMotion::Press(point) => {
                self.pointer(event_loop, point);
                self.button(event_loop, ElementState::Pressed);
            }
            DragMotion::Move(point) => self.pointer(event_loop, point),
            DragMotion::Release => self.button(event_loop, ElementState::Released),
        }
    }
}

impl Probe {
    /// One wheel tick: up through the retained history, then back down.
    pub(super) fn inject_wheel(&mut self, event_loop: &ActiveEventLoop) {
        let DriverState::Wheel(wheel) = &mut self.driver else {
            return;
        };
        if wheel.sent >= wheel.total {
            return;
        }
        // A positive vertical line delta scrolls up, into history.
        let lines = if wheel.sent < wheel.total / 2 { 1.0 } else { -1.0 };
        wheel.sent += 1;
        let delta = MouseScrollDelta::LineDelta(0.0, lines);
        let event = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta,
            phase: TouchPhase::Moved,
        };
        self.window_input(event_loop, event);
    }
}

impl Probe {
    /// Dispatch a synthetic event to the measurement window.
    pub(super) fn window_input(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(window_id) = self.main_id else {
            return;
        };
        let synthetic = InvocationKind::WindowEvent { redraw: false };
        let invocation = Invocation::of(synthetic, Some(window_id), true);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, active| {
            app.window_event(active, window_id, event)
        });
    }
}

impl Probe {
    pub(super) fn pointer(&mut self, event_loop: &ActiveEventLoop, point: (f64, f64)) {
        let position = PhysicalPosition::new(point.0, point.1);
        self.window_input(
            event_loop,
            WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position },
        );
    }
}

impl Probe {
    pub(super) fn button(&mut self, event_loop: &ActiveEventLoop, state: ElementState) {
        let event = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        };
        self.window_input(event_loop, event);
    }
}

#[cfg(test)]
#[path = "probe_input_tests.rs"]
mod probe_input_tests;
