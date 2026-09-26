//! Per-device containment of wgpu errors and device loss.
//!
//! wgpu's default uncaptured-error handler panics, and the release profile
//! aborts on panic, so one invalid operation would end every window and shell.
//! [Handler installation](crate::device_errors::install_device_error_handlers) replaces it
//! and installs the device-lost callback where the renderer requests a device.
//! Renderers on one shared context hold one [`crate::device_errors::DeviceErrorState`],
//! so an error raised by any of them stops GPU work for all of them.
//!
//! Production code pushes no error scopes. Every validation, out-of-memory, or
//! internal error reaches the handler, which wgpu runs inline on the raising thread,
//! and moves the device to [`crate::device_errors::DeviceState::Unusable`]. Only the
//! test fault hook's isolated operation uses an explicit scope, through
//! [`crate::device_errors::run_isolated_validation`].

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Whether a device still accepts GPU work.
///
/// Transitions are one-way within a device generation: `Usable`, then
/// `Unusable`, then `Lost`. Nothing moves a device back to `Usable`; recovery
/// has to build a new device, and with it a new state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceState {
    /// No contained error has stopped this device.
    Usable,
    /// A validation, out-of-memory, or internal error reached the handler.
    /// Objects created on the device may be invalid, so no GPU work runs.
    Unusable,
    /// The device-lost callback ran, after a driver reset or an intentional
    /// destroy.
    Lost,
}

impl DeviceState {
    const fn to_raw(self) -> u8 {
        match self {
            Self::Usable => 0,
            Self::Unusable => 1,
            Self::Lost => 2,
        }
    }

    const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Usable,
            1 => Self::Unusable,
            _ => Self::Lost,
        }
    }
}

/// One reading of a device's state together with its destroy request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceGate {
    /// The device state at the time of the reading.
    pub state: DeviceState,
    /// Whether an intentional destroy was requested before the reading.
    pub destroy_requested: bool,
}

impl DeviceGate {
    /// Whether a renderer may issue GPU work: the device is usable and no
    /// intentional destroy is pending.
    #[must_use]
    pub const fn accepts_gpu_work(self) -> bool {
        matches!(self.state, DeviceState::Usable) && !self.destroy_requested
    }
}

/// How far one frame got before its device stopped accepting work.
///
/// The render path reads the device gate at three checkpoints: before GPU
/// work, after submission, and after presentation. Only `Presented` may
/// acknowledge the frame's plan or count as a native presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFrameOutcome {
    /// The device had stopped before the frame issued GPU work.
    NotStarted,
    /// Work was issued, but the device had stopped by the end of submission.
    /// The surface texture is dropped unpresented.
    SubmittedNotPresented,
    /// The frame reached the presenter, but the device stopped during
    /// presentation. The plan stays unacknowledged.
    PresentedNotAcknowledged,
    /// The device stayed usable through presentation.
    Presented,
}

impl DeviceFrameOutcome {
    /// Whether the surface texture may be handed to the presenter.
    #[must_use]
    pub const fn presents(self) -> bool {
        matches!(self, Self::PresentedNotAcknowledged | Self::Presented)
    }

    /// Whether the frame may acknowledge its plan and advance the
    /// successful-frame count.
    #[must_use]
    pub const fn acknowledges(self) -> bool {
        matches!(self, Self::Presented)
    }
}

/// Map the gate readings at a frame's three checkpoints to its outcome.
///
/// Pure, so the whole table is testable without a GPU. A later reading
/// matters only when every earlier one accepted work. Before presenting, the
/// render path passes its after-submission reading twice: the state only
/// moves forward, so that reading already decides whether the frame presents.
#[must_use]
pub const fn decide_frame_outcome(
    before: DeviceGate,
    after_submit: DeviceGate,
    after_present: DeviceGate,
) -> DeviceFrameOutcome {
    if !before.accepts_gpu_work() {
        // When: `before` refuses work, nothing was issued, so neither later reading applies.
        return DeviceFrameOutcome::NotStarted;
    }
    if !after_submit.accepts_gpu_work() {
        // When: `after_submit` refuses work, the submission may reference an invalid object.
        return DeviceFrameOutcome::SubmittedNotPresented;
    }
    if !after_present.accepts_gpu_work() {
        // When: `after_present` refuses work, the presented frame must stay unacknowledged.
        return DeviceFrameOutcome::PresentedNotAcknowledged;
    }
    DeviceFrameOutcome::Presented
}

/// The kind of one contained error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceErrorKind {
    /// A validation error reached the uncaptured handler.
    Validation,
    /// An out-of-memory error reached the uncaptured handler.
    OutOfMemory,
    /// An internal error reached the uncaptured handler.
    Internal,
    /// A validation error captured by the isolated-operation scope.
    Isolated,
    /// The device-lost callback ran.
    Lost,
}

/// Coalesced per-kind counts for one device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceErrorCounts {
    /// Uncaptured validation errors.
    pub validation: u64,
    /// Uncaptured out-of-memory errors.
    pub out_of_memory: u64,
    /// Uncaptured internal errors.
    pub internal: u64,
    /// Errors captured by the isolated-operation scope.
    pub isolated: u64,
    /// Device-lost callback invocations.
    pub lost: u64,
}

/// The first error of one class, kept for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceErrorRecord {
    /// The device generation the error belongs to.
    pub generation: u64,
    /// The error's kind.
    pub kind: DeviceErrorKind,
    /// The renderer operation that was running on the raising thread, such as
    /// `render.submit`, or `unlabelled` outside every labelled scope.
    pub operation: &'static str,
    /// wgpu's description of the error, or the loss reason and message.
    pub description: String,
}

/// A point-in-time copy of one device's containment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceErrorSnapshot {
    /// Process-unique identity of the device this state belongs to.
    pub generation: u64,
    /// The device state.
    pub state: DeviceState,
    /// Whether an intentional destroy was requested.
    pub destroy_requested: bool,
    /// Coalesced per-kind counts.
    pub counts: DeviceErrorCounts,
    /// Log records emitted: one per transition, plus the first isolated error.
    pub records_logged: u64,
    /// App wakes delivered: at most one per transition.
    pub wakes: u64,
    /// GPU-work scopes the gate admitted.
    pub admitted_work: u64,
    /// GPU-work scopes the gate refused because the device had stopped.
    pub refused_work: u64,
    /// The error that moved the device to `Unusable`, if any.
    pub unusable: Option<DeviceErrorRecord>,
    /// The device-lost record, set once.
    pub lost: Option<DeviceErrorRecord>,
    /// The first isolated error, if any.
    pub isolated: Option<DeviceErrorRecord>,
}

/// Callback that wakes the app after a device transition.
///
/// It runs inline on whichever thread raised the error, at most once per
/// transition. It must not wait on app state and must not panic: a panic here
/// unwinds through wgpu's error handler, and the release profile aborts.
pub type DeviceStateWaker = Arc<dyn Fn() + Send + Sync>;

/// Source of process-unique device generations. Only uniqueness matters:
/// tests compare generations and never assert an absolute value.
static NEXT_DEVICE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Label recorded for an error raised outside every labelled GPU-work scope.
const UNLABELLED_OPERATION: &str = "unlabelled";

/// Bound on the poll that lets an intentional destroy reach the lost callback.
const DESTROY_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// Size of the buffer the frame-validation fault clears at an invalid offset.
const FRAME_FAULT_PROBE_BYTES: u64 = 16;

thread_local! {
    /// The operation label of the innermost live [`GpuWorkScope`] on this thread.
    static CURRENT_OPERATION: Cell<&'static str> = const { Cell::new(UNLABELLED_OPERATION) };
}

fn current_operation() -> &'static str {
    // A thread that is tearing down its locals can still raise an error; it gets a fixed label.
    CURRENT_OPERATION.try_with(Cell::get).unwrap_or("thread_exit")
}

fn swap_operation(operation: &'static str) -> &'static str {
    // A thread that is tearing down its locals keeps the unlabelled default.
    CURRENT_OPERATION.try_with(|cell| cell.replace(operation)).unwrap_or(UNLABELLED_OPERATION)
}

/// Containment state shared by every renderer on one wgpu device.
///
/// The handlers only touch atomics and set-once cells, so they never wait on
/// an app, window, or renderer lock, and they never panic.
pub struct DeviceErrorState {
    generation: u64,
    state: AtomicU8,
    destroy_requested: AtomicBool,
    validation: AtomicU64,
    out_of_memory: AtomicU64,
    internal: AtomicU64,
    isolated: AtomicU64,
    lost: AtomicU64,
    records_logged: AtomicU64,
    wakes: AtomicU64,
    admitted_work: AtomicU64,
    refused_work: AtomicU64,
    unusable_record: OnceLock<DeviceErrorRecord>,
    lost_record: OnceLock<DeviceErrorRecord>,
    isolated_record: OnceLock<DeviceErrorRecord>,
    waker: OnceLock<DeviceStateWaker>,
}

impl Default for DeviceErrorState {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceErrorState {
    /// A usable state with a new, process-unique device generation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: NEXT_DEVICE_GENERATION.fetch_add(1, Ordering::SeqCst),
            state: AtomicU8::new(DeviceState::Usable.to_raw()),
            destroy_requested: AtomicBool::new(false),
            validation: AtomicU64::new(0),
            out_of_memory: AtomicU64::new(0),
            internal: AtomicU64::new(0),
            isolated: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            records_logged: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
            admitted_work: AtomicU64::new(0),
            refused_work: AtomicU64::new(0),
            unusable_record: OnceLock::new(),
            lost_record: OnceLock::new(),
            isolated_record: OnceLock::new(),
            waker: OnceLock::new(),
        }
    }

    /// Process-unique identity of the device this state belongs to.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The current device state.
    #[must_use]
    pub fn state(&self) -> DeviceState {
        DeviceState::from_raw(self.state.load(Ordering::SeqCst))
    }

    /// Read the state and the destroy request together.
    #[must_use]
    pub fn gate(&self) -> DeviceGate {
        DeviceGate {
            destroy_requested: self.destroy_requested.load(Ordering::SeqCst),
            state: self.state(),
        }
    }

    /// Whether the gate admits GPU work right now.
    #[must_use]
    pub fn accepts_gpu_work(&self) -> bool {
        self.gate().accepts_gpu_work()
    }

    /// Current coalesced per-kind counts.
    #[must_use]
    pub fn counts(&self) -> DeviceErrorCounts {
        DeviceErrorCounts {
            validation: self.validation.load(Ordering::SeqCst),
            out_of_memory: self.out_of_memory.load(Ordering::SeqCst),
            internal: self.internal.load(Ordering::SeqCst),
            isolated: self.isolated.load(Ordering::SeqCst),
            lost: self.lost.load(Ordering::SeqCst),
        }
    }

    /// A point-in-time copy of the whole state, for diagnostics and tests.
    #[must_use]
    pub fn snapshot(&self) -> DeviceErrorSnapshot {
        let gate = self.gate();
        DeviceErrorSnapshot {
            generation: self.generation,
            state: gate.state,
            destroy_requested: gate.destroy_requested,
            counts: self.counts(),
            records_logged: self.records_logged.load(Ordering::SeqCst),
            wakes: self.wakes.load(Ordering::SeqCst),
            admitted_work: self.admitted_work.load(Ordering::SeqCst),
            refused_work: self.refused_work.load(Ordering::SeqCst),
            unusable: self.unusable_record.get().cloned(),
            lost: self.lost_record.get().cloned(),
            isolated: self.isolated_record.get().cloned(),
        }
    }

    /// Install the callback that wakes the app after a transition.
    ///
    /// Only the first call on a device installs a waker; renderers that share
    /// the device keep it. Returns whether this call installed it.
    pub fn set_waker(&self, waker: DeviceStateWaker) -> bool {
        self.waker.set(waker).is_ok()
    }

    /// Open a labelled GPU-work scope, or refuse while the device is stopped.
    ///
    /// Errors raised on this thread while the scope lives carry `operation`.
    /// `None` means the device is `Unusable` or `Lost`, or an intentional
    /// destroy is pending, and the caller must issue no GPU work.
    #[must_use]
    pub fn enter_gpu_work(&self, operation: &'static str) -> Option<GpuWorkScope> {
        if !self.accepts_gpu_work() {
            // When: `accepts_gpu_work` is false, work would reach an invalid or destroyed device.
            self.refused_work.fetch_add(1, Ordering::SeqCst);
            return None;
        }
        self.admitted_work.fetch_add(1, Ordering::SeqCst);
        Some(GpuWorkScope { previous: swap_operation(operation), _thread: PhantomData })
    }

    /// Run `f` as labelled GPU work while the device is usable.
    ///
    /// Returns `None` without running `f` while the device is stopped.
    pub fn gpu_work<R>(&self, operation: &'static str, f: impl FnOnce() -> R) -> Option<R> {
        let _scope = self.enter_gpu_work(operation)?;
        Some(f())
    }

    /// Record an intentional destroy before it starts, so the gate closes
    /// before the device becomes invalid.
    pub fn request_destroy(&self) {
        self.destroy_requested.store(true, Ordering::SeqCst);
    }

    /// Record an error that reached the uncaptured handler.
    ///
    /// The first one moves the device to `Unusable`, logs one record, and
    /// wakes the app; later errors only update the counts.
    pub fn record_uncaptured(&self, error: &wgpu::Error) {
        let kind = match error {
            wgpu::Error::Validation { .. } => DeviceErrorKind::Validation,
            wgpu::Error::OutOfMemory { .. } => DeviceErrorKind::OutOfMemory,
            wgpu::Error::Internal { .. } => DeviceErrorKind::Internal,
        };
        self.record_stop(kind, || error.to_string());
    }

    /// Record a validation stop the renderer observed itself, such as a
    /// surface acquisition that returned a validation status.
    ///
    /// Counts nothing when the handler already stopped the device, so one
    /// error is never counted twice.
    pub fn record_observed_validation(&self, description: &'static str) {
        if self.state() != DeviceState::Usable {
            // When: `state` is no longer `Usable`, the handler already recorded this same error.
            return;
        }
        self.record_stop(DeviceErrorKind::Validation, || description.to_owned());
    }

    /// Record the device-lost callback.
    ///
    /// The first call moves the device to `Lost`, logs one record, and wakes
    /// the app. The lost record is set once; a repeat only counts.
    pub fn record_lost(&self, reason: wgpu::DeviceLostReason, message: &str) {
        self.lost.fetch_add(1, Ordering::SeqCst);
        if !self.advance(DeviceState::Lost) {
            // When: `advance` to `Lost` fails, the earlier loss record stays authoritative.
            return;
        }
        let separator = if message.is_empty() { "" } else { ": " };
        let description = format!("{reason:?}{separator}{message}");
        let record = self.record(DeviceErrorKind::Lost, description);
        self.log_transition(DeviceState::Lost, &record, Some(reason));
        let _ = self.lost_record.set(record);
        self.wake();
    }

    /// Record an error captured by the isolated-operation scope.
    ///
    /// The device stays usable. Only the first isolated error logs; later ones
    /// only count, and none wakes the app.
    pub fn record_isolated(&self, error: &wgpu::Error) {
        if self.isolated.fetch_add(1, Ordering::SeqCst) != 0 {
            // When: an isolated error was already recorded, a repeat only raises the count.
            return;
        }
        let record = self.record(DeviceErrorKind::Isolated, error.to_string());
        let counts = self.counts();
        tracing::warn!(
            target: "sonic::gpu",
            generation = record.generation,
            state = ?self.state(),
            kind = ?record.kind,
            operation = record.operation,
            description = %record.description,
            validation = counts.validation,
            out_of_memory = counts.out_of_memory,
            internal = counts.internal,
            isolated = counts.isolated,
            lost = counts.lost,
            "contained isolated GPU error"
        );
        self.records_logged.fetch_add(1, Ordering::SeqCst);
        let _ = self.isolated_record.set(record);
    }

    fn counter(&self, kind: DeviceErrorKind) -> &AtomicU64 {
        match kind {
            DeviceErrorKind::Validation => &self.validation,
            DeviceErrorKind::OutOfMemory => &self.out_of_memory,
            DeviceErrorKind::Internal => &self.internal,
            DeviceErrorKind::Isolated => &self.isolated,
            DeviceErrorKind::Lost => &self.lost,
        }
    }

    /// Move the state forward to `target`; true only for the call that moved it.
    fn advance(&self, target: DeviceState) -> bool {
        self.state.fetch_max(target.to_raw(), Ordering::SeqCst) < target.to_raw()
    }

    fn record_stop(&self, kind: DeviceErrorKind, describe: impl FnOnce() -> String) {
        self.counter(kind).fetch_add(1, Ordering::SeqCst);
        if !self.advance(DeviceState::Unusable) {
            // When: `advance` to `Unusable` fails, an earlier stop exists; repeats only count.
            return;
        }
        let record = self.record(kind, describe());
        self.log_transition(DeviceState::Unusable, &record, None);
        let _ = self.unusable_record.set(record);
        self.wake();
    }

    fn record(&self, kind: DeviceErrorKind, description: String) -> DeviceErrorRecord {
        DeviceErrorRecord {
            generation: self.generation,
            kind,
            operation: current_operation(),
            description,
        }
    }

    fn log_transition(
        &self,
        state: DeviceState,
        record: &DeviceErrorRecord,
        lost_reason: Option<wgpu::DeviceLostReason>,
    ) {
        let counts = self.counts();
        let message = if state == DeviceState::Lost {
            "GPU device lost"
        } else {
            "GPU device stopped accepting work"
        };
        tracing::error!(
            target: "sonic::gpu",
            generation = record.generation,
            state = ?state,
            kind = ?record.kind,
            operation = record.operation,
            description = %record.description,
            lost_reason = ?lost_reason,
            destroy_requested = self.destroy_requested.load(Ordering::SeqCst),
            validation = counts.validation,
            out_of_memory = counts.out_of_memory,
            internal = counts.internal,
            isolated = counts.isolated,
            lost = counts.lost,
            "{message}"
        );
        self.records_logged.fetch_add(1, Ordering::SeqCst);
    }

    fn wake(&self) {
        let Some(waker) = self.waker.get() else {
            // When: no app waker is installed yet, the next render call still observes the stop.
            return;
        };
        self.wakes.fetch_add(1, Ordering::SeqCst);
        waker();
    }
}

/// A labelled region of GPU work on one thread.
///
/// Returned by [`DeviceErrorState::enter_gpu_work`]. Errors raised on this
/// thread while it lives carry its operation label; dropping it restores the
/// enclosing label.
#[must_use = "dropping the scope ends the labelled GPU work"]
pub struct GpuWorkScope {
    previous: &'static str,
    // The label is thread-local, so the scope must end on the thread that opened it.
    _thread: PhantomData<*const ()>,
}

impl GpuWorkScope {
    /// Relabel the rest of this scope, for a multi-step operation such as a
    /// frame's upload, acquisition, encode, submission, and presentation.
    pub fn set_operation(&self, operation: &'static str) {
        // A thread that is tearing down its locals has no label left to change.
        let _ = CURRENT_OPERATION.try_with(|cell| cell.set(operation));
    }
}

// Lifecycle: `GpuWorkScope` restores its `previous` label into `CURRENT_OPERATION` on drop,
// so nested scopes unwind in order.
impl Drop for GpuWorkScope {
    fn drop(&mut self) {
        // A thread that is tearing down its locals has no label left to restore.
        let _ = CURRENT_OPERATION.try_with(|cell| cell.set(self.previous));
    }
}

/// Install the uncaptured-error handler and the device-lost callback on a
/// newly requested device, and return the state they record into.
///
/// Call it once, immediately after `request_device` returns: each installation
/// replaces the previous handler, and a device without one falls back to
/// wgpu's default handler, which panics.
#[must_use]
pub fn install_device_error_handlers(device: &wgpu::Device) -> Arc<DeviceErrorState> {
    let state = Arc::new(DeviceErrorState::new());
    let uncaptured = Arc::clone(&state);
    device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
        uncaptured.record_uncaptured(&error);
    }));
    let lost = Arc::clone(&state);
    device.set_device_lost_callback(move |reason, message| {
        lost.record_lost(reason, &message);
    });
    state
}

/// Run `operation` inside an explicit validation scope and record any error
/// it raises as isolated.
///
/// This is the only error scope in production code, and only the test fault
/// hook calls it, so a contained error can be shown not to stop rendering.
/// Returns whether the scope captured an error.
pub fn run_isolated_validation(
    device: &wgpu::Device,
    state: &DeviceErrorState,
    operation: impl FnOnce(),
) -> bool {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    operation();
    let Some(error) = pollster::block_on(scope.pop()) else {
        // When: the operation raised no validation error, there is nothing isolated to record.
        return false;
    };
    state.record_isolated(&error);
    true
}

/// Deliberately destroy `device` and wait, bounded, for its lost callback.
///
/// wgpu reports an intentional destroy only while the device is polled, and
/// production code never polls, so this is the one poll outside tests. The
/// destroy request closes the gate first, so no renderer issues work while the
/// device is invalid but not yet recorded lost.
pub fn destroy_and_await_loss(device: &wgpu::Device, state: &DeviceErrorState) {
    state.request_destroy();
    device.destroy();
    // A timeout leaves the state unlost; the caller's missing lost record reports it.
    let _ = device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(DESTROY_POLL_TIMEOUT) });
}

/// A test fault the renderer's fault hook can raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuFaultKind {
    /// Create a buffer with an empty usage inside an explicit validation
    /// scope. The error is logged as isolated and the device stays usable.
    IsolatedOperation,
    /// Arm the next glyph-upload rebuild to create an invalid texture.
    RetainedResourceCreation,
    /// Record an invalid command in every later frame.
    FrameValidation,
    /// Request a destroy, destroy the device, then poll for up to 5 s so the
    /// lost callback runs.
    DestroyDevice,
}

/// Create the buffer the frame-validation fault records an invalid clear into.
pub(crate) fn create_frame_fault_probe(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sonic-frame-fault-probe"),
        size: FRAME_FAULT_PROBE_BYTES,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Record a clear at an unaligned offset, which wgpu rejects as a validation
/// error. Used only while the frame-validation fault is armed.
pub(crate) fn record_invalid_frame_command(
    encoder: &mut wgpu::CommandEncoder,
    probe: &wgpu::Buffer,
) {
    encoder.clear_buffer(probe, 1, None);
}

/// Submit one command buffer that holds the invalid frame command.
///
/// The Windows CPU presenter issues no GPU work of its own, so the armed
/// frame-validation fault needs this separate submission to reach the device.
#[cfg(target_os = "windows")]
pub(crate) fn submit_invalid_frame_command(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    probe: &wgpu::Buffer,
) {
    let descriptor = wgpu::CommandEncoderDescriptor { label: Some("sonic-fault") };
    let mut encoder = device.create_command_encoder(&descriptor);
    record_invalid_frame_command(&mut encoder, probe);
    queue.submit(Some(encoder.finish()));
}

#[cfg(test)]
#[path = "device_errors_tests.rs"]
mod device_errors_tests;
