//! What can be learned about a session that did not end on purpose.
//!
//! The honest boundary comes first, because everything here depends on it:
//!
//! **After `SIGKILL` or `TerminateProcess`, SonicTerm writes nothing.** The
//! process is gone before any handler runs. No dump, no final log line, no
//! flush. Any feature claiming to capture a memory dump for those cases is
//! claiming something the operating system does not permit, and this module
//! never makes that claim — a report for a hard kill says explicitly that no
//! process-written dump exists, and offers what does exist instead.
//!
//! So evidence comes from three places, in descending order of how much they
//! can tell you:
//!
//! 1. **Artifacts SonicTerm wrote itself.** A Rust panic produces a
//!    session-tagged file with a backtrace and the tail of the log ring.
//!    Compatible artifacts already present in the crash directory are still
//!    classified by content.
//! 2. **The session marker** ([`crate::session_state`]), which survives any
//!    kill because it is written *before* the failure. It proves a session did
//!    not reach its shutdown path, and deliberately infers nothing about why.
//! 3. **Operating-system postmortem records**, when the OS wrote any. On Unix,
//!    the fatal-signal path first appends a fixed marker to the active log and
//!    re-raises the signal so the operating system can produce its record.
//!
//! ## On not claiming unrelated files
//!
//! Discovery matches conservatively — a report naming a file SonicTerm did not
//! produce would send someone reading a stranger's crash log while looking for
//! their own. Where a match is by filename convention rather than by
//! provenance, the report says so rather than implying certainty.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::session_state::{self, PriorSession};

/// What kind of failure an artifact SonicTerm wrote describes.
///
/// Only failures where the process still had control can be classified at all.
/// Everything else is [`Self::Unknown`] rather than a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A Rust panic on any thread.
    Panic,
    /// A fatal signal the handler caught before re-raising.
    FatalSignal,
    /// Allocator failure, which reaches the log via `SIGABRT`.
    AllocFailure,
    /// An artifact whose content names no recognised failure.
    Unknown,
}

impl ArtifactKind {
    /// Classify from dump contents.
    ///
    /// Reads what the artifact says rather than inferring from its name: the
    /// filename is a timestamp, which carries no failure information at all.
    #[must_use]
    fn classify(contents: &str) -> Self {
        if let Some(classification) =
            contents.lines().find_map(|line| line.strip_prefix("classification:").map(str::trim))
        {
            // When: the dump carries an explicit classification header, which
            // states the failure kind rather than leaving it inferred.
            return match classification {
                "panic" => Self::Panic,
                "fatal_signal" => Self::FatalSignal,
                "alloc_failure" => Self::AllocFailure,
                _ => Self::Unknown,
            };
        }
        if contents.contains("FATAL: SIG") || contents.contains("after fatal signal") {
            Self::FatalSignal
        } else if contents.contains("allocator failure") {
            // When: contents names an allocator failure, which reaches the log
            // through SIGABRT rather than through the panic hook.
            Self::AllocFailure
        } else if contents.contains("panic") || contents.contains("== sonic crash dump ==") {
            // When: contents carries panic text or the crash-dump banner, so
            // the process still had control when it wrote this.
            Self::Panic
        } else {
            // When: contents matches no marker any writer emits, so the failure
            // stays unrecognised rather than guessed at.
            Self::Unknown
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::FatalSignal => "fatal signal",
            Self::AllocFailure => "allocator failure",
            Self::Unknown => "unrecognised",
        }
    }
}

/// One artifact SonicTerm wrote for a catchable failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashArtifact {
    /// Where it is, so a bug report can attach it.
    pub path: PathBuf,
    /// What kind of failure it describes.
    pub kind: ArtifactKind,
    /// The session it belongs to, when the artifact records one.
    ///
    /// `None` for an artifact written before session tagging existed, or one
    /// whose header could not be read. Absent rather than guessed: attributing
    /// a dump to the wrong session sends a reader to the wrong log window.
    pub session_id: Option<String>,
}

/// Persisted breadcrumbs written for one session before it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreadcrumbEvidence {
    /// Where the bounded breadcrumb snapshot is stored.
    pub path: PathBuf,
    /// The exact session identity encoded by the file name.
    pub session_id: String,
}

/// A postmortem record the operating system wrote.
///
/// Distinct from [`CrashArtifact`], which SonicTerm wrote itself. The
/// distinction matters to a reader: an OS record can exist for failures
/// SonicTerm never saw, and can be absent for failures it handled cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsEvidence {
    /// Where the record is.
    pub path: PathBuf,
    /// How confident the match is, stated rather than implied.
    pub attribution: Attribution,
    /// Which standard operating-system store supplied the candidate.
    pub source: OsEvidenceSource,
}

/// The standard operating-system store that supplied an evidence candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsEvidenceSource {
    /// The current macOS user's `DiagnosticReports` directory.
    MacUserDiagnosticReports,
    /// The machine-wide macOS `DiagnosticReports` directory.
    MacSystemDiagnosticReports,
    /// Windows Error Reporting's pending report queue.
    WindowsWerQueue,
    /// Windows Error Reporting's archived report store.
    WindowsWerArchive,
    /// Windows LocalDumps under `%LOCALAPPDATA%\CrashDumps`.
    WindowsLocalDumps,
}

/// How a discovered OS record was matched to SonicTerm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// The filename follows the platform's convention for this application.
    ///
    /// Not proof. Conventions collide, and a user may have unrelated files in
    /// the same directory, so a report built on this says how it matched.
    ByName,
}

/// A limitation that changes how OS evidence should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostmortemNote {
    /// Windows WER and LocalDumps paths were inspected, but their registry
    /// configuration was not.
    WerRegistryConfigurationNotInspected,
}

impl fmt::Display for PostmortemNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WerRegistryConfigurationNotInspected => f.write_str(
                "Windows WER registry configuration was not inspected; only standard filesystem \
                 locations were checked",
            ),
        }
    }
}

/// Everything known about one prior session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostmortemReport {
    /// How the session ended, as far as the marker can establish.
    pub session: PriorSession,
    /// Artifacts SonicTerm wrote for that session.
    pub artifacts: Vec<CrashArtifact>,
    /// Bounded breadcrumbs written for the exact session identity.
    pub breadcrumbs: Vec<BreadcrumbEvidence>,
    /// OS records that appear related.
    pub os_evidence: Vec<OsEvidence>,
    /// Limitations that qualify the evidence search.
    pub notes: Vec<PostmortemNote>,
}

impl PostmortemReport {
    /// Did the prior session fail to reach its shutdown path?
    #[must_use]
    pub fn is_unclean(&self) -> bool {
        matches!(self.session, PriorSession::Unclean(_) | PriorSession::Corrupt { .. })
    }

    /// Whether SonicTerm itself wrote anything about this failure.
    ///
    /// False for every uncatchable termination, which is the case the report
    /// must describe honestly rather than leave a reader inferring.
    #[must_use]
    pub fn has_process_written_dump(&self) -> bool {
        !self.artifacts.is_empty()
    }
}

impl fmt::Display for PostmortemReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.session)?;

        if self.has_process_written_dump() {
            for artifact in &self.artifacts {
                write!(
                    f,
                    "; SonicTerm wrote a {} artifact at {}",
                    artifact.kind.as_str(),
                    artifact.path.display()
                )?;
            }
        } else if self.is_unclean() {
            // When: is_unclean holds with no artifact written, so the report
            // must say the dump does not exist rather than leave it unsaid.

            // A hard kill destroys the process before any handler runs, so
            // there is no dump to find — and a reader who is not told that
            // will keep looking for one, or worse, conclude the dump was lost.
            write!(
                f,
                "; no process-written memory dump exists for this session. SonicTerm cannot \
                 write one after an uncatchable termination such as SIGKILL or \
                 TerminateProcess, because the process is destroyed before any handler runs. \
                 The evidence below is what survives"
            )?;
        }

        if matches!(self.session, PriorSession::Corrupt { .. }) {
            write!(f, "; unclean session details unavailable")?;
        }

        for breadcrumb in &self.breadcrumbs {
            write!(
                f,
                "; breadcrumb evidence for session {} is at {}",
                breadcrumb.session_id,
                breadcrumb.path.display()
            )?;
        }

        if self.os_evidence.is_empty() {
            write!(f, "; no operating-system postmortem records found")?;
        } else {
            // When: os_evidence holds candidates, each is named with the
            // qualifier that it matched by convention, not by provenance.
            for evidence in &self.os_evidence {
                write!(
                    f,
                    "; an operating-system record at {} matches by filename convention and may \
                     relate to this session",
                    evidence.path.display()
                )?;
            }
        }
        for note in &self.notes {
            write!(f, "; {note}")?;
        }
        Ok(())
    }
}

/// Build a report for every prior session found under `log_dir`.
///
/// `current` is this session's id, excluded so a launch does not report
/// itself.
#[must_use]
pub fn collect(log_dir: &Path, current: Option<&str>) -> Vec<PostmortemReport> {
    let artifacts = discover_artifacts(&crate::path::crash_dir_in(log_dir));
    let platform = discover_os_evidence();

    session_state::scan_prior_sessions(log_dir, current)
        .into_iter()
        .map(|session| {
            let session_id = match &session {
                PriorSession::CleanExit(marker) | PriorSession::Unclean(marker) => {
                    Some(marker.id.clone())
                }
                PriorSession::Corrupt { .. } => None,
            };
            let matched = artifacts
                .iter()
                .filter(|artifact| match (&artifact.session_id, &session_id) {
                    (Some(artifact_id), Some(session_id)) => artifact_id == session_id,
                    _ => false,
                })
                .cloned()
                .collect();
            let breadcrumbs = session_id
                .as_deref()
                .and_then(|id| breadcrumb_evidence(log_dir, id))
                .into_iter()
                .collect();
            PostmortemReport {
                session,
                artifacts: matched,
                breadcrumbs,
                os_evidence: platform.evidence.clone(),
                notes: platform.notes.clone(),
            }
        })
        .collect()
}

/// Log what the previous session left behind, then clear its marker.
///
/// Called once at startup after the logger exists. Reports at WARN for an
/// unclean exit — a user whose terminal vanished is owed the finding in the
/// log they already have, not one they would have had if they had raised the
/// level first — and clears the marker so the same session is reported once
/// rather than on every launch forever.
pub fn report_prior_sessions(log_dir: &Path, current: Option<&str>) {
    for report in collect(log_dir, current) {
        if report.is_unclean() {
            tracing::warn!(
                target: "sonic_exit",
                process_written_dump = report.has_process_written_dump(),
                artifacts = report.artifacts.len(),
                breadcrumbs = report.breadcrumbs.len(),
                os_records = report.os_evidence.len(),
                "{report}"
            );
        } else {
            // When: report.is_unclean is false, the prior session recorded its
            // own shutdown, so the finding goes to debug rather than warn.
            tracing::debug!(target: "sonic_exit", "{report}");
        }

        match &report.session {
            PriorSession::CleanExit(marker) | PriorSession::Unclean(marker) => {
                // When: the marker identifies its session, so its file is
                // removed and the finding is reported once, not every launch.
                let path = session_state::session_dir(log_dir)
                    .join(format!("session-{}.marker", marker.id));
                let _ = session_state::clear(&path);
            }
            PriorSession::Corrupt { path } => {
                // When: the marker is Corrupt, the unreadable file at path is
                // cleared too, so it is not re-reported forever.
                let _ = session_state::clear(path);
            }
        }
    }
}

/// Read the crash directory and classify what SonicTerm wrote.
#[must_use]
fn discover_artifacts(crash_dir: &Path) -> Vec<CrashArtifact> {
    let Ok(entries) = std::fs::read_dir(crash_dir) else {
        // When: read_dir cannot open crash_dir, SonicTerm wrote no artifacts
        // there and there is nothing to classify.
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            // When: path is not a file — a subdirectory or special entry — so
            // it holds no dump text to read.
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            // When: read_to_string fails, the artifact cannot be classified by
            // content, and its kind is not guessed from the name.
            continue;
        };
        found.push(CrashArtifact {
            kind: ArtifactKind::classify(&contents),
            session_id: session_id_from(&contents),
            path,
        });
    }
    found
}

/// Pull the session id out of a dump header, if it records one.
fn session_id_from(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.strip_prefix("session:")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value != "<none>")
    })
}

fn breadcrumb_evidence(log_dir: &Path, session_id: &str) -> Option<BreadcrumbEvidence> {
    let path = crate::breadcrumbs::breadcrumb_path(log_dir, session_id).ok()?;
    path.is_file().then(|| BreadcrumbEvidence { path, session_id: session_id.to_string() })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlatformEvidence {
    evidence: Vec<OsEvidence>,
    notes: Vec<PostmortemNote>,
}

/// macOS: CrashReporter writes `.ips` records under `DiagnosticReports`.
///
/// Matched by an application-name token at the start of the filename, followed
/// by the separator CrashReporter uses before its timestamp. This is a
/// convention rather than proof of provenance, hence [`Attribution::ByName`]
/// and the report's wording that a record "may relate to" the session.
#[cfg(any(test, target_os = "macos"))]
#[must_use]
fn discover_macos_evidence_at(user_dir: &Path, system_dir: &Path) -> Vec<OsEvidence> {
    let mut found = Vec::new();
    for (root, source) in [
        (user_dir, OsEvidenceSource::MacUserDiagnosticReports),
        (system_dir, OsEvidenceSource::MacSystemDiagnosticReports),
    ] {
        let Ok(entries) = std::fs::read_dir(root) else {
            // When: read_dir cannot open root, that DiagnosticReports location
            // holds nothing here, so the other root is still tried.
            continue;
        };
        let mut from_root = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                // When: name is not UTF-8, so to_str yields nothing to match
                // against the CrashReporter naming convention.
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !lower.ends_with(".ips") || !is_conservative_sonicterm_name(&lower) {
                // When: lower is not an .ips record, or the conservative name
                // test rejects it, so it is another application's report.
                continue;
            }
            from_root.push(OsEvidence {
                path: entry.path(),
                attribution: Attribution::ByName,
                source,
            });
        }
        from_root.sort_by(|left, right| left.path.cmp(&right.path));
        found.extend(from_root);
    }
    found
}

#[cfg(any(test, target_os = "macos"))]
fn is_conservative_sonicterm_name(lower_name: &str) -> bool {
    let stem = lower_name.strip_suffix(".ips").unwrap_or(lower_name);
    let Some(rest) = stem.strip_prefix("sonicterm") else {
        // When: strip_prefix finds no sonicterm prefix on stem, so the record
        // names another application entirely.
        return false;
    };
    rest.is_empty()
        || rest.starts_with('-')
        || rest.starts_with('_')
        || rest.strip_prefix("-mac").is_some_and(|suffix| {
            suffix.is_empty() || suffix.starts_with('-') || suffix.starts_with('_')
        })
}

/// Windows: WER queues reports and, when configured, writes local dumps.
///
/// **The WER registry configuration is not read.** Whether `LocalDumps` is
/// enabled lives under `HKLM\SOFTWARE\Microsoft\Windows\Windows Error
/// Reporting\LocalDumps`, and this function inspects only the filesystem
/// locations. The returned [`PostmortemNote`] makes that limitation part of the
/// typed report rather than implying the registry was checked.
#[cfg(any(test, windows))]
#[must_use]
fn discover_windows_evidence_at(local_app_data: &Path) -> PlatformEvidence {
    let roots = [
        (local_app_data.join("CrashDumps"), OsEvidenceSource::WindowsLocalDumps),
        (
            local_app_data.join("Microsoft/Windows/WER/ReportQueue"),
            OsEvidenceSource::WindowsWerQueue,
        ),
        (
            local_app_data.join("Microsoft/Windows/WER/ReportArchive"),
            OsEvidenceSource::WindowsWerArchive,
        ),
    ];

    let mut evidence = Vec::new();
    for (root, source) in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            // When: read_dir cannot open root, that WER or LocalDumps store
            // does not exist here, so the remaining roots are still tried.
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                // When: name is not UTF-8, so to_str yields nothing to match
                // against the WER naming convention.
                continue;
            };
            if !is_windows_sonicterm_record(name, source) {
                // When: is_windows_sonicterm_record rejects name for this
                // source, so it belongs to another application in that store.
                continue;
            }
            evidence.push(OsEvidence {
                path: entry.path(),
                attribution: Attribution::ByName,
                source,
            });
        }
    }
    evidence.sort_by(|left, right| left.path.cmp(&right.path));
    PlatformEvidence { evidence, notes: vec![PostmortemNote::WerRegistryConfigurationNotInspected] }
}

#[cfg(any(test, windows))]
fn is_windows_sonicterm_record(name: &str, source: OsEvidenceSource) -> bool {
    let lower = name.to_ascii_lowercase();
    match source {
        OsEvidenceSource::WindowsLocalDumps => {
            lower == "sonicterm.dmp"
                || lower.starts_with("sonicterm.exe.") && lower.ends_with(".dmp")
                || lower.starts_with("sonicterm-windows.exe.") && lower.ends_with(".dmp")
        }
        OsEvidenceSource::WindowsWerQueue | OsEvidenceSource::WindowsWerArchive => {
            lower.starts_with("appcrash_sonicterm_")
                || lower.starts_with("appcrash_sonicterm-")
                || lower.starts_with("sonicterm_")
                || lower.starts_with("sonicterm-")
        }
        OsEvidenceSource::MacUserDiagnosticReports
        | OsEvidenceSource::MacSystemDiagnosticReports => false,
    }
}

#[cfg(target_os = "macos")]
#[must_use]
fn discover_os_evidence() -> PlatformEvidence {
    let user = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Logs/DiagnosticReports"));
    let system = PathBuf::from("/Library/Logs/DiagnosticReports");
    PlatformEvidence {
        evidence: user
            .as_deref()
            .map_or_else(Vec::new, |user| discover_macos_evidence_at(user, &system)),
        notes: Vec::new(),
    }
}

#[cfg(windows)]
#[must_use]
fn discover_os_evidence() -> PlatformEvidence {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).map_or_else(
        || PlatformEvidence {
            evidence: Vec::new(),
            notes: vec![PostmortemNote::WerRegistryConfigurationNotInspected],
        },
        |local| discover_windows_evidence_at(&local),
    )
}

/// Platforms with no known postmortem store report none.
#[cfg(not(any(target_os = "macos", windows)))]
#[must_use]
fn discover_os_evidence() -> PlatformEvidence {
    PlatformEvidence::default()
}

#[cfg(test)]
#[path = "postmortem_tests.rs"]
mod postmortem_tests;
