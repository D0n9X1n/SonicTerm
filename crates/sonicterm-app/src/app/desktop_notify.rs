//! Desktop notifications off the event-loop thread: one worker sends one request at a time to the
//! platform's notification service, each bounded in time, so a stuck service can neither block
//! the app nor pile up native calls.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

/// How many requests may wait for the worker; a full queue drops the newest.
const QUEUE_CAPACITY: usize = 4;

/// How long one native request may take before the worker stops waiting for it.
pub(crate) const REQUEST_BOUND: Duration = Duration::from_secs(5);

/// One desktop notification: sanitized text only, and the identifier a later one replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopNotice {
    pub(crate) title: String,
    pub(crate) body: String,
    /// `sonicterm-<session>-<pane>-<id>` for an OSC 99 id, so a later one replaces it.
    pub(crate) native_id: Option<String>,
}

/// How one native request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryOutcome {
    Delivered,
    /// The service is missing or refused permission; later requests may still succeed.
    Unavailable(&'static str),
    /// The request failed or timed out; Linux and Windows report it, macOS only refusals.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Failed(&'static str),
}

/// A platform notification service the worker drives.
pub(crate) trait NotifyBackend: Send + 'static {
    /// Deliver `notice` and call `done` once, when the native call ends, from any thread.
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>);

    /// Whether a call that outlived its bound is still running and must finish before another
    /// is sent; a backend that cancels its own late call answers `false`.
    fn quarantines(&self) -> bool;
}

/// The app's handle to the notification worker. Dropping it ends the worker once it is idle; it
/// is never joined, so a stuck native call cannot hold up shutdown.
pub(crate) struct DesktopNotifier {
    queue: SyncSender<DesktopNotice>,
    dropped: u64,
}

impl DesktopNotifier {
    /// Start a worker that sends to `backend`, waiting at most `bound` for each request.
    pub(crate) fn spawn(backend: impl NotifyBackend, bound: Duration) -> Self {
        let (queue, requests) = mpsc::sync_channel(QUEUE_CAPACITY);
        let spawned = std::thread::Builder::new()
            .name("sonicterm-desktop-notify".into())
            .spawn(move || run_worker(backend, requests, bound));
        // A worker that could not start drops every request; notifications stay in-app.
        if let Err(error) = spawned {
            tracing::warn!(target: "sonicterm_app::app", %error, "desktop notification worker did not start");
        }
        Self { queue, dropped: 0 }
    }

    /// Queue `notice`; a full queue or a stopped worker drops it, which is counted and logged by
    /// size only.
    pub(crate) fn send(&mut self, notice: DesktopNotice) {
        let sizes = (notice.title.len(), notice.body.len());
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
            self.queue.try_send(notice)
        {
            self.dropped += 1;
            tracing::debug!(target: "sonicterm_app::app", title_len = sizes.0, body_len = sizes.1, dropped = self.dropped, "desktop notification dropped");
        }
    }

    /// Requests dropped because the queue was full or the worker had stopped.
    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Send each request in turn, waiting at most `bound` for its native call. A call past its bound
/// on a quarantining backend blocks further sends until it finishes; requests meanwhile are
/// dropped, since the in-app notification already showed them.
fn run_worker(mut backend: impl NotifyBackend, requests: Receiver<DesktopNotice>, bound: Duration) {
    let mut late: Option<Receiver<DeliveryOutcome>> = None;
    while let Ok(notice) = requests.recv() {
        if let Some(outstanding) = &late {
            // When: a `late` call timed out earlier, it must finish before another is sent.
            match outstanding.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {
                    // When: `try_recv` finds the call still running, this request is dropped.
                    tracing::debug!(target: "sonicterm_app::app", "desktop notification skipped: a request is still running");
                    continue;
                }
                Ok(_) | Err(mpsc::TryRecvError::Disconnected) => late = None,
            }
        }
        let sizes = (notice.title.len(), notice.body.len());
        let (done_sender, done) = mpsc::sync_channel(1);
        backend.deliver(
            notice,
            Box::new(move |outcome| {
                // A worker that stopped waiting has nothing left to tell.
                let _ = done_sender.try_send(outcome);
            }),
        );
        match done.recv_timeout(bound) {
            Ok(outcome) => {
                tracing::debug!(target: "sonicterm_app::app", title_len = sizes.0, body_len = sizes.1, ?outcome, "desktop notification sent");
            }
            // An uncancellable call that outlived its bound holds every later request until it ends.
            Err(RecvTimeoutError::Timeout) if backend.quarantines() => {
                tracing::debug!(target: "sonicterm_app::app", "desktop notification timed out; holding further requests");
                late = Some(done);
            }
            Err(_) => {
                tracing::debug!(target: "sonicterm_app::app", "desktop notification timed out");
            }
        }
    }
}

/// The app's one notification worker, started on the first desktop notification; `None` where
/// this platform has no usable service. Unit tests never start one, so they post nothing.
static NOTIFIER: std::sync::Mutex<Option<Option<DesktopNotifier>>> = std::sync::Mutex::new(None);

/// Send `notice` to the desktop, starting the worker on first use.
pub(crate) fn send_desktop(notice: DesktopNotice) {
    let Ok(mut notifier) = NOTIFIER.lock() else {
        // When: the lock was poisoned by a panic, desktop notifications stop; in-app ones go on.
        return;
    };
    // Unit tests never start a platform service, so they post nothing.
    let notifier = notifier.get_or_insert_with(|| platform_notifier().filter(|_| !cfg!(test)));
    if let Some(notifier) = notifier.as_mut() {
        notifier.send(notice);
    }
}

/// The native identifier for OSC 99 id `id` from `pane_id`: a per-process nonce keeps two
/// SonicTerm processes, whose pane ids both start at 1, from replacing each other's notifications.
pub(crate) fn native_notice_id(pane_id: u64, id: &str) -> String {
    static SESSION: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let session = SESSION.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.finish()
    });
    format!("sonicterm-{session:016x}-{pane_id}-{id}")
}

/// `text` with `&`, `<` and `>` escaped, for a notification server that reads bodies as markup.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(target_os = "linux")]
#[path = "desktop_notify_linux.rs"]
mod linux;
#[cfg(target_os = "macos")]
#[path = "desktop_notify_macos.rs"]
mod macos;
#[cfg(target_os = "windows")]
#[path = "desktop_notify_windows.rs"]
mod windows;

/// This platform's notification service, or `None` where none can be used (an unbundled macOS
/// process), which leaves notifications in-app.
pub(crate) fn platform_notifier() -> Option<DesktopNotifier> {
    #[cfg(target_os = "macos")]
    let backend = macos::UserNotificationCenter::new()?;
    #[cfg(target_os = "linux")]
    let backend = linux::DesktopPortal::new(REQUEST_BOUND);
    #[cfg(target_os = "windows")]
    let backend = windows::WindowsToasts;
    Some(DesktopNotifier::spawn(backend, REQUEST_BOUND))
}

#[cfg(test)]
#[path = "desktop_notify_tests.rs"]
mod desktop_notify_tests;
