use super::*;

impl PtyWriterProgress {
    pub(super) fn new() -> Self {
        Self {
            closing: std::sync::atomic::AtomicBool::new(false),
            epoch: Instant::now(),
            operation: AtomicU64::new(0),
            in_flight_bytes: AtomicUsize::new(0),
            completed_messages: AtomicU64::new(0),
            admission: Mutex::new(()),
            input_admitted: AtomicU32::new(0),
            input_completed: AtomicU32::new(0),
            admitted_at: std::array::from_fn(|_| AtomicU64::new(0)),
            writer_on: AtomicU8::new(WRITER_ON_IDLE),
            terminal: AtomicU8::new(TERMINAL_RUNNING),
        }
    }

    /// Admit one user-input message: under `admission`, call `attempt(false)` once the writer has
    /// ended so it refuses without sending; else count and stamp the ordinal before `attempt(true)`
    /// sends, so the writer can never complete an unadmitted message, and roll back a refused send.
    // Ordering: terminal is loaded Acquire; input_admitted is loaded Relaxed under the lock and,
    // like admitted_at, stored Release for input_progress's Acquire loads.
    fn admit<T>(&self, attempt: impl FnOnce(bool) -> Result<(), T>) -> Result<(), T> {
        let _admission = self.admission.lock();
        if self.terminal.load(Ordering::Acquire) != TERMINAL_RUNNING {
            // When: terminal is set, the writer ended and an admission now would be lost unrecorded.
            return attempt(false);
        }
        let previous = self.input_admitted.load(Ordering::Relaxed);
        let ordinal = previous.wrapping_add(1);
        let tag = u64::from(ordinal & INPUT_ORDINAL_MASK);
        self.admitted_at[ordinal as usize % INPUT_SLOTS]
            .store((tag << 44) | self.elapsed_millis(), Ordering::Release);
        self.input_admitted.store(ordinal, Ordering::Release);
        let sent = attempt(true);
        if sent.is_err() {
            // A refused send returns its ordinal, so ordinals stay in channel order.
            self.input_admitted.store(previous, Ordering::Release);
        }
        sent
    }

    /// Record why the writer stopped, once, under `admission`, so no later admission succeeds.
    // Ordering: terminal is stored Release, paired with admit's and input_progress's Acquire loads.
    pub(super) fn record_terminal(&self, cause: u8) {
        let _admission = self.admission.lock();
        self.terminal.store(cause, Ordering::Release);
    }

    /// The oldest outstanding user-input write, from atomics only: no queue, channel or admission lock.
    // Ordering: terminal, input_completed, input_admitted, admitted_at and writer_on load Acquire,
    // pairing with their Release stores.
    pub(super) fn input_progress(&self) -> PtyInputProgress {
        let terminal = self.terminal.load(Ordering::Acquire);
        let completed = self.input_completed.load(Ordering::Acquire);
        let admitted = self.input_admitted.load(Ordering::Acquire);
        let outstanding = admitted.wrapping_sub(completed) & INPUT_ORDINAL_MASK;
        if terminal != TERMINAL_RUNNING {
            // When: terminal is not running, every outstanding admission is lost.
            return PtyInputProgress::Terminal {
                failed: terminal != TERMINAL_STOPPED,
                lost: outstanding,
            };
        }
        if outstanding == 0 {
            // When: outstanding is 0, every admitted write completed.
            return PtyInputProgress::Idle;
        }
        let oldest = completed.wrapping_add(1);
        let stamp = self.admitted_at[oldest as usize % INPUT_SLOTS].load(Ordering::Acquire);
        let on_reply = self.writer_on.load(Ordering::Acquire) == WRITER_ON_REPLY;
        if self.input_completed.load(Ordering::Acquire) != completed
            || (stamp >> 44) as u32 != oldest & INPUT_ORDINAL_MASK
        {
            // When: input_completed moved or the slot's tag is another ordinal, the sample raced.
            return PtyInputProgress::Raced;
        }
        let admitted_ms = stamp & ((1 << 44) - 1);
        PtyInputProgress::Oldest {
            ordinal: oldest & INPUT_ORDINAL_MASK,
            admitted_at: self.epoch + Duration::from_millis(admitted_ms),
            on_reply,
        }
    }

    // Ordering: `operation`, `in_flight_bytes`, and `completed_messages` use `Relaxed` for independent observations.
    pub(super) fn observe(
        &self,
        queued_messages: usize,
        queued_bytes: usize,
    ) -> PtyInputDiagnostics {
        let operation = self.operation.load(Ordering::Relaxed);
        let phase = operation & 3;
        let writer_phase = match phase {
            1 => PtyWriterPhase::Writing,
            2 => PtyWriterPhase::Flushing,
            3 => PtyWriterPhase::Stopped,
            _ => PtyWriterPhase::Idle,
        };
        let in_flight_millis = if phase == 1 || phase == 2 {
            Some(self.elapsed_millis().saturating_sub(operation >> 2))
        } else {
            // When: `phase` is idle or stopped, no active operation has a meaningful elapsed time.
            None
        };
        PtyInputDiagnostics {
            queued_messages,
            queued_bytes,
            queue_capacity: PTY_INPUT_QUEUE_CAPACITY,
            writer_phase,
            in_flight_bytes: self.in_flight_bytes.load(Ordering::Relaxed),
            completed_messages: self.completed_messages.load(Ordering::Relaxed),
            in_flight_millis,
        }
    }

    fn elapsed_millis(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u128::from(u64::MAX >> 2)) as u64
    }

    // Ordering: `operation` uses `Relaxed` to pack phase and its timestamp without publishing payload data.
    pub(super) fn begin(&self, phase: PtyWriterPhase) {
        self.operation.store((self.elapsed_millis() << 2) | phase as u64, Ordering::Relaxed);
    }
}

impl PtyInputSender {
    /// Queue input, refusing rather than blocking.
    ///
    /// # Errors
    ///
    /// Returns [`PtyInputError`] when the message exceeds the cap, the queue
    /// is full, or the writer has gone. The error retains the bytes so the
    /// caller can retry or report rather than losing them silently.
    pub fn send(&self, bytes: Vec<u8>) -> Result<(), PtyInputError> {
        try_queue_pty_input(&self.tx, &self.queued_bytes, &self.writer_progress, bytes)
    }

    /// The oldest outstanding user-input write, or how the writer stopped.
    #[must_use]
    pub fn input_progress(&self) -> PtyInputProgress {
        self.writer_progress.input_progress()
    }

    /// Whether the owning pane has begun intentional PTY teardown rather than a native writer failure.
    pub fn is_closing(&self) -> bool {
        self.writer_progress.closing.load(Ordering::SeqCst)
    }

    /// Sample queue occupancy and writer activity without blocking or inspecting input bytes.
    // Ordering: `queued_bytes` uses `Relaxed` for an observation independent of channel length and writer progress.
    #[must_use]
    pub fn diagnostics(&self) -> PtyInputDiagnostics {
        self.writer_progress.observe(self.tx.len(), self.queued_bytes.load(Ordering::Relaxed))
    }
}

impl PtyInputError {
    /// Recover the rejected bytes so a caller can retry or report their size.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::MessageTooLarge(bytes)
            | Self::QueueFull(bytes)
            | Self::WriterDisconnected(bytes) => bytes,
        }
    }
}

pub(super) fn pty_input_channel() -> (Sender<Outgoing>, Receiver<Outgoing>) {
    crossbeam_channel::bounded(PTY_INPUT_QUEUE_CAPACITY)
}

// Ordering: `queued_bytes` uses `Relaxed`; channel ownership supplies ordering, while the atomic tracks byte accounting only.
pub(super) fn try_queue_pty_input(
    tx: &Sender<Outgoing>,
    queued_bytes: &AtomicUsize,
    progress: &PtyWriterProgress,
    bytes: Vec<u8>,
) -> Result<(), PtyInputError> {
    if !pty_input_message_allowed(bytes.len()) {
        // When: `bytes` exceeds the cap, reject it before charging or entering the bounded queue.
        return Err(PtyInputError::MessageTooLarge(bytes));
    }
    // Counted before the send, because once the message is in the channel the
    // writer thread may drain it before this line would otherwise run — which
    // would decrement a counter that had never been incremented.
    let len = bytes.len();
    queued_bytes.fetch_add(len, Ordering::Relaxed);
    let admitted = progress.admit(|running| {
        if !running {
            // When: running is false, the writer has ended, so the bytes are handed back unsent.
            return Err(PtyInputError::WriterDisconnected(bytes));
        }
        tx.try_send(bytes).map_err(|error| match error {
            crossbeam_channel::TrySendError::Full(bytes) => PtyInputError::QueueFull(bytes),
            crossbeam_channel::TrySendError::Disconnected(bytes) => {
                PtyInputError::WriterDisconnected(bytes)
            }
        })
    });
    if admitted.is_err() {
        // A refused message never entered the queue, so it is not queued memory.
        queued_bytes.fetch_sub(len, Ordering::Relaxed);
    }
    admitted
}

/// Reports whether one input message fits the enforced per-message byte cap.
#[must_use]
pub fn pty_input_message_allowed(bytes: usize) -> bool {
    bytes <= MAX_PTY_INPUT_MESSAGE_BYTES
}

/// Worst-case bytes that can wait in one pane's PTY input channel.
///
/// The product of the slot count and the per-message cap. This is the bound,
/// not the occupancy: [`PtyHandle::queued_input_bytes`] reports what is
/// actually held.
#[must_use]
pub const fn max_pty_queued_input_bytes() -> usize {
    PTY_INPUT_QUEUE_CAPACITY.saturating_mul(MAX_PTY_INPUT_MESSAGE_BYTES)
}

#[cfg(test)]
#[path = "pty_input_tests.rs"]
mod pty_input_tests;
