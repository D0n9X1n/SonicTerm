//! Checkpoint readiness, memory readings and frame-bound transitions.

use super::*;

impl PendingCheckpoint {
    /// Where the footprint wait stands: absent when unmanaged.
    pub(super) fn footprint_status(&self) -> FootprintStatus {
        match (&self.footprint, self.footprint_answered) {
            (None, _) => FootprintStatus::Absent,
            (Some(_), false) => FootprintStatus::Pending,
            (Some(_), true) => FootprintStatus::Answered,
        }
    }
}

impl PendingCheckpoint {
    /// The probe's next wake for this checkpoint; `next_deadline` uses it while one is pending.
    pub(super) fn wake(&self, now: Instant) -> Option<Instant> {
        waits::checkpoint_wake(self.footprint_status(), self.sampling.as_ref(), now, POLL_INTERVAL)
    }
}

/// Whether the plan moves past a checkpoint step after `outcome`: only once it advanced. The step
/// loop starts the next phase only when this is true.
pub(super) fn plan_moves_on(outcome: &CheckpointOutcome) -> bool {
    matches!(outcome, CheckpointOutcome::Advance)
}

/// The probe's next wake while it runs its steps: the pending checkpoint's own wake while one is
/// pending, else the soonest of `phase_wakes`, else `run_deadline`; never later than `run_deadline`.
pub(super) fn steps_wake(
    pending: Option<&PendingCheckpoint>,
    now: Instant,
    phase_wakes: impl IntoIterator<Item = Option<Instant>>,
    run_deadline: Instant,
) -> Instant {
    let wake = match pending {
        Some(pending) => pending.wake(now).unwrap_or(now + POLL_INTERVAL),
        None => phase_wakes.into_iter().flatten().min().unwrap_or(run_deadline),
    };
    wake.min(run_deadline)
}

/// One turn at a checkpoint step. The first turn writes the record and the request once, then
/// every turn polls the footprint and takes a memory sample when one is due, recording each
/// attempt on the record. `sample` takes attempt `attempt` at checkpoint `index` (`label`) and
/// returns whether it measured every pane. The step advances only when `checkpoint_ready` holds.
pub(super) fn checkpoint_turn(
    pending: &mut Option<PendingCheckpoint>,
    records: &mut Vec<CheckpointRecord>,
    site: &CheckpointSite<'_>,
    step: &CheckpointStep,
    mut sample: impl FnMut(usize, &str, u32) -> bool,
) -> CheckpointOutcome {
    let mut current = match pending.take() {
        Some(current) if current.index == step.ordinal && current.label == step.label => current,
        Some(current) => {
            // When: the pending checkpoint is not this step's, two checkpoints would mix; stop.
            return CheckpointOutcome::Invalid(format!(
                "checkpoint identity changed: {}-{} was pending when the plan reached {}-{}",
                current.index, current.label, step.ordinal, step.label
            ));
        }
        None => open_checkpoint(records, site, step, (site.clock)()),
    };
    if !current.footprint_answered {
        if let Some(wait) = &current.footprint {
            let done = (site.exists)(&wait.done);
            // The footprint file is looked at only once `.done` says it is complete.
            let footprint = done && (site.exists)(&wait.json);
            match waits::checkpoint_progress(done, footprint, (site.clock)(), wait.deadline) {
                CheckpointProgress::Waiting => {}
                CheckpointProgress::Answered { footprint } => {
                    current.footprint_answered = true;
                    if footprint {
                        if let Some(record) =
                            records.iter_mut().rev().find(|record| record.index == current.index)
                        {
                            record.footprint_file = Some(wait.relative_json.clone());
                        }
                    }
                }
                CheckpointProgress::Expired => {
                    return CheckpointOutcome::Invalid(waits::checkpoint_expired_reason(
                        &wait.stem,
                        site.footprint_wait,
                    ));
                }
            }
        }
    }
    let (index, label) = (current.index, current.label);
    if site.sampling {
        // The clock is read here, after the request write and the footprint checks, so the decision
        // to sample is made at the time it is taken. The sampling state is created from this same
        // read on the first turn, so its window opens at its first attempt, never before file I/O.
        let now = (site.clock)();
        let sampling = current.sampling.get_or_insert_with(|| CheckpointSampling::new(index, now));
        waits::sampling_turn(sampling, now, |attempt| sample(index, label, attempt));
        if let Some(record) = records.iter_mut().rev().find(|record| record.index == index) {
            record.sampling = Some(sampling.state.as_str());
            record.attempts = Some(sampling.attempts);
            record.last_attempt_complete = Some(sampling.last_attempt_complete);
        }
    }
    if waits::checkpoint_ready(current.footprint_status(), current.sampling.as_ref()) {
        CheckpointOutcome::Advance
    } else {
        // When: the footprint or the sampling is still pending, keep the checkpoint for the next wake.
        *pending = Some(current);
        CheckpointOutcome::Wait
    }
}

/// Append each `(checkpoint index, reading)` to the latest record of that checkpoint, in the order the
/// attempts were taken.
pub(super) fn attach_atlas_readings(
    records: &mut [CheckpointRecord],
    readings: Vec<(usize, AtlasReading)>,
) {
    for (index, reading) in readings {
        if let Some(record) = records.iter_mut().rev().find(|record| record.index == index) {
            record.atlas_readings.push(reading);
        }
    }
}

/// A checkpoint's first turn: write its request, push its record, and start its footprint wait
/// (managed runs) from `now`. Its sampling (builds with the hook) is created later in the turn, from
/// a clock read taken after this I/O, immediately before its first attempt.
pub(super) fn open_checkpoint(
    records: &mut Vec<CheckpointRecord>,
    site: &CheckpointSite<'_>,
    step: &CheckpointStep,
    now: Instant,
) -> PendingCheckpoint {
    let index = step.ordinal;
    let label = step.label;
    let stem = format!("{index}-{label}");
    let request =
        serde_json::json!({"label": label, "unix_s": step.unix_s, "pid": std::process::id()});
    let path = site.scratch.join(format!("checkpoints/{stem}.request"));
    if let Err(error) = write_atomic(&path, request.to_string().as_bytes()) {
        tracing::warn!(target: LOG_TARGET, %error, label, "cannot write the checkpoint request");
    }
    tracing::info!(target: LOG_TARGET, index, label, "perf_scenarios checkpoint");
    records.push(CheckpointRecord {
        index,
        label,
        unix_s: step.unix_s,
        footprint_file: None,
        fresh_after_unix_s: step.fresh_after_unix_s,
        frame_texture_bytes: step.frame_texture_bytes,
        completeness: step.completeness.clone(),
        ..CheckpointRecord::default()
    });
    let footprint = site.managed.then(|| CheckpointWait {
        done: site.scratch.join(format!("checkpoints/{stem}.done")),
        json: site.scratch.join(format!("checkpoints/{stem}.json")),
        relative_json: format!("checkpoints/{stem}.json"),
        deadline: now + site.footprint_wait,
        stem,
    });
    PendingCheckpoint { index, label, footprint, footprint_answered: false, sampling: None }
}

impl Probe {
    /// Take one turn at the checkpoint step `step_index` (`label`): the first turn writes the request
    /// and the record; a managed run then waits up to `CHECKPOINT_WAIT`, without blocking the event
    /// loop, for the comparison script's `.done`, and a build with the checkpoint-memory hook samples
    /// memory until a sample is complete or its window runs out. With no `.done` by then the run
    /// ends invalid and the next phase never starts. Returns whether the plan may move on.
    pub(super) fn advance_checkpoint(
        &mut self,
        event_loop: &ActiveEventLoop,
        step_index: usize,
        label: &'static str,
    ) -> bool {
        let ordinal = self.plan.steps[..step_index]
            .iter()
            .filter(|step| matches!(step, Step::Checkpoint(_)))
            .count();
        // The record's optional fields are read once, when the checkpoint is first reached.
        let (fresh_after_unix_s, frame_texture_bytes, completeness) = if self
            .checkpoint_pending
            .is_none()
        {
            let fresh_after_unix_s =
                self.plan.fresh_after.filter(|rule| rule.checkpoint == label).and_then(|rule| {
                    self.phase_ends.iter().rev().find(|(name, ..)| *name == rule.anchor).map(
                        |(_, _, ended_unix_s)| {
                            waits::fresh_after_unix_s(
                                *ended_unix_s,
                                Duration::from_millis(rule.delay_ms),
                            )
                        },
                    )
                });
            let completeness = takes_completeness(self.plan.scenario, label)
                .then(|| self.app.main_renderer().map(renderer_completeness))
                .flatten();
            (
                fresh_after_unix_s,
                checkpoint_frame_texture_bytes(label, self.frame_texture_extent()),
                completeness,
            )
        } else {
            // When: the checkpoint is pending, its record already holds these fields.
            (None, None, None)
        };
        let step = CheckpointStep {
            ordinal,
            label,
            unix_s: unix_now(),
            fresh_after_unix_s,
            frame_texture_bytes,
            completeness,
        };
        let clock = Instant::now;
        let exists = |path: &Path| path.exists();
        let site = CheckpointSite {
            scratch: &self.scratch,
            managed: self.request.managed,
            sampling: CHECKPOINT_MEMORY,
            footprint_wait: CHECKPOINT_WAIT,
            clock: &clock,
            exists: &exists,
        };
        let app = &mut self.app;
        // Each attempt's atlas reading is taken right after its memory line, with no frame between.
        let mut readings = Vec::new();
        let outcome = checkpoint_turn(
            &mut self.checkpoint_pending,
            &mut self.checkpoints,
            &site,
            &step,
            |index, label, attempt| {
                let complete = sample_checkpoint_memory(app, index, label, attempt);
                readings.push((index, crate::counters::atlas_reading(app, attempt)));
                complete
            },
        );
        attach_atlas_readings(&mut self.checkpoints, readings);
        let moves_on = plan_moves_on(&outcome);
        if let CheckpointOutcome::Invalid(reason) = outcome {
            self.invalidate(event_loop, reason);
        }
        moves_on
    }
}

#[cfg(test)]
#[path = "probe_checkpoint_tests.rs"]
mod probe_checkpoint_tests;
