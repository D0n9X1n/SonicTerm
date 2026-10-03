//! Foreground-process probes on a worker thread, with a latest-value result map.
//!
//! Frames and the Windows timer only set demand and read each pane's cache. One
//! `sonicterm-fg-probe` thread per App samples the panes that want a sample and stores the
//! newest result per pane in a shared map. The event loop drains that map on
//! [`UserEvent::ForegroundProbeReady`] and accepts a result only for the live pane whose
//! process identity it was taken for and whose child is not known to have exited.
//!
//! Bounds: at most one map entry and one stored result per live pane, a batch of at most the
//! live panes at its take, and at most one undelivered ready event.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::Instant,
};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use sonicterm_io::{
    proc_info::{ForegroundProcess, ProcessIdentity},
    pty::PtyExitObserved,
};
use winit::{event_loop::EventLoopProxy, window::WindowId};

use super::{
    frame_counters::{AtomicHistogram, HistogramUnit},
    privilege::cached_foreground_privileged,
    redraw::RedrawCause,
    App, PaneState, UserEvent, FOREGROUND_PROCESS_TTL,
};

/// Why a pane wants a foreground sample.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WantOrigin {
    /// A non-burst frame found the pane's cache older than the TTL.
    Frame,
    /// The Windows activity wake (accepted input or quiet output) expired.
    Activity,
    /// The Windows warning wake expired while a per-tab warning is shown.
    Warning,
}

impl WantOrigin {
    /// Merge a new demand into a pending one: a non-warning origin replaces `Warning`, and
    /// `Warning` never replaces another origin, so clearing warning-only demand keeps the rest.
    fn merge(pending: Option<Self>, new: Self) -> Self {
        match (pending, new) {
            (Some(existing), Self::Warning) => existing,
            (_, new) => new,
        }
    }
}

/// One sample the worker took for a pane, tagged with the identity token it was taken for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProbeResult {
    /// Start token of the process the sample describes.
    pub(crate) token: u64,
    /// The foreground process, or `None` when there was none or the pid changed hands.
    pub(crate) observation: Option<ForegroundProcess>,
    /// When the worker finished this pane's sample.
    pub(crate) sampled_at: Instant,
}

/// A pane's slot in the shared map.
struct Entry {
    identity: ProcessIdentity,
    want: Option<WantOrigin>,
    result: Option<ProbeResult>,
}

/// State the worker and the event loop share under one mutex.
#[derive(Default)]
struct MapState {
    entries: HashMap<u64, Entry>,
    /// A ready event is queued and not yet drained.
    posted: bool,
    /// The worker has exited; the next drain enters `Unavailable`.
    dead: bool,
}

/// Post one ready event to the event loop; `false` when it can no longer be delivered.
pub(crate) type Notify = Box<dyn Fn() -> bool + Send + Sync>;
/// Sample one batch of identities, calling `stop` before each probe; results keep input order.
pub(crate) type Sampler = Arc<
    dyn Fn(&[ProcessIdentity], &dyn Fn() -> bool) -> Vec<Option<ForegroundProcess>> + Send + Sync,
>;
/// Start the worker job on a new thread.
pub(crate) type Spawner =
    Box<dyn Fn(Box<dyn FnOnce() + Send>) -> std::io::Result<()> + Send + Sync>;
/// The worker's clock; production reads `Instant::now`.
pub(crate) type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The map, the shutdown flag and the ready notifier, shared by the App, the worker and
/// each pane's registration.
pub(crate) struct ProbeShared {
    map: Mutex<MapState>,
    shutdown: AtomicBool,
    notify: Notify,
}

/// Counters the worker records when the App's counter gate is on.
#[derive(Debug)]
pub(crate) struct ForegroundWorkerStats {
    /// Batches probed.
    pub(crate) probes: AtomicU64,
    /// Panes those batches covered.
    pub(crate) panes: AtomicU64,
    /// Each batch's duration, in microseconds.
    pub(crate) probe: AtomicHistogram,
    /// Results the event loop dropped: the pane was gone, re-identified or exited.
    pub(crate) stale: AtomicU64,
}

impl Default for ForegroundWorkerStats {
    fn default() -> Self {
        Self {
            probes: AtomicU64::new(0),
            panes: AtomicU64::new(0),
            probe: AtomicHistogram::new(HistogramUnit::Micros),
            stale: AtomicU64::new(0),
        }
    }
}

/// The worker's lifecycle as the event loop sees it.
enum WorkerState {
    NotStarted,
    Running(Sender<()>),
    /// Spawn failed, the worker died, or the App is dropping; demand resolves synchronously.
    Unavailable,
}

/// The outcome of setting demand for one pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Demand {
    /// The cache, or an earlier demand, is younger than the TTL.
    Fresh,
    /// The pane wants a sample; the caller wakes the worker once for all its demands.
    Queued,
    /// Resolved without a worker: exit, no identity, or `Unavailable`. `changed` reports
    /// whether the cached observation changed.
    Resolved {
        /// Whether a cached process was cleared.
        changed: bool,
    },
}

/// Every injectable part of [`ForegroundProbes`].
pub(crate) struct ProbeParts {
    pub(crate) notify: Notify,
    pub(crate) sampler: Sampler,
    pub(crate) spawner: Spawner,
    pub(crate) clock: Clock,
    pub(crate) live_workers: Arc<AtomicUsize>,
}

/// One App's foreground probes: the shared map, the worker handle and its parts.
pub(crate) struct ForegroundProbes {
    shared: Arc<ProbeShared>,
    worker: Mutex<WorkerState>,
    sampler: Sampler,
    spawner: Spawner,
    clock: Clock,
    live_workers: Arc<AtomicUsize>,
    /// Worker statistics fixed when the counter gate seals; `None` when the gate is off.
    stats: OnceLock<Option<Arc<ForegroundWorkerStats>>>,
}

impl ForegroundProbes {
    /// Production probes posting through `proxy`; with no proxy every post fails, so a worker
    /// that starts exits after its first batch and later demand resolves synchronously.
    pub(crate) fn new(proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        // Windows' proxy is `Send` but not `Sync`, so the shared notifier holds it under a mutex.
        let proxy = Mutex::new(proxy);
        Self::from_parts(ProbeParts {
            notify: Box::new(move || {
                proxy
                    .lock()
                    .as_ref()
                    .is_some_and(|proxy| proxy.send_event(UserEvent::ForegroundProbeReady).is_ok())
            }),
            sampler: Arc::new(sample_platform),
            spawner: Box::new(|job| {
                std::thread::Builder::new().name("sonicterm-fg-probe".into()).spawn(job).map(drop)
            }),
            clock: Arc::new(Instant::now),
            live_workers: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Probes built from explicit parts; tests inject the sampler, spawner, clock and notifier.
    pub(crate) fn from_parts(parts: ProbeParts) -> Self {
        Self {
            shared: Arc::new(ProbeShared {
                map: Mutex::new(MapState::default()),
                shutdown: AtomicBool::new(false),
                notify: parts.notify,
            }),
            worker: Mutex::new(WorkerState::NotStarted),
            sampler: parts.sampler,
            spawner: parts.spawner,
            clock: parts.clock,
            live_workers: parts.live_workers,
            stats: OnceLock::new(),
        }
    }

    /// Fix the worker statistics when the counter gate seals; later calls are ignored.
    pub(crate) fn seal_stats(&self, stats: Option<Arc<ForegroundWorkerStats>>) {
        let _ = self.stats.set(stats);
    }

    /// The sealed worker statistics, if the gate is on.
    pub(crate) fn stats(&self) -> Option<&ForegroundWorkerStats> {
        self.stats.get().and_then(Option::as_deref)
    }

    /// Live `sonicterm-fg-probe` threads, reported as `live_fg_probe_workers`.
    // Ordering: live_workers loads Relaxed; it is a diagnostic count that orders nothing.
    pub(crate) fn live_workers(&self) -> usize {
        self.live_workers.load(Ordering::Relaxed)
    }

    /// Whether demand now resolves synchronously because the worker is gone for good.
    pub(crate) fn is_unavailable(&self) -> bool {
        matches!(*self.worker.lock(), WorkerState::Unavailable)
    }

    /// Set demand for one pane, or resolve it without the worker.
    ///
    /// A confirmed exit, a missing identity and `Unavailable` each cache `(now, None)`. A
    /// pane demands only when the newer of its sample and its last demand is a TTL old, unless
    /// `force` (a Windows wake that already waited the TTL). `fg_demanded_at` is the
    /// event-loop-owned rate limit.
    pub(crate) fn request(
        &self,
        pane_id: u64,
        pane: &mut PaneState,
        origin: WantOrigin,
        now: Instant,
        force: bool,
    ) -> Demand {
        let source = pane.foreground_source();
        if source.as_ref().is_some_and(|(_, exited)| exited.is_exited()) {
            // When: `exited.is_exited()`, the pid may already name another process; clear once.
            let cleared = pane.fg_probe.is_none()
                && pane.fg_proc_cache.as_ref().is_some_and(|(_, process)| process.is_none());
            if cleared {
                // When: the exit was already cleared, nothing changes until the pane closes.
                return Demand::Fresh;
            }
            return Demand::Resolved { changed: pane.clear_foreground(now) };
        }
        let last =
            pane.fg_proc_cache.as_ref().map(|(sampled, _)| *sampled).max(pane.fg_demanded_at);
        if !force
            && last.is_some_and(|last| now.saturating_duration_since(last) < FOREGROUND_PROCESS_TTL)
        {
            // When: `last` is younger than the TTL, the cache stands and no demand is set.
            return Demand::Fresh;
        }
        let Some((identity, _)) = source else {
            // When: `source` is `None`, the identity query failed or no PTY exists; nothing to sample.
            return Demand::Resolved { changed: pane.clear_foreground(now) };
        };
        if self.is_unavailable() {
            // When: `is_unavailable()`, there is no worker, so demand resolves synchronously.
            return Demand::Resolved { changed: pane.clear_foreground(now) };
        }
        pane.fg_demanded_at = Some(now);
        // A registration for another identity is dropped first, so its entry goes before ours.
        if pane.fg_probe.as_ref().is_some_and(|registration| registration.identity != identity) {
            pane.fg_probe = None;
        }
        {
            let mut map = self.shared.map.lock();
            let entry =
                map.entries.entry(pane_id).or_insert(Entry { identity, want: None, result: None });
            // A stored entry for another identity holds another process's demand and result.
            if entry.identity != identity {
                *entry = Entry { identity, want: None, result: None };
            }
            entry.want = Some(WantOrigin::merge(entry.want, origin));
        }
        // Register once, so the entry leaves the map with the pane.
        if pane.fg_probe.is_none() {
            pane.fg_probe =
                Some(ProbeRegistration { shared: Arc::clone(&self.shared), pane_id, identity });
        }
        Demand::Queued
    }

    /// Wake the worker for pending demand, starting it on first use.
    ///
    /// A full wake channel already holds a wake, so this one coalesces into it. A failed spawn
    /// or a disconnected worker enters `Unavailable`.
    pub(crate) fn wake(&self) {
        let mut spawn_error = None;
        let sender = {
            let mut worker = self.worker.lock();
            // The first demand starts the worker; the counter gate is sealed by now.
            if matches!(*worker, WorkerState::NotStarted) {
                match self.spawn_worker() {
                    Ok(sender) => *worker = WorkerState::Running(sender),
                    Err(error) => spawn_error = Some(error),
                }
            }
            match &*worker {
                WorkerState::Running(sender) => Some(sender.clone()),
                WorkerState::NotStarted | WorkerState::Unavailable => None,
            }
        };
        if let Some(error) = spawn_error {
            // When: `spawn_error` is set, no worker exists and none will be retried.
            self.enter_unavailable(&format!("worker spawn failed: {error}"));
            return;
        }
        let Some(sender) = sender else {
            // When: `sender` is `None`, the probes are `Unavailable` and demand was resolved.
            return;
        };
        // A disconnected channel means the worker exited without being told to.
        if let Err(TrySendError::Disconnected(())) = sender.try_send(()) {
            self.enter_unavailable("worker exited");
        }
    }

    /// Start the worker thread; its live count rises first and falls on exit, unwind or failure.
    fn spawn_worker(&self) -> std::io::Result<Sender<()>> {
        let (wake_tx, wake_rx) = crossbeam_channel::bounded(1);
        let live = LiveGuard::enter(Arc::clone(&self.live_workers));
        let shared = Arc::clone(&self.shared);
        let sampler = Arc::clone(&self.sampler);
        let clock = Arc::clone(&self.clock);
        let stats = self.stats.get_or_init(|| None).clone();
        (self.spawner)(Box::new(move || {
            let _live = live;
            let exit = ExitGuard { shared };
            run_worker(&wake_rx, &exit.shared, &sampler, stats.as_deref(), &clock);
            drop(exit);
        }))?;
        Ok(wake_tx)
    }

    /// Enter `Unavailable` once: one warning, every entry removed, no retry.
    // Lock order: worker -> map; the worker guard drops before map is locked, so they never nest.
    pub(crate) fn enter_unavailable(&self, reason: &str) {
        let previous_state = std::mem::replace(&mut *self.worker.lock(), WorkerState::Unavailable);
        if matches!(previous_state, WorkerState::Unavailable) {
            // When: `previous_state` matches `WorkerState::Unavailable`, it was entered and reported before.
            return;
        }
        tracing::warn!(
            target: "sonicterm_app::app",
            reason,
            "foreground-process probes unavailable; tab process names and privilege warnings stop sampling"
        );
        self.shared.map.lock().entries.clear();
    }

    /// Take every stored result, read `dead` and clear `posted`, in one critical section.
    pub(crate) fn drain(&self) -> Drained {
        let mut map = self.shared.map.lock();
        let results = map
            .entries
            .iter_mut()
            .filter_map(|(&pane_id, entry)| entry.result.take().map(|result| (pane_id, result)))
            .collect();
        let dead = map.dead;
        map.posted = false;
        Drained { results, dead }
    }

    /// Drop every demand that is still warning-only; frame and activity demand stays.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn clear_warning_wants(&self) {
        for entry in self.shared.map.lock().entries.values_mut() {
            // A pending `Warning` alone served a warning that has cleared.
            if entry.want == Some(WantOrigin::Warning) {
                entry.want = None;
            }
        }
    }

    /// Count results the event loop dropped, when the counter gate is on.
    // Ordering: stale adds Relaxed; it is a statistic that orders nothing.
    pub(crate) fn note_stale(&self, count: u64) {
        if let Some(stats) = self.stats().filter(|_| count > 0) {
            stats.stale.fetch_add(count, Ordering::Relaxed);
        }
    }
}

// Lifecycle: dropping ForegroundProbes sets shutdown, clears map entries and drops the worker's wake sender.
impl Drop for ForegroundProbes {
    // Ordering: shutdown stores Release, pairing with the worker's Acquire loads.
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.map.lock().entries.clear();
        *self.worker.get_mut() = WorkerState::Unavailable;
    }
}

/// Results taken by one drain, and whether the worker had died.
pub(crate) struct Drained {
    pub(crate) results: Vec<(u64, ProbeResult)>,
    pub(crate) dead: bool,
}

/// A pane's claim on its map entry; dropping it removes the entry, so the map never outlives
/// its panes. It moves with the pane on transfer and tear-out.
pub(crate) struct ProbeRegistration {
    shared: Arc<ProbeShared>,
    pane_id: u64,
    /// The process identity this pane's entry was registered for.
    pub(crate) identity: ProcessIdentity,
}

// Lifecycle: ProbeRegistration drop removes its map entries slot only while it still names its identity.
impl Drop for ProbeRegistration {
    fn drop(&mut self) {
        let mut map = self.shared.map.lock();
        // Only an entry that still names this identity is this registration's to remove.
        if map.entries.get(&self.pane_id).is_some_and(|entry| entry.identity == self.identity) {
            map.entries.remove(&self.pane_id);
        }
    }
}

/// Counts a worker thread from just before spawn until its job ends or is dropped unrun.
struct LiveGuard {
    live_workers: Arc<AtomicUsize>,
}

impl LiveGuard {
    // Ordering: live_workers adds Relaxed; the count is diagnostic.
    fn enter(live_workers: Arc<AtomicUsize>) -> Self {
        live_workers.fetch_add(1, Ordering::Relaxed);
        Self { live_workers }
    }
}

// Lifecycle: LiveGuard drop runs fetch_sub on live_workers when the worker exits, unwinds, or its spawn fails.
impl Drop for LiveGuard {
    // Ordering: live_workers subtracts Relaxed; the count is diagnostic.
    fn drop(&mut self) {
        self.live_workers.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Marks the worker dead as it leaves, normally or by unwinding.
struct ExitGuard {
    shared: Arc<ProbeShared>,
}

// Lifecycle: ExitGuard drop marks the map dead and calls notify only if no posted event is undelivered.
impl Drop for ExitGuard {
    // Ordering: shutdown loads Acquire, pairing with the Release store in `ForegroundProbes::drop`.
    fn drop(&mut self) {
        let post = {
            let mut map = self.shared.map.lock();
            map.dead = true;
            let post = !map.posted;
            map.posted = true;
            post
        };
        if post && !self.shared.shutdown.load(Ordering::Acquire) {
            // When: `post` and no shutdown, one event tells the event loop to drain and stop.
            let _ = (self.shared.notify)();
        }
    }
}

/// The worker: one batch per wake until the channel disconnects, shutdown, or a failed post.
fn run_worker(
    wake: &Receiver<()>,
    shared: &ProbeShared,
    sampler: &Sampler,
    stats: Option<&ForegroundWorkerStats>,
    clock: &Clock,
) {
    while wake.recv().is_ok() {
        if !probe_batch(shared, sampler, stats, clock) {
            // When: `probe_batch` returns false, shutdown or a failed post ends the worker.
            return;
        }
    }
}

/// Probe every entry that wants a sample, store each surviving result and post at most one
/// event. Returns `false` when the worker must exit.
///
/// A result survives only if its entry still exists with the identity it was taken for; the
/// newest write wins. A batch with no survivors posts nothing.
// Ordering: shutdown loads Acquire (pairs with drop's Release); probes and panes add Relaxed as statistics.
fn probe_batch(
    shared: &ProbeShared,
    sampler: &Sampler,
    stats: Option<&ForegroundWorkerStats>,
    clock: &Clock,
) -> bool {
    let stop = || shared.shutdown.load(Ordering::Acquire);
    if stop() {
        // When: `stop()` before the take, a buffered wake exits without probing.
        return false;
    }
    let batch: Vec<(u64, ProcessIdentity)> = {
        let mut map = shared.map.lock();
        map.entries
            .iter_mut()
            .filter_map(|(&pane_id, entry)| entry.want.take().map(|_| (pane_id, entry.identity)))
            .collect()
    };
    if batch.is_empty() {
        // When: `batch` is empty, every demand was cleared or retired before the take.
        return true;
    }
    // With the gate off, the only clock reads are one per probed pane.
    let started = stats.map(|_| clock());
    let identities: Vec<ProcessIdentity> = batch.iter().map(|(_, identity)| *identity).collect();
    let observations = sampler(&identities, &stop);
    if stop() {
        // When: `stop()` after the probe, the App is gone; write nothing and post nothing.
        return false;
    }
    let results: Vec<(u64, ProcessIdentity, ProbeResult)> = batch
        .into_iter()
        .zip(observations)
        .map(|((pane_id, identity), observation)| {
            let result = ProbeResult { token: identity.start, observation, sampled_at: clock() };
            (pane_id, identity, result)
        })
        .collect();
    if let (Some(stats), Some(started)) = (stats, started) {
        let finished = results.last().map_or(started, |(_, _, result)| result.sampled_at);
        stats.probes.fetch_add(1, Ordering::Relaxed);
        stats.panes.fetch_add(results.len() as u64, Ordering::Relaxed);
        stats.probe.record_us(
            u64::try_from(finished.saturating_duration_since(started).as_micros())
                .unwrap_or(u64::MAX),
        );
    }
    let post = {
        let mut map = shared.map.lock();
        let mut survivors = 0usize;
        for (pane_id, identity, result) in results {
            if let Some(entry) =
                map.entries.get_mut(&pane_id).filter(|entry| entry.identity == identity)
            {
                // The newer result replaces any older one for the same identity.
                entry.result = Some(result);
                survivors += 1;
            }
        }
        let post = survivors > 0 && !map.posted;
        map.posted |= post;
        post
    };
    // A failed post means the event loop is gone; `posted` stays set, so death posts nothing more.
    !post || stop() || (shared.notify)()
}

/// macOS: probe each pid once, re-reading its start token before and after so a pid reused
/// during the walk yields no observation.
#[cfg(target_os = "macos")]
fn sample_platform(
    identities: &[ProcessIdentity],
    stop: &dyn Fn() -> bool,
) -> Vec<Option<ForegroundProcess>> {
    use sonicterm_io::proc_info::{foreground_process_info, process_start_token};
    let mut observations = Vec::with_capacity(identities.len());
    for identity in identities {
        if stop() {
            // When: `stop()`, the App is dropping; the caller discards the partial batch.
            break;
        }
        let before = process_start_token(identity.pid);
        let observation = foreground_process_info(identity.pid);
        let after = process_start_token(identity.pid);
        let unchanged = before == Some(identity.start) && after == Some(identity.start);
        observations.push(observation.filter(|_| unchanged));
    }
    observations
}

/// Windows: one process-table snapshot for the whole batch. Each pid stays reserved by the
/// pane's retained child handle until exit is published, so no token re-read is needed.
#[cfg(windows)]
fn sample_platform(
    identities: &[ProcessIdentity],
    stop: &dyn Fn() -> bool,
) -> Vec<Option<ForegroundProcess>> {
    if stop() {
        // When: `stop()`, the App is dropping; take no snapshot.
        return Vec::new();
    }
    let pids: Vec<u32> = identities.iter().map(|identity| identity.pid).collect();
    sonicterm_io::proc_info::foreground_processes_info(&pids)
}

/// Other platforms report no identity, so no worker starts; a batch would observe nothing.
#[cfg(not(any(target_os = "macos", windows)))]
fn sample_platform(
    identities: &[ProcessIdentity],
    stop: &dyn Fn() -> bool,
) -> Vec<Option<ForegroundProcess>> {
    let _ = stop;
    identities.iter().map(|_| None).collect()
}

impl PaneState {
    /// The identity to sample and the child's exit flag; `None` without an identity.
    pub(crate) fn foreground_source(&self) -> Option<(ProcessIdentity, PtyExitObserved)> {
        #[cfg(test)]
        if let Some(source) = self.test_foreground_source.clone() {
            // When: `test_foreground_source` is set, a test stands in for the real PTY.
            return Some(source);
        }
        let pty = self.pty.as_ref()?;
        Some((pty.process_identity()?, pty.exit_observed()))
    }

    /// Cache `(now, None)` and drop the registration; returns whether a process was cleared.
    pub(crate) fn clear_foreground(&mut self, now: Instant) -> bool {
        let changed = self.fg_proc_cache.as_ref().is_some_and(|(_, process)| process.is_some());
        self.fg_proc_cache = Some((now, None));
        self.fg_probe = None;
        changed
    }
}

impl App {
    /// Apply every result the worker stored, then enter `Unavailable` if it died.
    ///
    /// A result is accepted only for a live pane still registered with the result's token and
    /// whose child is not known to have exited; others are dropped and counted stale. A changed
    /// observation or warning repaints that window's chrome; a hidden window is marked only.
    pub(super) fn drain_foreground_probe_results(&mut self, now: Instant) {
        let drained = self.fg_probes.drain();
        let mut stale = 0u64;
        let mut repaint: Vec<WindowId> = Vec::new();
        for (pane_id, result) in drained.results {
            let Some((&window_id, window)) =
                self.windows.iter_mut().find(|(_, window)| window.panes.contains_key(&pane_id))
            else {
                // When: no window holds `pane_id`, the pane closed after its sample was taken.
                stale += 1;
                continue;
            };
            let pane = window.panes.get_mut(&pane_id).expect("located pane");
            let current = pane
                .fg_probe
                .as_ref()
                .is_some_and(|registration| registration.identity.start == result.token);
            let exited = pane.foreground_source().is_some_and(|(_, exited)| exited.is_exited());
            if !current || exited {
                // When: the token no longer matches or the child exited, the sample may describe another process.
                stale += 1;
                continue;
            }
            let changed = pane.fg_proc_cache.as_ref().and_then(|(_, process)| process.as_ref())
                != result.observation.as_ref();
            pane.fg_proc_cache = Some((result.sampled_at, result.observation));
            let privileged = cached_foreground_privileged(pane);
            let mut chrome = changed;
            for (tab_idx, tab_state) in window.tab_states.iter().enumerate() {
                // A tab showing this pane takes its warning from the accepted sample.
                if tab_state.active_pane == pane_id {
                    chrome |= window.tabs.set_foreground_privileged(tab_idx, privileged);
                }
            }
            if chrome && !repaint.contains(&window_id) {
                repaint.push(window_id);
            }
        }
        self.fg_probes.note_stale(stale);
        for id in repaint {
            self.repaint_owner(id, &[RedrawCause::Chrome]);
        }
        // A dead worker's results are applied above first; demand now resolves synchronously.
        if drained.dead {
            self.fg_probes.enter_unavailable("worker stopped");
        }
        #[cfg(windows)]
        {
            let warning_active = self.foreground_warning_active();
            self.finish_foreground_process_probe(now, warning_active);
        }
        #[cfg(not(windows))]
        let _ = now;
    }

    /// Clear a pane's foreground state when its child exits, clean or not; the pane may stay.
    pub(super) fn clear_exited_foreground(&mut self, pane_id: u64, now: Instant) {
        let Some((&window_id, window)) =
            self.windows.iter_mut().find(|(_, window)| window.panes.contains_key(&pane_id))
        else {
            // When: no window holds `pane_id`, the pane is already gone.
            return;
        };
        let pane = window.panes.get_mut(&pane_id).expect("located pane");
        let mut chrome = pane.clear_foreground(now);
        for (tab_idx, tab_state) in window.tab_states.iter().enumerate() {
            // A tab showing this pane loses its warning with the exit.
            if tab_state.active_pane == pane_id {
                chrome |= window.tabs.set_foreground_privileged(tab_idx, false);
            }
        }
        if chrome {
            self.repaint_owner(window_id, &[RedrawCause::Chrome]);
        }
    }
}

/// The Windows activity wake: armed by accepted input or quiet output.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingForegroundProbe {
    /// Earliest instant at which the foreground process must be sampled again.
    pub(crate) due: Instant,
    /// Whether output activity is forbidden from postponing this deadline.
    pub(crate) fixed: bool,
}

/// The Windows sampling schedule: an activity wake and a warning wake, each consumed when due.
///
/// After every due call each stored wake is absent or strictly after `now`.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ForegroundSchedule {
    /// Input arms a fixed deadline; output arms or postpones an unfixed one.
    pub(crate) activity_wake: Option<PendingForegroundProbe>,
    /// Re-sample while a per-tab warning is shown in a non-elevated process.
    pub(crate) warning_wake: Option<Instant>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl ForegroundSchedule {
    /// Accepted input fixes a deadline one TTL out; an elevated process needs no samples.
    pub(crate) fn arm_after_input(&mut self, now: Instant, privileged: bool) {
        self.activity_wake = (!privileged)
            .then_some(PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: true });
    }

    /// Quiet output arms or postpones an unfixed deadline; it never postpones a fixed one.
    pub(crate) fn arm_after_output(&mut self, now: Instant, privileged: bool) {
        if privileged {
            // When: `privileged`, every tab already carries the global warning.
            self.activity_wake = None;
            return;
        }
        if self.activity_wake.is_some_and(|wake| wake.fixed) {
            // When: input already fixed a deadline, output cannot postpone its sample.
            return;
        }
        self.activity_wake =
            Some(PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: false });
    }

    /// The earlier of the two wakes, for the event loop's due-work fold.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let activity = self.activity_wake.map(|wake| wake.due);
        match (activity, self.warning_wake) {
            (Some(activity), Some(warning)) => Some(activity.min(warning)),
            (activity, warning) => activity.or(warning),
        }
    }

    /// Take each wake due at `now`, leaving future wakes untouched, and return the demand
    /// origin: `Activity` if an activity wake was taken, else `Warning` if a taken warning
    /// wake still applies (a warning shown and the process not privileged), else nothing.
    pub(crate) fn take_due(
        &mut self,
        now: Instant,
        warning_active: bool,
        privileged: bool,
    ) -> Option<WantOrigin> {
        let activity = self.activity_wake.take_if(|wake| wake.due <= now).is_some();
        let warning = self.warning_wake.take_if(|due| *due <= now).is_some();
        if activity {
            // When: an activity wake was taken, it covers a simultaneous warning wake too.
            return Some(WantOrigin::Activity);
        }
        (warning && warning_active && !privileged).then_some(WantOrigin::Warning)
    }

    /// Re-arm the warning wake (earliest wins) only while a warning is shown in a non-elevated
    /// process; otherwise clear it. Returns `true` when the warning wake was cleared, so the
    /// caller drops warning-only demand.
    pub(crate) fn settle_warning(
        &mut self,
        now: Instant,
        warning_active: bool,
        privileged: bool,
    ) -> bool {
        if warning_active && !privileged {
            let rearm = now + FOREGROUND_PROCESS_TTL;
            self.warning_wake = Some(self.warning_wake.map_or(rearm, |due| due.min(rearm)));
            false
        } else {
            // When: no warning is shown or the process is privileged, warning polling stops.
            self.warning_wake = None;
            true
        }
    }
}

#[cfg(test)]
#[path = "fg_probe_tests.rs"]
mod fg_probe_tests;
