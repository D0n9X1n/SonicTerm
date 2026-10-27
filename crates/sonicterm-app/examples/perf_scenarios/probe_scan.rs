//! Bounded role-grid scanning and workload delivery verification.

use super::*;

impl Probe {
    /// A nonblocking look at the open sample's echo cell.
    pub(super) fn echo_snapshot(&self) -> EchoSnapshot {
        let Some(sample) = self.open_sample.as_ref() else {
            return EchoSnapshot::Busy;
        };
        let Some(pane) = self.app.main_panes().and_then(|panes| panes.get(&sample.pane)) else {
            return EchoSnapshot::Busy;
        };
        match pane.parser.try_lock() {
            Some(parser) => snapshot_echo(parser.grid(), &sample.target),
            None => EchoSnapshot::Busy,
        }
    }
}

impl Probe {
    /// Fully redraw the measurement window once, through the App's own output path, after the
    /// scan first sees S11's image registered. The renderer skips a frame whose identity is
    /// unchanged, and a skip presents nothing, so the retained identity is cleared first: the
    /// frame then presents whenever the window can present at all.
    pub(super) fn request_image_redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window_id) = self.main_id else {
            return;
        };
        if let Some(renderer) = self.app.main_renderer_mut() {
            renderer.invalidate_retained_frame();
        }
        let invocation = Invocation::of(InvocationKind::UserEvent, Some(window_id), true);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, active| {
            app.user_event(active, UserEvent::RequestRedraw(window_id))
        });
    }
}

impl Probe {
    /// Write `progress.json` with every measurement completed so far; called only between
    /// phases. A failed write is logged and leaves the measurements untouched.
    pub(super) fn record_progress(&self) {
        let path = self.scratch.join("progress.json");
        let hash = self.request.harness_hash.as_deref();
        let written = write_atomic_with(&path, |file| {
            let mut writer = std::io::BufWriter::new(file);
            write_progress(&mut writer, hash, self.measurements())?;
            std::io::Write::flush(&mut writer)
        });
        if let Err(error) = written {
            tracing::warn!(target: LOG_TARGET, %error, "cannot write progress.json");
        }
    }
}

impl Probe {
    /// The main renderer's presented frames so far.
    pub(super) fn frame_count(&self) -> u64 {
        #[cfg(test)]
        if let Some((frames, _)) = self.test_readings {
            // When: a test stands in for the renderer, its count is the reading.
            return frames;
        }
        self.app.main_renderer().map_or(0, GpuRenderer::successful_frame_count)
    }
}

impl Probe {
    /// Whether a sentinel, an image or the typing prompt is still awaited.
    pub(super) fn scan_wanted(&self) -> bool {
        self.sentinel_roles.iter().any(|role| self.sentinel_seen[*role].is_none())
            || (self.image.role.is_some() && !self.image.present.seen())
            || matches!(&self.driver, DriverState::Typing(typing) if typing.origin.is_none())
    }
}

impl Probe {
    /// Scan in dispatches the App already receives, at most every `SCAN_INTERVAL`. A throttled
    /// scan in a dispatch from outside the harness leaves one trailing check `SCAN_INTERVAL` after
    /// the last scan; the harness's own wakes, that check's included, never arm another.
    pub(super) fn maybe_scan(&mut self, now: Instant, trigger: ScanTrigger) {
        let wanted = self.scan_wanted();
        if self.scan.observe(now, wanted, trigger) {
            self.run_scan(now);
        }
    }
}

impl Probe {
    /// Look, without blocking, for each awaited sentinel, the awaited image and the typing prompt.
    pub(super) fn run_scan(&mut self, now: Instant) {
        for index in 0..self.sentinel_roles.len() {
            let role = self.sentinel_roles[index];
            if self.sentinel_seen[role].is_some() {
                continue;
            }
            let sentinel = &self.sentinels[role];
            let workload = self.plan.roles[role];
            let found = self
                .role_grid(role, |grid| {
                    sentinel_row(grid, sentinel, workload, scenarios::BUILD_HOST)
                })
                .flatten();
            if let Some(row) = found {
                self.sentinel_seen[role] = Some(now);
                self.sentinel_rows[role] = Some(row);
            }
        }
        if let (Some(role), false) = (self.image.role, self.image.present.seen()) {
            let registered = self
                .role_pane(role)
                .and_then(|pane| pane.inline_images.try_lock().map(|images| !images.is_empty()));
            if registered == Some(true) {
                self.image.present.saw_registration(now, self.frames);
            }
        }
        let prompt_role = match &self.driver {
            DriverState::Typing(typing) if typing.origin.is_none() => Some(typing.role),
            _ => None,
        };
        if let Some(role) = prompt_role {
            let found = self
                .role_grid(role, |grid| {
                    prompt_origin(grid, workload::PROMPT)
                        .map(|origin| (origin, grid.cols, prompt_identity(grid)))
                })
                .flatten();
            if let (Some((origin, cols, identity)), DriverState::Typing(typing)) =
                (found, &mut self.driver)
            {
                typing.origin = Some(origin);
                typing.identity = identity;
                typing.cols = cols;
                typing.first_at = Some(now);
            }
        }
    }
}

impl Probe {
    /// Read `role`'s grid without blocking; `None` when the pane is gone or its parser is busy.
    pub(super) fn role_grid<Output>(
        &self,
        role: usize,
        read: impl FnOnce(&Grid) -> Output,
    ) -> Option<Output> {
        let pane = self.role_pane(role)?;
        let parser = pane.parser.try_lock()?;
        Some(read(parser.grid()))
    }
}

impl Probe {
    /// Role `role`'s cursor as (absolute row, column), read under its parser lock.
    pub(super) fn role_cursor(&self, role: usize) -> Option<(u64, u16)> {
        let pane = self.role_pane(role)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cursor_absolute_row(), grid.cursor.col))
    }
}

impl Probe {
    /// Note the roles' first output after GO for the phase's diagnostics: the first dispatch at which a
    /// role's cursor has moved from where GO found it.
    pub(super) fn note_first_output(&mut self) {
        let awaiting = self.meter.as_ref().is_some_and(|meter| meter.present.awaits_output());
        if !awaiting {
            // When: no phase awaits its first output, there is nothing to time.
            return;
        }
        let moved = self
            .go_cursors
            .iter()
            .enumerate()
            .any(|(role, at_go)| at_go.is_some() && self.role_cursor(role) != *at_go);
        if !moved {
            // When: no role's cursor moved since GO, the output so far was read before GO.
            return;
        }
        if let Some(meter) = self.meter.as_mut() {
            let offset_ms = ms_between(meter.started, Instant::now());
            meter.present.note_output(offset_ms);
        }
    }
}

impl Probe {
    /// Release every role at GO through `release`, then stamp GO. When the phase awaits its roles' first
    /// output, each role's cursor is read first, before any role is released, as the baseline that output
    /// moves: a released role can print, and its worker publish, before this method returns.
    pub(super) fn release_roles(
        &mut self,
        awaits_output: bool,
        mut release: impl FnMut(&mut Self, usize) -> std::io::Result<()>,
    ) -> Result<(), String> {
        self.go_cursors = if awaits_output {
            (0..self.plan.roles.len()).map(|role| self.role_cursor(role)).collect()
        } else {
            Vec::new()
        };
        for role in 0..self.plan.roles.len() {
            release(self, role)
                .map_err(|error| format!("cannot write GO for role {role}: {error}"))?;
        }
        self.go_at = Some(Instant::now());
        Ok(())
    }
}

impl Probe {
    /// Release role `role`: on Unix, write its GO FIFO, which the role script blocks reading, and hold it
    /// open; elsewhere, connect to the port its program role waits on, then write its GO marker.
    pub(super) fn release_role(&mut self, role: usize) -> std::io::Result<()> {
        let marker = self.scratch.join(format!("go/{role}"));
        #[cfg(unix)]
        {
            // Read-write, so the open never waits for a reader and the line stays buffered for one.
            let mut fifo = std::fs::OpenOptions::new().read(true).write(true).open(&marker)?;
            std::io::Write::write_all(&mut fifo, b"go\n")?;
            self.go_signals.push(fifo);
        }
        #[cfg(not(unix))]
        {
            crate::workload::connect_go(&self.scratch, role)?;
            std::fs::write(&marker, b"")?;
        }
        Ok(())
    }
}

impl Probe {
    pub(super) fn role_pane(&self, role: usize) -> Option<&PaneState> {
        let pane_id = *self.role_panes.get(role)?;
        self.app.main_panes()?.get(&pane_id)
    }
}

impl Probe {
    /// The active tab's active pane in the measurement window.
    pub(super) fn active_pane(&self) -> Option<u64> {
        let main = self.app.main()?;
        main.tab_states.get(main.tabs.active_index()).map(|tab| tab.active_pane)
    }
}

impl Probe {
    /// Every measurement completed so far, as both `progress.json` and `result.json` record it;
    /// latency samples count only in a scenario that types.
    pub(super) fn measurements(&self) -> Measurements<'_> {
        Measurements {
            frame_counters: self.counters_mode,
            phases: &self.phases,
            latency: self.has_typing().then_some(self.samples.as_slice()),
            throughput: self.throughput,
            uncover_ms: self.uncover_ms,
            scrollback_rows_retained: self.scrollback_rows_retained,
            checkpoints: &self.checkpoints,
        }
    }
}

impl Probe {
    pub(super) fn has_typing(&self) -> bool {
        self.plan.steps.iter().any(
            |step| matches!(step, Step::Phase(phase) if matches!(phase.driver, Driver::Typing { .. })),
        )
    }
}

impl Probe {
    /// End the open phase, reading its counter totals at its end, before any progress write.
    pub(super) fn finish_meter(&mut self) {
        let Some(meter) = self.meter.take() else {
            // When: `meter` is None, no phase is open and nothing ends.
            return;
        };
        let completion = self.completion_of(&meter);
        let started = meter.started;
        let counters_end = self.counter_totals();
        // The window's end edge: the instant the counter end was observed, latched before any transport.
        let counters_end_at = self.guard_now();
        let mut record = meter.finish(counters_end, completion);
        record.s10_attribution = self.attribution.take();
        self.phases.push(record);
        self.guard_window = Some((started, counters_end_at));
    }
}

impl Probe {
    /// Whether `pane`, just recorded as a role pane, had already exited; if so the run is invalidated.
    pub(super) fn recorded_exit_invalidates(
        &mut self,
        event_loop: &ActiveEventLoop,
        pane: u64,
    ) -> bool {
        let exit = self.exited_panes.iter().find(|(exited, _)| *exited == pane).copied();
        let reason = exit.and_then(|(pane_id, was_clean)| {
            let finished = self.outcome.is_some();
            waits::role_exit_reason(pane_id, was_clean, &self.role_panes, true, finished)
        });
        let Some(reason) = reason else {
            // When: the pane has not exited, or the run already has its outcome, nothing changes.
            return false;
        };
        self.invalidate(event_loop, reason);
        true
    }
}

impl Probe {
    /// The main renderer's retained image-atlas bytes; `None` before a renderer exists.
    pub(super) fn image_atlas_bytes(&self) -> Option<usize> {
        Some(self.app.main_renderer()?.retained_amounts().image_atlas.bytes)
    }
}

impl Probe {
    /// Whether the image atlas grew since startup ended; `None` when either reading is missing.
    pub(super) fn image_atlas_grew(&self) -> Option<bool> {
        let start = self.image_atlas_start?;
        Some(self.image_atlas_bytes()? > start)
    }
}

impl Probe {
    /// When S11's image must have registered on Windows: `IMAGE_REGISTER_WAIT` after its phase began.
    pub(super) fn image_register_deadline(&self) -> Instant {
        self.image.started.map_or(self.run_deadline, |started| started + IMAGE_REGISTER_WAIT)
    }
}

impl Probe {
    /// What a print phase's roles failed to deliver, on Windows: S3's rows and S9's wide tokens.
    pub(super) fn delivery_problem(&self) -> Option<String> {
        let checked: Vec<(usize, Workload)> = self
            .sentinel_roles
            .iter()
            .filter_map(|role| self.plan.roles.get(*role).map(|workload| (*role, *workload)))
            .filter(|(_, workload)| {
                matches!(
                    workload,
                    Workload::Flood { .. } | Workload::PrintThenShell(Fixture::EmojiCjk)
                )
            })
            .collect();
        if checked.is_empty() {
            // When: no checked workload ran, the fixtures need not be generated.
            return None;
        }
        let files = workload::fixtures(&self.plan);
        checked.into_iter().find_map(|(role, workload)| match workload {
            Workload::Flood { lines, .. } => self.flood_problem(role, lines, &files),
            _ => self.wide_token_problem(role, &files),
        })
    }
}

impl Probe {
    /// S3: whether the rows between READY and the sentinel are exactly the planned lines, and the
    /// retained rows above the sentinel are the end of `bulk.txt`.
    pub(super) fn flood_problem(
        &self,
        role: usize,
        lines: u32,
        files: &[workload::FixtureFile],
    ) -> Option<String> {
        let bulk = files.iter().find(|file| file.relative_path == "bulk.txt");
        let Some(workload::FixtureBody::Repeated { block, count }) = bulk.map(|file| &file.body)
        else {
            return Some(format!("role {role}: S3 has no repeated bulk.txt fixture to count"));
        };
        let (Some(ready), Some(sentinel)) = (self.ready_rows[role], self.sentinel_rows[role])
        else {
            return Some(format!("role {role}'s READY or sentinel row was never located, so its rows cannot be counted"));
        };
        // bulk.txt repeats one block, so its lines, and its end, are that block's.
        let text = String::from_utf8_lossy(block);
        let block_lines: Vec<&str> = text.lines().collect();
        let tail: &[&str] = if *count == 0 {
            // When: no bulk block was printed, the rows above the sentinel are `y` lines, not bulk.txt's.
            &[]
        } else {
            &block_lines[block_lines.len().saturating_sub(BULK_TAIL_LINES)..]
        };
        // The count uses the pane's own width. Every line is ASCII, so its width is its length, and
        // a `y` line is one cell wide, so it takes one row at any width.
        let checked = self.read_role_grid(role, |grid| {
            let block_rows = planned_rows(block_lines.iter().map(|line| line.len()), grid.cols);
            let planned = u64::from(lines) + block_rows * *count as u64;
            row_count_mismatch(ready, sentinel, planned)
                .or_else(|| bulk_tail_mismatch(grid, sentinel, tail))
        });
        match checked {
            None => Some(format!("role {role}'s grid could not be read to count its rows")),
            Some(reason) => reason.map(|reason| format!("role {role}: {reason}")),
        }
    }
}

impl Probe {
    /// S9: whether the retained rows hold every wide token the emoji-cjk fixture printed.
    pub(super) fn wide_token_problem(
        &self,
        role: usize,
        files: &[workload::FixtureFile],
    ) -> Option<String> {
        // emoji-cjk.txt is the file Fixture::EmojiCjk is written to.
        let file = files.iter().find(|file| file.relative_path == "emoji-cjk.txt");
        let Some(workload::FixtureBody::Bytes(bytes)) = file.map(|file| &file.body) else {
            return Some(format!("role {role}: S9 has no emoji-cjk.txt fixture to check"));
        };
        let text = String::from_utf8_lossy(bytes);
        let tokens = wide_tokens(&text);
        let missing = self.read_role_grid(role, |grid| {
            let retained = retained_text(grid);
            missing_wide_tokens(&retained, &tokens)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        match missing {
            None => Some(format!("role {role}'s grid could not be read to check its wide tokens")),
            Some(missing) if missing.is_empty() => None,
            Some(missing) => Some(format!(
                "role {role}'s grid lacks {} wide token(s) its fixture printed: {}",
                missing.len(),
                missing.join(" ")
            )),
        }
    }
}

impl Probe {
    /// Read `role`'s grid outside every measured phase, retrying a busy parser briefly; `None` when
    /// the pane is gone or its parser stayed busy.
    pub(super) fn read_role_grid<Output>(
        &self,
        role: usize,
        read: impl Fn(&Grid) -> Output,
    ) -> Option<Output> {
        for _ in 0..GRID_READ_ATTEMPTS {
            if let Some(output) = self.role_grid(role, &read) {
                return Some(output);
            }
            std::thread::sleep(GRID_READ_PAUSE);
        }
        None
    }
}
