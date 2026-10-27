//! Bounded breadcrumb history, event batching, and atomic snapshot persistence.

use super::*;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
struct CapturedEvent {
    timestamp_unix_ms: i64,
    event: BreadcrumbEvent,
}

#[derive(Debug)]
struct WorkerState {
    pinned: [Option<CapturedEvent>; 6],
    lifecycle: VecDeque<CapturedEvent>,
    history: VecDeque<(i64, ProcessPressure)>,
    lifecycle_capacity: usize,
    history_capacity: usize,
}

impl WorkerState {
    /// Allocate bounded lifecycle and pressure rings alongside fixed pinned slots.
    fn new(lifecycle_capacity: usize, history_capacity: usize) -> Self {
        Self {
            pinned: [None; 6],
            lifecycle: VecDeque::with_capacity(lifecycle_capacity),
            history: VecDeque::with_capacity(history_capacity),
            lifecycle_capacity,
            history_capacity,
        }
    }

    /// Replace keyed state in its stable slot while appending lifecycle transitions.
    fn capture(&mut self, event: BreadcrumbEvent) {
        let captured =
            CapturedEvent { timestamp_unix_ms: chrono::Utc::now().timestamp_millis(), event };
        match event.key() {
            Some(key) => self.pinned[key.index()] = Some(captured),
            None => self.capture_lifecycle(captured),
        }
    }

    /// Append one lifecycle transition, evicting the oldest at capacity.
    fn capture_lifecycle(&mut self, captured: CapturedEvent) {
        if self.lifecycle.len() >= self.lifecycle_capacity {
            // When: lifecycle.len reaches lifecycle_capacity, discard the oldest
            // transition before appending the accepted captured transition.
            let _ = self.lifecycle.pop_front();
        }
        self.lifecycle.push_back(captured);
    }

    /// Timestamp one pressure sample and keep only the configured newest window.
    fn capture_pressure(&mut self, pressure: ProcessPressure) {
        while self.history.len() >= self.history_capacity {
            let _ = self.history.pop_front();
        }
        self.history.push_back((chrono::Utc::now().timestamp_millis(), pressure));
    }

    /// Render mandatory state first, then the newest pressure suffix that fits.
    fn render_for_limit(&self, max_bytes: u64) -> Vec<String> {
        let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let mut lines = Vec::new();
        let mut used = 0usize;
        for key in EventKey::ORDER {
            if let Some(captured) = self.pinned[key.index()] {
                push_line(&mut lines, &mut used, captured.event.render(captured.timestamp_unix_ms));
            }
        }
        for captured in &self.lifecycle {
            push_line(&mut lines, &mut used, captured.event.render(captured.timestamp_unix_ms));
        }

        let mut selected_history = VecDeque::new();
        for (timestamp, pressure) in self.history.iter().rev() {
            let line = render_pressure(*timestamp, *pressure);
            let bytes = line.len().saturating_add(1);
            if bytes > max_bytes.saturating_sub(used) {
                // When: bytes exceeds max_bytes - used, this next-oldest history
                // line and every older line are omitted from the byte budget.
                break;
            }
            used = used.saturating_add(bytes);
            selected_history.push_front(line);
        }
        lines.extend(selected_history);
        lines
    }
}

/// Append one mandatory line and account for its newline in the byte budget.
fn push_line(lines: &mut Vec<String>, used: &mut usize, line: String) {
    *used = used.saturating_add(line.len().saturating_add(1));
    lines.push(line);
}

/// Run immediate and deadline pressure samples beside bounded event batching.
///
/// Sampling deadlines take priority over caller traffic; cancellation drains at
/// most one bounded queue snapshot before the final atomic persistence.
pub(super) fn run_worker(
    receiver: mpsc::Receiver<WorkerMessage>,
    path: &Path,
    limits: BreadcrumbLimits,
    sampler: Arc<dyn PressureSampler>,
    cancellation: &SamplerCancellation,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "breadcrumb path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;

    let mut state = WorkerState::new(limits.ring_capacity, limits.history_capacity);
    if let Some(pressure) = sampler.sample(cancellation) {
        state.capture_pressure(pressure);
    }
    persist_state(path, &state, limits.max_file_bytes)?;
    let mut next_sample_at = Instant::now() + limits.pressure_interval;

    loop {
        if cancellation.is_cancelled() {
            // When: cancellation is set, drain at most the bounded queue snapshot,
            // persist it, and exit even if recorder clones remain alive.
            for message in
                std::iter::from_fn(|| receiver.try_recv().ok()).take(limits.queue_capacity)
            {
                let _ = process_message(message, &mut state);
            }
            persist_state(path, &state, limits.max_file_bytes)?;
            break;
        }
        let now = Instant::now();
        if now >= next_sample_at {
            // When: now reaches next_sample_at, sample before receiving another
            // caller event so a continuously populated queue cannot starve it.
            if let Some(pressure) = sampler.sample(cancellation) {
                state.capture_pressure(pressure);
                persist_state(path, &state, limits.max_file_bytes)?;
            }
            next_sample_at = Instant::now() + limits.pressure_interval;
            continue;
        }

        let message = receive_next_message(
            &receiver,
            next_sample_at.saturating_duration_since(Instant::now()),
        );
        if message == WorkerMessage::Deadline {
            // When: message is Deadline, return to the priority check so the
            // sampler runs before any subsequently queued caller event.
            continue;
        }
        let mut shutting_down = process_message(message, &mut state);
        let batch_started = Instant::now();
        while !shutting_down
            && !cancellation.is_cancelled()
            && batch_started.elapsed() < MAX_BATCH_TIME
            && Instant::now() < next_sample_at
        {
            let until_deadline = next_sample_at.saturating_duration_since(Instant::now());
            let wait = COALESCE_IDLE.min(until_deadline);
            match receiver.recv_timeout(wait) {
                Ok(message) => shutting_down = process_message(message, &mut state),
                Err(error) => {
                    // When: recv_timeout returns an error, Timeout closes this
                    // batch and Disconnected also requests worker shutdown.
                    if error == mpsc::RecvTimeoutError::Disconnected {
                        shutting_down = true;
                    }
                    break;
                }
            }
        }
        persist_state(path, &state, limits.max_file_bytes)?;
        if shutting_down {
            // When: shutting_down is set, the final accepted state is persisted
            // before the worker exits.
            break;
        }
    }
    Ok(())
}

/// Convert channel timeout and disconnect states into explicit worker messages.
fn receive_next_message(
    receiver: &mpsc::Receiver<WorkerMessage>,
    timeout: Duration,
) -> WorkerMessage {
    match receiver.recv_timeout(timeout) {
        Ok(message) => message,
        Err(mpsc::RecvTimeoutError::Timeout) => WorkerMessage::Deadline,
        Err(mpsc::RecvTimeoutError::Disconnected) => WorkerMessage::Shutdown,
    }
}

/// Apply an event to worker state and report whether shutdown was requested.
fn process_message(message: WorkerMessage, state: &mut WorkerState) -> bool {
    match message {
        WorkerMessage::Event(event) => {
            state.capture(event);
            false
        }
        WorkerMessage::Deadline => false,
        WorkerMessage::Shutdown => true,
    }
}

/// Persist one bounded snapshot through the platform's atomic replacement seam.
fn persist_state(path: &Path, state: &WorkerState, max_bytes: u64) -> io::Result<()> {
    persist_state_with_replace(path, state, max_bytes, crate::path::replace_file)
}

/// Write a complete temporary snapshot and remove it if replacement fails.
fn persist_state_with_replace(
    path: &Path,
    state: &WorkerState,
    max_bytes: u64,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let lines = state.render_for_limit(max_bytes);
    let used = lines.iter().map(|line| line.len().saturating_add(1)).sum();
    let mut contents = String::with_capacity(used);
    for line in lines {
        contents.push_str(&line);
        contents.push('\n');
    }

    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match replace(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            // When: replace returns error, remove the partial candidate and
            // retain the previous complete destination.
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "breadcrumbs_worker_tests.rs"]
mod breadcrumbs_worker_tests;
