//! Live-reload watcher for `sonicterm.toml`.
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
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
/// Handle to a running watcher thread. Drop it to stop watching (the
/// underlying [`RecommendedWatcher`] is freed and the background thread
/// shuts down on its next event poll).
#[derive(Debug, Clone)]
pub enum ConfigWatchUpdate {
    Config(Box<Config>),
    Keymap(Keymap),
}

pub struct ConfigWatcher {
    rx: Receiver<ConfigWatchUpdate>,
    // Held so the watcher stays alive; never read directly.
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Spawn a watcher for `path`. The watcher monitors the parent
    /// directory and filters for events whose path matches `path`'s
    /// basename.  Returns `Err` if the parent directory cannot be
    /// observed (e.g. it does not exist yet).
    pub fn spawn(path: PathBuf) -> Result<Self> {
        Self::spawn_with_wake(path, || {})
    }

    /// Same as [`spawn`], but invokes `wake` on the watcher thread
    /// every time a new [`Config`] is delivered down the channel.
    ///
    /// The wake callback is the channel-vs-event-loop bridge: winit's
    /// main loop sits in `ControlFlow::Wait` between events, so a
    /// `try_latest()` call only runs on the next OS-driven event
    /// (key, mouse, pty bytes, resize). If the terminal is idle when
    /// `sonicterm.toml` changes, the reload would sit queued indefinitely
    /// without an external nudge. `wake` is how the App wires its
    /// [`winit::event_loop::EventLoopProxy`] in so the loop is woken
    /// immediately on every delivery. Passing a no-op closure (as
    /// `spawn` does) keeps the watcher usable in tests/tools that
    /// have no event loop.
    pub fn spawn_with_wake<F>(path: PathBuf, wake: F) -> Result<Self>
    where
        F: Fn() + Send + 'static,
    {
        let (tx, rx) = unbounded::<ConfigWatchUpdate>();
        let watcher = spawn_inner(path, tx, Box::new(wake))?;
        Ok(Self { rx, _watcher: watcher })
    }

    /// Non-blocking poll: returns the most recent config delivered since
    /// the last call (older queued ones are drained and discarded).
    pub fn try_latest(&self) -> Option<Config> {
        let mut latest = None;
        while let Ok(update) = self.rx.try_recv() {
            if let ConfigWatchUpdate::Config(cfg) = update {
                latest = Some(*cfg);
            }
        }
        latest
    }

    /// Non-blocking poll for the newest config and newest keymap delivered
    /// since the last call. Older queued values of the same kind are drained
    /// and discarded.
    pub fn try_latest_updates(&self) -> (Option<Config>, Option<Keymap>) {
        let mut config = None;
        let mut keymap = None;
        while let Ok(update) = self.rx.try_recv() {
            match update {
                ConfigWatchUpdate::Config(cfg) => config = Some(*cfg),
                ConfigWatchUpdate::Keymap(km) => keymap = Some(km),
            }
        }
        (config, keymap)
    }

    /// Blocking receive for tests / instrumentation. Returns `None` on
    /// timeout.
    pub fn recv_timeout(&self, dur: Duration) -> Option<Config> {
        match self.rx.recv_timeout(dur).ok()? {
            ConfigWatchUpdate::Config(cfg) => Some(*cfg),
            ConfigWatchUpdate::Keymap(_) => None,
        }
    }
}

fn spawn_inner(
    path: PathBuf,
    tx: Sender<ConfigWatchUpdate>,
    wake: Box<dyn Fn() + Send + 'static>,
) -> Result<RecommendedWatcher> {
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
    let keymap_path = sonicterm_cfg::keymap::default_user_keymap_path();
    let keymap_basename = keymap_path.as_ref().and_then(|p| p.file_name().map(|s| s.to_owned()));
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
            let touches_config =
                ev.paths.iter().any(|p| p.file_name() == Some(basename_clone.as_os_str()));
            let touches_keymap = keymap_basename.as_ref().is_some_and(|name| {
                ev.paths.iter().any(|p| p.file_name() == Some(name.as_os_str()))
            });
            if !touches_config && !touches_keymap {
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
            if touches_config {
                match Config::load_or_default(&target) {
                    Ok(cfg) => {
                        let toml = match cfg.to_toml() {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        if last_sent_toml.as_deref() != Some(toml.as_str()) {
                            if tx.send(ConfigWatchUpdate::Config(Box::new(cfg))).is_err() {
                                // Receiver dropped — app is shutting down.
                                return;
                            }
                            // Wake the main event loop so the queued config
                            // is consumed promptly even when the terminal is
                            // otherwise idle (winit's ControlFlow::Wait would
                            // otherwise hold the main thread until the next
                            // OS event).
                            wake();
                            last_sent_toml = Some(toml);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("sonicterm.toml reload failed: {e:#}");
                    }
                }
            }
            if touches_keymap {
                if let Some(path) = keymap_path.as_ref() {
                    match Keymap::load(path) {
                        Ok(km) => {
                            if tx.send(ConfigWatchUpdate::Keymap(km)).is_err() {
                                return;
                            }
                            wake();
                        }
                        Err(e) => tracing::warn!("keymap.toml reload failed: {e:#}"),
                    }
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
