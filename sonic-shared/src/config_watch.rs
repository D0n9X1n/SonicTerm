//! Live-reload watcher for `sonic.toml`.
//!
//! Spawns a background thread that uses [`notify::RecommendedWatcher`] to
//! observe the parent directory of the config file (editors often
//! delete-then-rename on save, so watching the file itself misses the new
//! inode). When an event arrives for the target basename, the file is
//! re-parsed and the resulting [`Config`] is delivered through a
//! [`crossbeam_channel::Receiver`] for the main thread to consume on its
//! next redraw tick.
//!
//! Errors during parse are logged via `tracing::warn!` and the channel
//! stays open with the previous config still in effect — a malformed save
//! never crashes the running terminal.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sonic_core::config::Config;

/// Handle to a running watcher thread. Drop it to stop watching (the
/// underlying [`RecommendedWatcher`] is freed and the background thread
/// shuts down on its next event poll).
pub struct ConfigWatcher {
    rx: Receiver<Config>,
    // Held so the watcher stays alive; never read directly.
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Spawn a watcher for `path`. The watcher monitors the parent
    /// directory and filters for events whose path matches `path`'s
    /// basename.  Returns `Err` if the parent directory cannot be
    /// observed (e.g. it does not exist yet).
    pub fn spawn(path: PathBuf) -> Result<Self> {
        let (tx, rx) = unbounded::<Config>();
        let watcher = spawn_inner(path, tx)?;
        Ok(Self { rx, _watcher: watcher })
    }

    /// Non-blocking poll: returns the most recent config delivered since
    /// the last call (older queued ones are drained and discarded).
    pub fn try_latest(&self) -> Option<Config> {
        let mut latest = None;
        while let Ok(cfg) = self.rx.try_recv() {
            latest = Some(cfg);
        }
        latest
    }

    /// Blocking receive for tests / instrumentation. Returns `None` on
    /// timeout.
    pub fn recv_timeout(&self, dur: Duration) -> Option<Config> {
        self.rx.recv_timeout(dur).ok()
    }
}

fn spawn_inner(path: PathBuf, tx: Sender<Config>) -> Result<RecommendedWatcher> {
    let parent: PathBuf = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("config path has no parent dir: {path:?}"))?;
    let basename = path
        .file_name()
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("config path has no basename: {path:?}"))?;

    // Make sure the directory exists; some test scenarios pass a path
    // that won't exist yet, so we create it lazily.
    std::fs::create_dir_all(&parent).ok();

    let target = path.clone();
    let basename_clone = basename.clone();
    // notify's event channel is std::sync::mpsc — we forward into a
    // crossbeam channel after filtering + parsing.
    let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<notify::Event>>();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // The send only fails if the receiver dropped (i.e. the
            // forwarder thread has exited), which already means we're
            // shutting down — nothing to do.
            let _ = raw_tx.send(res);
        })?;
    watcher.watch(&parent, RecursiveMode::NonRecursive)?;

    std::thread::Builder::new().name("sonic-config-watch".into()).spawn(move || {
        // Coalesce bursts: editors often emit Remove + Create +
        // Modify in rapid succession. We wait briefly for the dust
        // to settle, then re-parse once.
        const SETTLE: Duration = Duration::from_millis(80);
        // De-dup: skip deliveries that round-trip to the same TOML
        // as the previous one. Suppresses the "FSEvents replays
        // pre-watch writes" case (macOS) and the redundant Modify
        // bursts editors emit on save.
        let mut last_sent_toml: Option<String> = None;
        for event in raw_rx.iter() {
            let Ok(ev) = event else { continue };
            if !is_interesting(&ev.kind) {
                continue;
            }
            if !ev.paths.iter().any(|p| p.file_name() == Some(basename_clone.as_os_str())) {
                continue;
            }
            // Drain any further events that arrive within SETTLE so
            // we don't reload three times per save.
            let deadline = std::time::Instant::now() + SETTLE;
            while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                match raw_rx.recv_timeout(remaining) {
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            match Config::load_or_default(&target) {
                Ok(cfg) => {
                    let toml = match cfg.to_toml() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if last_sent_toml.as_deref() == Some(toml.as_str()) {
                        continue;
                    }
                    if tx.send(cfg).is_err() {
                        // Receiver dropped — app is shutting down.
                        return;
                    }
                    last_sent_toml = Some(toml);
                }
                Err(e) => {
                    tracing::warn!("sonic.toml reload failed: {e:#}");
                }
            }
        }
    })?;

    Ok(watcher)
}

fn is_interesting(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}
