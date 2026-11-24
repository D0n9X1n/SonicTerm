//! Sensitive parser replies, the OSC 52 clipboard read answers, which must never reach the reply
//! spool's disk spill nor wait on a lock: one bounded slot per PTY, admitted with `try_lock` on the
//! writer's admission lock, each carrying a custody value (the clipboard lease and read ticket)
//! that the writer drops only once the bytes are written or discarded.

use super::*;

/// Sensitive replies one PTY's writer may hold queued; a pane has one clipboard read in flight.
const SENSITIVE_REPLY_SLOTS: usize = 1;

/// The incarnation the next spawned PTY takes; 0 is never handed out.
static NEXT_PTY_INCARNATION: AtomicU64 = AtomicU64::new(1);

/// A distinct, nonzero incarnation for a newly spawned PTY.
// Ordering: NEXT_PTY_INCARNATION is Relaxed; each caller needs only a distinct value.
pub(super) fn next_pty_incarnation() -> u64 {
    NEXT_PTY_INCARNATION.fetch_add(1, Ordering::Relaxed)
}

/// The bounded channel a PTY's writer reads sensitive replies from.
pub(crate) fn sensitive_reply_channel() -> (Sender<SensitiveReply>, Receiver<SensitiveReply>) {
    crossbeam_channel::bounded(SENSITIVE_REPLY_SLOTS)
}

/// One sensitive reply and what it keeps alive until the writer is done with its bytes. `Debug`
/// prints only the length, so the reply can never be logged.
pub struct SensitiveReply {
    bytes: Vec<u8>,
    custody: Box<dyn std::any::Any + Send>,
}

impl SensitiveReply {
    /// A reply of `bytes` whose `custody` the writer drops after writing or discarding them.
    #[must_use]
    pub fn new(bytes: Vec<u8>, custody: Box<dyn std::any::Any + Send>) -> Self {
        Self { bytes, custody }
    }

    /// The reply's length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The reply's bytes, for the writer and tests; never logged.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether the reply holds no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The bytes to write and the custody to drop once they are written.
    pub(super) fn into_parts(self) -> (Vec<u8>, Box<dyn std::any::Any + Send>) {
        (self.bytes, self.custody)
    }
}

impl std::fmt::Debug for SensitiveReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SensitiveReply").field("bytes", &self.bytes.len()).finish()
    }
}

/// Why a sensitive reply was not queued; each variant hands the reply back with its custody.
#[derive(Debug)]
pub enum SensitiveReplyError {
    /// The admission lock was held; the caller may retry on a later turn.
    Contended(SensitiveReply),
    /// The reply exceeds [`MAX_PTY_INPUT_MESSAGE_BYTES`].
    TooLarge(SensitiveReply),
    /// The writer already holds a sensitive reply.
    Full(SensitiveReply),
    /// The writer stopped; nothing will be written.
    WriterStopped(SensitiveReply),
}

/// Queue `reply` on `slot` without waiting: refused past the message cap, when the admission lock
/// is taken, once the writer recorded its end, or when the slot is full. Under the admission lock,
/// so no reply is admitted after `record_terminal`, and `input_seen` is never touched.
// Ordering: terminal loads Acquire under the admission lock, pairing with record_terminal's store.
pub(super) fn try_admit_sensitive(
    progress: &PtyWriterProgress,
    slot: &Sender<SensitiveReply>,
    reply: SensitiveReply,
) -> Result<(), SensitiveReplyError> {
    if !pty_input_message_allowed(reply.len()) {
        // When: reply is past the per-message cap, it is refused before any lock.
        return Err(SensitiveReplyError::TooLarge(reply));
    }
    let Some(_admission) = progress.admission.try_lock() else {
        // When: try_lock finds the admission lock taken, the caller retries rather than waits.
        return Err(SensitiveReplyError::Contended(reply));
    };
    if progress.terminal.load(Ordering::Acquire) != TERMINAL_RUNNING {
        // When: terminal is recorded, the writer ended and would never write the reply.
        return Err(SensitiveReplyError::WriterStopped(reply));
    }
    slot.try_send(reply).map_err(|error| match error {
        crossbeam_channel::TrySendError::Full(reply) => SensitiveReplyError::Full(reply),
        crossbeam_channel::TrySendError::Disconnected(reply) => {
            SensitiveReplyError::WriterStopped(reply)
        }
    })
}

impl PtyHandle {
    /// Queue a sensitive reply for this PTY's writer, never blocking and never through the reply
    /// spool; the writer drops the reply's custody after writing it.
    ///
    /// # Errors
    ///
    /// [`SensitiveReplyError`], handing the reply back, when it could not be queued now.
    pub fn try_send_reply(&self, reply: SensitiveReply) -> Result<(), SensitiveReplyError> {
        try_admit_sensitive(&self.writer_progress, &self.sensitive_tx, reply)
    }

    /// This PTY's incarnation: distinct for every PTY the process spawns, never 0.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }
}

#[cfg(test)]
#[path = "pty_sensitive_reply_tests.rs"]
mod pty_sensitive_reply_tests;
