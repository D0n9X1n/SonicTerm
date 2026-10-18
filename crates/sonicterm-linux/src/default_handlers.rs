//! The Linux branch of the default-handler command: shell-script MIME types through `xdg-mime`.
//!
//! Apply runs `xdg-mime default com.d0n9x1n.SonicTerm.open-script.desktop <types>` once, then
//! reads each type back with `xdg-mime query default`. Each run has no shell, happens off the
//! event-loop thread in its own process group, and is killed and reaped at a deadline. This
//! changes which program opens shell scripts (as drafts); it never chooses the terminal, and
//! it never writes `xdg-terminals.list`, `kdeglobals` or an alternative.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sonicterm_app::default_handlers::{
    DefaultHandlerBackend, HandlerItem, HandlerKind, HandlerOutcome, HandlerPlan,
    HandlerResultSink, PlanMode,
};

/// The hidden desktop entry that opens a shell script as a SonicTerm draft.
pub(crate) const OPEN_SCRIPT_DESKTOP_ID: &str = "com.d0n9x1n.SonicTerm.open-script.desktop";
/// The terminal desktop entry users list for xdg-terminal-exec themselves.
const TERMINAL_DESKTOP_ID: &str = "com.d0n9x1n.SonicTerm.desktop";
/// How long one `xdg-mime` run may take before its process group is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// The most stdout kept from one run; a read-back answer is one desktop id.
const MAX_OUTPUT_BYTES: u64 = 64 * 1024;
/// How often a running child is polled for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// The longest read-back answer quoted in a report.
const MAX_QUOTED_CHARS: usize = 80;

/// The environment the backend reads, captured once so tests can supply their own.
#[derive(Debug, Clone, Default)]
pub(crate) struct XdgEnvironment {
    /// `$XDG_DATA_HOME`.
    pub(crate) data_home: Option<OsString>,
    /// `$XDG_DATA_DIRS`.
    pub(crate) data_dirs: Option<OsString>,
    /// `$HOME`, for the default data home.
    pub(crate) home: Option<OsString>,
    /// `$PATH`, searched for `xdg-mime`.
    pub(crate) path: Option<OsString>,
}

impl XdgEnvironment {
    /// The current process's values.
    pub(crate) fn from_process() -> Self {
        Self {
            data_home: std::env::var_os("XDG_DATA_HOME"),
            data_dirs: std::env::var_os("XDG_DATA_DIRS"),
            home: std::env::var_os("HOME"),
            path: std::env::var_os("PATH"),
        }
    }
}

/// The XDG `applications` directories in lookup order: data home, then each data dir.
///
/// Unset or empty variables take their specification defaults; relative entries are ignored,
/// as the specification requires.
pub(crate) fn application_dirs(environment: &XdgEnvironment) -> Vec<PathBuf> {
    let non_empty = |value: &Option<OsString>| value.clone().filter(|value| !value.is_empty());
    let data_home = non_empty(&environment.data_home).map(PathBuf::from).or_else(|| {
        non_empty(&environment.home).map(|home| PathBuf::from(home).join(".local/share"))
    });
    let data_dirs = non_empty(&environment.data_dirs)
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
    data_home
        .into_iter()
        .chain(std::env::split_paths(&data_dirs))
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("applications"))
        .collect()
}

/// The first installed open-script entry in `dirs`.
pub(crate) fn find_desktop_entry(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(OPEN_SCRIPT_DESKTOP_ID)).find(|entry| entry.is_file())
}

/// The `MimeType` list of a desktop entry's `[Desktop Entry]` group.
pub(crate) fn mime_types_from_entry(text: &str) -> Vec<String> {
    let mut in_main_group = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
        } else if let Some(value) = line.strip_prefix("MimeType=").filter(|_| in_main_group) {
            // When: `strip_prefix` finds `MimeType=` inside the main group, that list is the claim.
            return value
                .split(';')
                .map(str::trim)
                .filter(|mime_type| !mime_type.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

/// The absolute path of executable `name` in `path`; relative `PATH` entries are skipped.
pub(crate) fn resolve_executable(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?).filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).find(
        |candidate| {
            std::fs::metadata(candidate).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        },
    )
}

/// The arguments that make the open-script entry the default for `mime_types`.
pub(crate) fn default_args(mime_types: &[String]) -> Vec<String> {
    ["default", OPEN_SCRIPT_DESKTOP_ID]
        .into_iter()
        .map(str::to_string)
        .chain(mime_types.iter().cloned())
        .collect()
}

/// The arguments that read back the default for `mime_type`.
pub(crate) fn query_args(mime_type: &str) -> Vec<String> {
    vec!["query".to_string(), "default".to_string(), mime_type.to_string()]
}

/// How one bounded run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    /// The child exited; `code` is `None` when a signal ended it.
    Exited { code: Option<i32>, stdout: String },
    /// The deadline passed; the child's process group was killed and the child reaped.
    TimedOut,
    /// The child could not be started.
    Failed(String),
}

impl RunOutcome {
    /// A short, value-free description for logs and reports.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Exited { code: Some(code), .. } => format!("exit {code}"),
            Self::Exited { code: None, .. } => "killed by a signal".to_string(),
            Self::TimedOut => "timed out".to_string(),
            Self::Failed(detail) => detail.clone(),
        }
    }
}

/// Run `program` with `args` and no shell, bounded by `timeout`.
///
/// The child leads its own process group. At the deadline the whole group is killed before
/// the child is reaped, so a helper it started cannot outlive the run.
pub(crate) fn run_bounded(program: &Path, args: &[String], timeout: Duration) -> RunOutcome {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // When: `spawn` returns `error`, no child exists to wait for or kill.
            return RunOutcome::Failed(format!("could not start ({})", error.kind()));
        }
    };
    let group = child.id();
    let (sender, receiver) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        // When: `stdout` was piped, read it on its own thread: a grandchild may hold the pipe
        // open, so the read is waited for only until the deadline.
        let _ = std::thread::Builder::new().name("sonicterm-xdg-mime-output".to_string()).spawn(
            move || {
                let mut bytes = Vec::new();
                let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut bytes);
                let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
            },
        );
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // When: `try_wait` returns `status`, the child exited and is reaped.
                break Some(status);
            }
            _ if Instant::now() >= deadline => {
                // When: `deadline` passed (or `try_wait` keeps failing), stop polling and kill.
                break None;
            }
            _ => std::thread::sleep(POLL_INTERVAL),
        }
    };
    let Some(status) = status else {
        // When: `status` is None, the deadline passed: kill the group while its leader is unreaped.
        kill_group(group);
        let _ = child.wait();
        return RunOutcome::TimedOut;
    };
    let stdout = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_default();
    RunOutcome::Exited { code: status.code(), stdout }
}

/// Send SIGKILL to the process group `group` leads.
fn kill_group(group: u32) {
    let Ok(group) = libc::pid_t::try_from(group) else {
        // When: `try_from` fails, the id is not a valid pid, so there is no group to signal.
        return;
    };
    let _ =
        // SAFETY: `kill` takes no pointer; the negative id names the group whose unreaped leader keeps it from reuse.
        unsafe { libc::kill(-group, libc::SIGKILL) };
}

/// Report `outcome` for every item of a run of `count` items.
fn report_all(sink: &HandlerResultSink, count: usize, outcome: &HandlerOutcome) {
    for index in 0..count {
        sink.report(index, outcome.clone());
    }
}

/// Make the open-script entry the default for `mime_types`, then read each one back.
///
/// Runs on a worker thread; each item reports `Changed` only when the read-back names the entry.
pub(crate) fn apply_mime_defaults(
    xdg_mime: &Path,
    mime_types: &[String],
    timeout: Duration,
    sink: &HandlerResultSink,
) {
    let set = run_bounded(xdg_mime, &default_args(mime_types), timeout);
    tracing::info!(
        target: "sonicterm_linux::default_handlers",
        status = %set.describe(),
        "xdg-mime default finished"
    );
    if !matches!(set, RunOutcome::Exited { code: Some(0), .. }) {
        // When: `matches!` rejects `set`, xdg-mime did not succeed, so no type may report changed.
        let detail = format!("xdg-mime default: {}", set.describe());
        report_all(sink, mime_types.len(), &HandlerOutcome::Failed(detail));
        return;
    }
    for (index, mime_type) in mime_types.iter().enumerate() {
        let outcome = match run_bounded(xdg_mime, &query_args(mime_type), timeout) {
            RunOutcome::Exited { code: Some(0), stdout }
                if stdout.trim() == OPEN_SCRIPT_DESKTOP_ID =>
            {
                HandlerOutcome::Changed
            }
            RunOutcome::Exited { code: Some(0), stdout } => {
                let answer: String = stdout.trim().chars().take(MAX_QUOTED_CHARS).collect();
                HandlerOutcome::Failed(format!("read back \"{answer}\""))
            }
            other => HandlerOutcome::Failed(format!("read-back: {}", other.describe())),
        };
        sink.report(index, outcome);
    }
}

/// The lines shown after the report: this is not a terminal choice, and how to make one.
pub(crate) fn terminal_instructions() -> Vec<String> {
    vec![
        "This sets the program for shell scripts, not your terminal. To use SonicTerm as the terminal, choose it yourself:".to_string(),
        "• Debian/Ubuntu: sudo update-alternatives --install /usr/bin/x-terminal-emulator x-terminal-emulator /usr/bin/sonicterm 20, then sudo update-alternatives --set x-terminal-emulator /usr/bin/sonicterm".to_string(),
        format!("• xdg-terminal-exec: add {TERMINAL_DESKTOP_ID} to ~/.config/xdg-terminals.list"),
        "• KDE: System Settings › Default Applications › Terminal Emulator".to_string(),
    ]
}

/// The `xdg-mime` backend the Linux palette command uses.
pub(crate) struct LinuxDefaultHandlers {
    environment: XdgEnvironment,
    timeout: Duration,
    /// The `xdg-mime` the last successful plan resolved; Apply runs exactly that file.
    prepared: Option<PathBuf>,
}

impl LinuxDefaultHandlers {
    /// A backend reading `environment`, bounding each `xdg-mime` run by `timeout`.
    pub(crate) fn new(environment: XdgEnvironment, timeout: Duration) -> Self {
        Self { environment, timeout, prepared: None }
    }

    /// The backend for this process's environment.
    pub(crate) fn from_process_env() -> Self {
        Self::new(XdgEnvironment::from_process(), COMMAND_TIMEOUT)
    }
}

impl DefaultHandlerBackend for LinuxDefaultHandlers {
    fn platform(&self) -> &'static str {
        "linux"
    }

    fn plan(&mut self) -> Result<HandlerPlan, String> {
        self.prepared = None;
        let Some(entry) = find_desktop_entry(&application_dirs(&self.environment)) else {
            // When: `find_desktop_entry` finds nothing, a tarball was not installed into a data dir.
            return Err(format!(
                "SonicTerm's open-script entry ({OPEN_SCRIPT_DESKTOP_ID}) is not installed in an XDG \
                 applications directory. Install the package, or see Packaging for a tarball install."
            ));
        };
        let text = std::fs::read_to_string(&entry).map_err(|error| {
            format!("Could not read SonicTerm's open-script entry ({}).", error.kind())
        })?;
        let mime_types = mime_types_from_entry(&text);
        if mime_types.is_empty() {
            // When: `mime_types` is empty, the installed entry claims no type to change.
            return Err("SonicTerm's open-script entry claims no MIME type.".to_string());
        }
        let Some(xdg_mime) = resolve_executable("xdg-mime", self.environment.path.as_deref())
        else {
            // When: `resolve_executable` finds no `xdg-mime`, nothing can record the association.
            return Err("xdg-mime not found (install xdg-utils).".to_string());
        };
        self.prepared = Some(xdg_mime);
        Ok(HandlerPlan {
            mode: PlanMode::Change,
            items: mime_types
                .into_iter()
                .map(|mime_type| HandlerItem::new(HandlerKind::MimeType, mime_type))
                .collect(),
            confirm_notes: vec![format!(
                "Desktop entry {OPEN_SCRIPT_DESKTOP_ID} (scripts open as drafts)"
            )],
            report_notes: terminal_instructions(),
        })
    }

    fn apply(&mut self, plan: &HandlerPlan, sink: HandlerResultSink) {
        let mime_types: Vec<String> = plan.items.iter().map(|item| item.name.clone()).collect();
        let Some(xdg_mime) = self.prepared.clone() else {
            // When: `prepared` is empty, no plan resolved `xdg-mime`, so nothing may run.
            let detail = HandlerOutcome::Failed("xdg-mime was not resolved".to_string());
            report_all(&sink, mime_types.len(), &detail);
            return;
        };
        let timeout = self.timeout;
        let worker_sink = sink.clone();
        let count = mime_types.len();
        let spawned = std::thread::Builder::new()
            .name("sonicterm-xdg-mime".to_string())
            .spawn(move || apply_mime_defaults(&xdg_mime, &mime_types, timeout, &worker_sink));
        if spawned.is_err() {
            // No worker exists to report, so every item fails here.
            report_all(&sink, count, &HandlerOutcome::Failed("worker did not start".to_string()));
        }
    }
}

#[cfg(test)]
#[path = "default_handlers_tests.rs"]
mod default_handlers_tests;
