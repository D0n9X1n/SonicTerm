//! `OSC 9;4` progress on the launcher entry through the `com.canonical.Unity.LauncherEntry`
//! D-Bus signal, which KDE Plasma, Dash to Dock and Ubuntu Dock show. With no listener the
//! signal has no visible effect.
//!
//! The entry is app-wide, keyed by the `.desktop` file. Signals go out from a dedicated thread,
//! started on the first report, so the event loop never blocks on D-Bus.

use std::collections::HashMap;

use sonicterm_app::taskbar_progress::{
    launcher_values, LatestValueThread, ProgressReport, ProgressSurface, TaskbarProgressBackend,
    PROGRESS_STOP_BOUND,
};

/// The application id the launcher entry is keyed by.
const APP_URI: &str = "application://com.d0n9x1n.SonicTerm.desktop";

/// The launcher entry, and the thread that signals it once a report arrived.
#[derive(Default)]
pub(crate) struct LauncherEntry {
    thread: Option<LatestValueThread<Option<ProgressReport>>>,
}

impl TaskbarProgressBackend for LauncherEntry {
    fn per_window(&self) -> bool {
        false
    }

    fn deliver(&mut self, _surface: ProgressSurface, report: Option<ProgressReport>) {
        if self.thread.is_none() {
            // The first report starts the signal thread.
            self.thread = LatestValueThread::spawn("sonicterm-launcher-progress", signal_sender())
                .inspect_err(|error| tracing::warn!(%error, "launcher progress thread unavailable"))
                .ok();
        }
        if let Some(thread) = self.thread.as_ref() {
            thread.post(report);
        }
    }

    fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            // When: thread is Some, its final clear goes out before it stops, within its bound.
            let _ = thread.stop(None, PROGRESS_STOP_BOUND);
        }
    }
}

/// The thread's sender: one session-bus connection, opened on first use and reused. A failed
/// connection is logged once and later values are dropped.
fn signal_sender() -> impl FnMut(Option<ProgressReport>) + Send + 'static {
    let mut connection: Option<zbus::Connection> = None;
    let mut failed = false;
    move |report| {
        if connection.is_none() && !failed {
            match async_io::block_on(zbus::Connection::session()) {
                Ok(opened) => connection = Some(opened),
                Err(error) => {
                    failed = true;
                    tracing::warn!(%error, "launcher progress: no session bus");
                }
            }
        }
        let Some(connection) = connection.as_ref() else {
            // When: no session bus connection exists, the launcher cannot be told.
            return;
        };
        let values = launcher_values(report);
        let mut properties: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        properties.insert("progress", values.progress.into());
        properties.insert("progress-visible", values.visible.into());
        properties.insert("urgent", values.urgent.into());
        let sent = async_io::block_on(connection.emit_signal(
            None::<zbus::names::BusName<'_>>,
            "/com/d0n9x1n/SonicTerm",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(APP_URI, properties),
        ));
        if let Err(error) = sent {
            tracing::debug!(%error, "launcher progress signal failed");
        }
    }
}
