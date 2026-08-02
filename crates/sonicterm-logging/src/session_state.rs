//! Whether the previous session ended on purpose.
//!
//! A process killed with `SIGKILL` or `TerminateProcess` runs no cleanup code
//! and writes no dump. Nothing SonicTerm does can change that, and this module
//! does not pretend otherwise. What it can do is leave evidence *before* the
//! kill, so the next launch can tell that a session ended without ever
//! reaching its own shutdown path.
//!
//! The mechanism is deliberately small: a marker file written at startup and
//! removed at a clean exit. A marker still present on the next launch means
//! the session that wrote it never got to remove it.
//!
//! ## What a stale marker does and does not prove
//!
//! It proves the process did not reach its shutdown path. It says **nothing**
//! about why — `SIGKILL`, a power loss, an OOM kill, and a hard reset are
//! indistinguishable here, and inferring one from a stale marker would be
//! inventing a cause. Classification, where it is possible at all, comes from
//! the artifacts a catchable failure leaves behind; see [`crate::postmortem`].
//!
//! ## Concurrent sessions
//!
//! Several SonicTerm instances can run at once, so markers are per-session
//! rather than one shared file — a single file would be overwritten by the
//! second instance and would report the first as clean when it was not.
//!
//! A marker belonging to a process that is *still alive* is not stale, so
//! liveness is checked before a marker is reported. Without that check, every
//! launch would report every concurrently-running sibling as an unclean exit.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

/// Distinguishes sessions armed by one process within the same millisecond.
///
/// The timestamp resolves to milliseconds and the pid is constant inside a
/// process, so those two alone are not a unique id — they collide on any pair
/// of `arm` calls close enough together, and the second marker overwrites the
/// first.
static ARM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// What a marker claims about the session that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The session was running when the marker was last written.
    Armed,
    /// The session reached its shutdown path and said so.
    Clean,
}

impl SessionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Clean => "clean",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "armed" => Some(Self::Armed),
            "clean" => Some(Self::Clean),
            _ => None,
        }
    }
}

/// The identity a marker records.
///
/// Deliberately narrow. Everything here is about the *process*, not about what
/// the user did with it: no shell, no command, no environment, no window or
/// tab titles, no paths the user opened. A postmortem record that leaked
/// session content would be a privacy defect shipped in the name of
/// diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMarker {
    /// Unique per launch. Carries a timestamp and the pid, nothing else.
    pub id: String,
    /// The process that wrote it, for the liveness check.
    pub pid: u32,
    /// SonicTerm version, so a marker read after an upgrade is attributable.
    pub version: String,
    /// Target platform string.
    pub platform: String,
    /// RFC 3339 UTC start time.
    pub started_at: String,
    /// What the session last claimed about itself.
    pub state: SessionState,
}

impl SessionMarker {
    fn render(&self) -> String {
        format!(
            "id={}\npid={}\nversion={}\nplatform={}\nstarted_at={}\nstate={}\n",
            self.id,
            self.pid,
            self.version,
            self.platform,
            self.started_at,
            self.state.as_str(),
        )
    }

    /// Parse a marker, or `None` if any required field is missing or unusable.
    ///
    /// Returning `None` rather than a partially-filled marker is what makes a
    /// truncated file classify as [`PriorSession::Corrupt`]. A marker half
    /// written when the power failed is real evidence of an unclean exit, and
    /// silently treating it as absent would discard exactly the case it was
    /// written for.
    fn parse(text: &str) -> Option<Self> {
        let mut id = None;
        let mut pid = None;
        let mut version = None;
        let mut platform = None;
        let mut started_at = None;
        let mut state = None;

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "id" => id = Some(value.to_string()),
                "pid" => pid = value.parse::<u32>().ok(),
                "version" => version = Some(value.to_string()),
                "platform" => platform = Some(value.to_string()),
                "started_at" => started_at = Some(value.to_string()),
                "state" => state = SessionState::parse(value),
                _ => {}
            }
        }

        let id = id?;
        let version = version?;
        let platform = platform?;
        let started_at = started_at?;
        if !valid_session_id(&id)
            || !valid_version(&version)
            || !valid_platform(&platform)
            || chrono::DateTime::parse_from_rfc3339(&started_at).is_err()
        {
            return None;
        }

        Some(Self { id, pid: pid?, version, platform, started_at, state: state? })
    }
}

/// How a previous session's marker reads on this launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorSession {
    /// The session recorded its own clean shutdown.
    CleanExit(SessionMarker),
    /// The session never reached its shutdown path.
    ///
    /// The cause is deliberately absent. A stale marker distinguishes "did not
    /// finish" from "finished"; it cannot distinguish a `SIGKILL` from a power
    /// cut, and naming one would be a guess presented as a finding.
    Unclean(SessionMarker),
    /// The marker could not be read or parsed.
    ///
    /// Still evidence of an unclean exit rather than a reason to stay silent:
    /// a marker truncated mid-write is what a kill during startup looks like.
    Corrupt {
        /// Where the unreadable marker sits, so a bug report can include it.
        path: PathBuf,
    },
}

impl fmt::Display for PriorSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CleanExit(marker) => {
                write!(f, "session {} exited cleanly", marker.id)
            }
            Self::Unclean(marker) => write!(
                f,
                "session {} (pid {}, version {}, started {}) did not reach its shutdown path; \
                 the cause is not recorded",
                marker.id, marker.pid, marker.version, marker.started_at
            ),
            Self::Corrupt { path } => {
                write!(f, "a session marker at {} could not be parsed", path.display())
            }
        }
    }
}

/// A marker held for the life of the process.
///
/// Not a `Drop` guard, and deliberately so. `Drop` runs on an unwind and on a
/// normal return but not on a kill, which would make a clean mark appear on
/// some paths that are not clean — a panic that unwinds out of `main` would
/// erase the evidence it exists to leave. Marking clean is an explicit call on
/// the shutdown path instead.
#[derive(Debug)]
pub struct ArmedSession {
    path: PathBuf,
    marker: SessionMarker,
}

impl ArmedSession {
    /// This session's id, for tagging artifacts written later.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.marker.id
    }

    /// Where the marker lives.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record that this session shut down on purpose.
    ///
    /// Removes the marker. If removal fails — a read-only directory, a file
    /// held open by a scanner — the marker is rewritten with `state=clean` so
    /// the next launch still reads a clean exit rather than a false unclean
    /// one. A diagnostic that reports a crash that did not happen trains its
    /// reader to ignore it.
    ///
    /// # Errors
    ///
    /// Returns the error from the fallback write when neither removal nor
    /// rewriting succeeds.
    pub fn mark_clean(self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(_) => {
                let mut marker = self.marker;
                marker.state = SessionState::Clean;
                write_atomically(&self.path, &marker.render())
            }
        }
    }
}

/// Where markers live, given the log directory.
#[must_use]
pub fn session_dir(log_dir: &Path) -> PathBuf {
    log_dir.join("sessions")
}

/// Write a marker for this process and return the handle that clears it.
///
/// Call once at startup, as early as the log directory is known. A session
/// killed before this runs leaves no marker and is indistinguishable from one
/// that never started — which is correct, because nothing observed it.
///
/// # Errors
///
/// Returns an [`io::Error`] when the session directory cannot be created or
/// the marker cannot be written.
pub fn arm(log_dir: &Path, version: &str) -> io::Result<ArmedSession> {
    if !valid_version(version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "version must be a bounded release-style ASCII identifier",
        ));
    }

    let dir = session_dir(log_dir);
    std::fs::create_dir_all(&dir)?;

    let pid = std::process::id();
    let now = chrono::Utc::now();
    // Timestamp, pid, and a process-local counter. The counter is what makes
    // the id unique rather than merely probable: the timestamp has millisecond
    // resolution and the pid is constant within a process, so two sessions
    // armed in the same millisecond by one process produced the same id and
    // the second marker overwrote the first — losing the evidence the first
    // one existed to leave.
    let sequence = ARM_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let id = format!("{}-{pid}-{sequence}", now.format("%Y%m%dT%H%M%S%.3fZ"));
    let marker = SessionMarker {
        id: id.clone(),
        pid,
        version: version.to_string(),
        platform: std::env::consts::OS.to_string(),
        started_at: now.to_rfc3339(),
        state: SessionState::Armed,
    };

    let path = dir.join(format!("session-{id}.marker"));
    write_atomically(&path, &marker.render())?;
    Ok(ArmedSession { path, marker })
}

fn valid_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_digit()
        && bytes.iter().filter(|byte| **byte == b'.').count() >= 2
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-' | b'+'))
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_platform(platform: &str) -> bool {
    !platform.is_empty()
        && platform.len() <= 32
        && platform
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Write via a temporary file and rename into place.
///
/// A kill partway through a direct write leaves a truncated marker, which the
/// next launch would have to classify as corrupt. Renaming an already-complete
/// file means the marker at `path` is either absent or whole.
fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, contents)?;
    match crate::path::replace_file(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Read every marker left by a session other than this one.
///
/// Markers whose owning process is still alive are omitted: a concurrently
/// running sibling has not exited at all, and reporting it as an unclean exit
/// would make every second instance look like a crash.
///
/// `current` is this session's id, so the marker just armed is not read back
/// as a finding.
#[must_use]
pub fn scan_prior_sessions(log_dir: &Path, current: Option<&str>) -> Vec<PriorSession> {
    let dir = session_dir(log_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "tmp") {
            if temp_marker_pid(&path).is_some_and(process_is_alive) {
                continue;
            }
            found.push(PriorSession::Corrupt { path });
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "marker") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            found.push(PriorSession::Corrupt { path });
            continue;
        };
        let Some(marker) = SessionMarker::parse(&text) else {
            found.push(PriorSession::Corrupt { path });
            continue;
        };
        if current.is_some_and(|current| current == marker.id) {
            continue;
        }
        if marker.state == SessionState::Armed && process_is_alive(marker.pid) {
            // Running right now, so it has not exited at all.
            continue;
        }
        found.push(match marker.state {
            SessionState::Clean => PriorSession::CleanExit(marker),
            SessionState::Armed => PriorSession::Unclean(marker),
        });
    }
    found
}

fn temp_marker_pid(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.strip_prefix("session-")?;
    let (without_sequence, _) = id.rsplit_once('-')?;
    let (_, pid) = without_sequence.rsplit_once('-')?;
    pid.parse().ok()
}

/// Delete a marker that has been reported, so it is reported once.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the file cannot be removed.
pub fn clear(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

/// Is a process with this id running?
///
/// **PID reuse is a real and unavoidable caveat.** An operating system may
/// reassign the id of an exited process to a new one, in which case a stale
/// marker reads as live and its session goes unreported. The alternative —
/// omitting the check — misreports every concurrently running instance as a
/// crash on every launch, which is both more frequent and more misleading.
/// Under-reporting a rare case is the cheaper error.
#[cfg(unix)]
#[must_use]
fn process_is_alive(pid: u32) -> bool {
    // Guarded before the call, not merely validated. `kill(0, sig)` addresses
    // the *caller's entire process group* rather than a process with id zero,
    // so passing a zero through would return success and report any marker
    // carrying it as a live session — the marker would then never be reported,
    // which is the exact failure this whole module exists to avoid. No real
    // process ever has id 0 on the platforms SonicTerm ships on, so a zero
    // here means a corrupt or synthetic marker: absent, not alive.
    if pid == 0 {
        return false;
    }
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering anything. No memory is touched.
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    // EPERM means the process exists but belongs to another user, which is
    // still alive for this purpose. Only ESRCH means genuinely absent.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows: open a handle with the narrowest right that proves existence.
///
/// Carries the same PID-reuse caveat as the Unix path, plus one of its own: a
/// handle can be opened for a process that has exited while another handle to
/// it remains open, so this can read as alive slightly past the exit.
#[cfg(windows)]
#[must_use]
fn process_is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // Guarded for the same reason as the Unix path: no real process carries
    // id 0, so a zero means a corrupt or synthetic marker. Treating it as
    // absent keeps the two platforms classifying such a marker identically.
    if pid == 0 {
        return false;
    }

    // SAFETY: `OpenProcess` returns either a valid handle or an error; the
    // handle is closed on every path that obtains one.
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

/// Platforms with no liveness check treat every marker as its state claims.
///
/// Reports a concurrently running session as unclean rather than hiding a real
/// one. SonicTerm ships on macOS and Windows, so this arm exists to keep the
/// crate building rather than to be correct on a platform nobody targets.
#[cfg(not(any(unix, windows)))]
#[must_use]
fn process_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
#[path = "session_state_tests.rs"]
mod session_state_tests;
