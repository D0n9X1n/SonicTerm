//! Contextual filesystem-target detection, validation, and direct-open contracts.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use sonicterm_cfg::url_scan::{
    bare_name_at_char_col_for_style, find_targets_for_style,
    target_candidates_at_char_col_for_style, DetectedTarget, PathStyle, TargetMatch,
};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::{Cell, CellFlags, Row};
use sonicterm_vt::vt::Osc7Cwd;
use winit::window::WindowId;

use super::App;

/// A typed target extracted from one terminal row at one cell column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RowTarget {
    pub(super) matched: TargetMatch,
    pub(super) start_col: u16,
    pub(super) end_col: u16,
}

/// Filesystem kind that may be handed to a platform default application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    File,
    Directory,
}

/// Result of asynchronous local-target validation, including the permitted native action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathOpenDecision {
    /// Open through the platform's ordinary default-application path.
    Openable(PathKind),
    /// Reveal in the native file manager without opening or executing the target.
    #[cfg(any(target_os = "macos", test))]
    Revealable(PathKind),
    /// Existing target whose identity or content is not safe to dispatch.
    Blocked,
    /// Target did not exist when probed.
    Missing,
}

impl PathOpenDecision {
    fn is_actionable(self) -> bool {
        match self {
            Self::Openable(_) => true,
            #[cfg(any(target_os = "macos", test))]
            Self::Revealable(_) => {
                // When: `self` is `Revealable`, permit the same epoch-keyed click path without opening the target.
                true
            }
            Self::Blocked | Self::Missing => false,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    fn is_blocked(self) -> bool {
        self == Self::Blocked
    }
}

/// Monotonic identity that prevents stale probe results from surviving ABA transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeEpoch(u64);

impl ProbeEpoch {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// One typed filesystem candidate carried to the background probe worker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathProbeCandidate {
    pub(crate) start_col: u16,
    pub(crate) end_col: u16,
    pub(crate) target: DetectedTarget,
    pub(crate) resolved_path: PathBuf,
}

impl PathProbeCandidate {
    fn display(&self) -> &str {
        match &self.target {
            DetectedTarget::PathCandidate(candidate) | DetectedTarget::BareName(candidate) => {
                candidate
            }
            DetectedTarget::Uri(_) => unreachable!(),
        }
    }

    fn span_len(&self) -> u16 {
        self.end_col.saturating_sub(self.start_col)
    }
}

/// Immutable identity of one bounded candidate set at one rendered pane cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathProbeKey {
    pub(crate) window_id: WindowId,
    pub(crate) pane_id: u64,
    pub(crate) viewport_row: u16,
    pub(crate) absolute_row: u64,
    pub(crate) view_top: u64,
    pub(crate) pointed_col: u16,
    pub(crate) candidates: Vec<PathProbeCandidate>,
    pub(crate) cwd: Option<Osc7Cwd>,
    pub(crate) cwd_revision: u64,
    pub(crate) row_fingerprint: u64,
    pub(crate) scrollback_evicted: u64,
    pub(crate) alt_screen: bool,
}

/// One openability request sent to the bounded probe worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeRequest {
    pub(crate) epoch: ProbeEpoch,
    pub(crate) key: PathProbeKey,
}

/// Selected candidate and openability returned by the background probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeSelection {
    pub(crate) candidate: PathProbeCandidate,
    pub(crate) decision: PathOpenDecision,
}

/// Openability result returned to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeResult {
    pub(crate) request: PathProbeRequest,
    pub(crate) selection: Option<PathProbeSelection>,
}

/// Per-window authorization state for the raw path currently under the pointer.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathProbeState {
    epoch: ProbeEpoch,
    current: Option<PathProbeKey>,
    selection: Option<PathProbeSelection>,
}

impl Default for ProbeEpoch {
    fn default() -> Self {
        Self::INITIAL
    }
}

impl PathProbeState {
    pub(super) fn invalidate(&mut self) -> bool {
        let changed = self.current.is_some() || self.selection.is_some();
        if changed {
            self.epoch = self.epoch.next();
            self.current = None;
            self.selection = None;
        }
        changed
    }

    pub(super) fn request(&mut self, key: PathProbeKey) -> Option<PathProbeRequest> {
        if self.current.as_ref() == Some(&key) {
            // When: `key` is already current, retain its epoch and avoid duplicating an in-flight openability probe.
            return None;
        }
        if self.current.as_ref().is_some_and(|current| {
            self.selection
                .as_ref()
                .is_some_and(|selection| key_preserves_selection(current, &key, selection))
        }) {
            // When: `key` adds no unprobed equal-or-longer contender, retain the selected span under the same row identity.
            self.current = Some(key);
            return None;
        }
        self.epoch = self.epoch.next();
        self.current = Some(key.clone());
        self.selection = None;
        Some(PathProbeRequest { epoch: self.epoch, key })
    }

    pub(super) fn accept(
        &mut self,
        result: &PathProbeResult,
        fresh: Option<&PathProbeKey>,
    ) -> bool {
        if result.request.epoch != self.epoch || self.current.as_ref() != Some(&result.request.key)
        {
            // When: `result` differs from the current epoch or key, reject it without disturbing a newer probe.
            return false;
        }
        if !fresh.is_some_and(|fresh| match result.selection.as_ref() {
            Some(selection) => key_preserves_selection(&result.request.key, fresh, selection),
            None => fresh == &result.request.key,
        }) {
            // When: `fresh` no longer reproduces the request context and selected span, revoke the stale authorization.
            self.epoch = self.epoch.next();
            self.current = None;
            self.selection = None;
            return false;
        }
        self.current = fresh.cloned();
        self.selection = result.selection.clone();
        true
    }

    pub(super) fn authorized_selection(
        &self,
        key: &PathProbeKey,
        modifier_held: bool,
    ) -> Option<&PathProbeSelection> {
        if !modifier_held || self.current.as_ref() != Some(key) {
            // When: `modifier_held` is false or `current` differs from `key`, no probe result authorizes this click.
            return None;
        }
        self.selection.as_ref().filter(|selection| selection.decision.is_actionable())
    }

    #[cfg(test)]
    pub(super) fn authorized(&self, key: &PathProbeKey, modifier_held: bool) -> bool {
        self.authorized_selection(key, modifier_held).is_some()
    }

    #[cfg(test)]
    pub(super) fn decision_for(&self, key: &PathProbeKey) -> Option<PathOpenDecision> {
        (self.current.as_ref() == Some(key))
            .then(|| self.selection.as_ref().map(|selection| selection.decision))
            .flatten()
    }
}

fn same_probe_context(left: &PathProbeKey, right: &PathProbeKey) -> bool {
    left.window_id == right.window_id
        && left.pane_id == right.pane_id
        && left.viewport_row == right.viewport_row
        && left.absolute_row == right.absolute_row
        && left.view_top == right.view_top
        && left.cwd == right.cwd
        && left.cwd_revision == right.cwd_revision
        && left.row_fingerprint == right.row_fingerprint
        && left.scrollback_evicted == right.scrollback_evicted
        && left.alt_screen == right.alt_screen
}

fn key_preserves_selection(
    probed: &PathProbeKey,
    destination: &PathProbeKey,
    selection: &PathProbeSelection,
) -> bool {
    let selected = &selection.candidate;
    // When: destination identity or selected-span ownership changes, discard authorization before any candidate-set work.
    if !same_probe_context(probed, destination)
        || destination.pointed_col < selected.start_col
        || destination.pointed_col >= selected.end_col
        || !destination.candidates.contains(selected)
    {
        return false;
    }
    let probed_candidates = probed.candidates.iter().collect::<HashSet<_>>();
    probed_candidates.contains(selected)
        // Shorter candidates cannot change a winner selected before their tier.
        && destination
            .candidates
            .iter()
            .filter(|candidate| candidate.span_len() >= selected.span_len())
            .all(|candidate| probed_candidates.contains(candidate))
}

/// Coalescing one-slot mailbox: one request runs while only the newest waits.
#[derive(Clone)]
pub(super) struct PathProbeMailbox {
    latest: Arc<Mutex<Option<PathProbeRequest>>>,
    wake: Sender<()>,
}

impl PathProbeMailbox {
    pub(super) fn new() -> (Self, Receiver<()>) {
        let (wake, receiver) = crossbeam_channel::bounded(1);
        (Self { latest: Arc::new(Mutex::new(None)), wake }, receiver)
    }

    pub(super) fn submit(&self, request: PathProbeRequest) -> Result<(), TrySendError<()>> {
        *self.latest.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(error @ TrySendError::Disconnected(())) => Err(error),
        }
    }

    #[cfg(test)]
    pub(super) fn take_latest(&self) -> Option<PathProbeRequest> {
        self.latest.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }
}

#[cfg(target_os = "linux")]
fn classify_local_target(path: &Path) -> PathOpenDecision {
    match with_opened_target(path, |file| classify_linux_file(path, file)) {
        Ok(decision) => decision,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathOpenDecision::Missing,
        Err(_) => PathOpenDecision::Blocked,
    }
}

#[cfg(target_os = "macos")]
fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_macos_target(path)
}

#[cfg(target_os = "windows")]
fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_windows_target(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn classify_local_target(path: &Path) -> PathOpenDecision {
    let _ = path;
    PathOpenDecision::Blocked
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn classify_nonsymlink_metadata(
    path: &Path,
) -> Result<(std::fs::Metadata, PathKind), PathOpenDecision> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PathOpenDecision::Missing
        } else {
            // When: `error.kind()` is not `NotFound`, deny an unreadable target instead of inferring its identity.
            PathOpenDecision::Blocked
        }
    })?;
    if metadata.file_type().is_symlink() {
        // When: `metadata.file_type()` is a symlink, reject identity redirection before invoking a native opener.
        return Err(PathOpenDecision::Blocked);
    }
    let kind = if metadata.is_file() {
        PathKind::File
    } else if metadata.is_dir() {
        // When: `metadata.is_dir()` identifies a directory, preserve that kind for activation-time revalidation.
        PathKind::Directory
    } else {
        // When: neither `metadata.is_file()` nor `metadata.is_dir()` holds, block sockets, devices, and other special entries.
        return Err(PathOpenDecision::Blocked);
    };
    Ok((metadata, kind))
}

#[cfg(any(target_os = "macos", test))]
fn macos_file_policy(path: &Path, prefix: &[u8], executable_mode: bool) -> PathOpenDecision {
    const BLOCKED_EXTENSIONS: &[&str] = &[
        "app",
        "command",
        "terminal",
        "workflow",
        "scpt",
        "applescript",
        "pkg",
        "mpkg",
        "dmg",
        "webloc",
        "osascript",
    ];
    const REVEALABLE_EXTENSIONS: &[&str] = &[
        "sh", "bash", "zsh", "csh", "tcsh", "ksh", "fish", "py", "pyw", "rb", "pl", "pm", "php",
        "lua", "tcl", "js", "jxa",
    ];
    const EXECUTABLE_MAGICS: &[&[u8]] = &[
        b"\x7fELF",
        b"#!",
        b"MZ",
        b"\xfe\xed\xfa\xce",
        b"\xce\xfa\xed\xfe",
        b"\xfe\xed\xfa\xcf",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
        b"\xca\xfe\xba\xbf",
        b"\xbf\xba\xfe\xca",
    ];
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    let blocked_extension =
        BLOCKED_EXTENSIONS.iter().any(|blocked| extension.eq_ignore_ascii_case(blocked));
    let revealable_extension =
        REVEALABLE_EXTENSIONS.iter().any(|suffix| extension.eq_ignore_ascii_case(suffix));
    let blocked_content = EXECUTABLE_MAGICS.iter().any(|magic| prefix.starts_with(magic));
    if executable_mode || blocked_extension || blocked_content {
        PathOpenDecision::Blocked
    } else if revealable_extension {
        // When: `revealable_extension` is the sole restriction, Finder may select the inert source without opening it.
        PathOpenDecision::Revealable(PathKind::File)
    } else {
        // When: `executable_mode`, `blocked_extension`, `blocked_content`, and `revealable_extension` are false, allow the file.
        PathOpenDecision::Openable(PathKind::File)
    }
}

#[cfg(target_os = "macos")]
fn classify_macos_target(path: &Path) -> PathOpenDecision {
    use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};

    let (_, kind) = match classify_nonsymlink_metadata(path) {
        Ok(classified) => classified,
        Err(decision) => {
            // When: `classify_nonsymlink_metadata` returns `Err`, retain its missing-or-blocked decision unchanged.
            return decision;
        }
    };
    if kind == PathKind::Directory {
        // When: `kind` is `Directory`, inspect bundle metadata before allowing Finder to open the target itself.
        let blocked_suffix = macos_file_policy(path, b"", false).is_blocked();
        if blocked_suffix {
            // When: `blocked_suffix` identifies package or launcher syntax, never hand that directory to LaunchServices.
            return PathOpenDecision::Blocked;
        }
        let bundle_marker = path.join("Contents/Info.plist");
        match std::fs::symlink_metadata(bundle_marker) {
            Ok(_) => {
                // When: `bundle_marker` exists, treat this directory as executable application content and block it.
                return PathOpenDecision::Blocked;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // When: `bundle_marker` is `NotFound`, the directory has no application-bundle marker and remains eligible.
            }
            Err(_) => {
                // When: reading `bundle_marker` fails for another reason, fail closed instead of assuming a safe directory.
                return PathOpenDecision::Blocked;
            }
        }
        return PathOpenDecision::Openable(PathKind::Directory);
    }

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // When: no-follow `open` reports `NotFound`, the path disappeared after metadata classification.
            return PathOpenDecision::Missing;
        }
        Err(_) => {
            // When: no-follow `open` fails otherwise, block unreadable or redirected identity.
            return PathOpenDecision::Blocked;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            // When: descriptor `metadata` is unavailable, fail closed instead of inferring file type or mode.
            return PathOpenDecision::Blocked;
        }
    };
    if !metadata.is_file() {
        // When: `metadata.is_file()` is false, the no-follow descriptor no longer identifies a regular file.
        return PathOpenDecision::Blocked;
    }
    let mut prefix = [0u8; 8];
    let read = match file.read_at(&mut prefix, 0) {
        Ok(read) => read,
        Err(_) => {
            // When: reading the descriptor prefix fails, deny content whose executable class cannot be determined.
            return PathOpenDecision::Blocked;
        }
    };
    let executable_mode = metadata.permissions().mode() & 0o111 != 0;
    macos_file_policy(path, &prefix[..read], executable_mode)
}

#[cfg(any(target_os = "windows", test))]
fn windows_file_name(path: &Path) -> Option<&str> {
    path.to_str()?.rsplit(['/', '\\']).next().filter(|name| !name.is_empty())
}

#[cfg(any(target_os = "windows", test))]
fn windows_path_policy(path: &Path, pathext: Option<&str>) -> PathOpenDecision {
    let Some(name) = windows_file_name(path) else {
        // When: `windows_file_name` cannot produce one nonempty component, reject an unclassifiable ShellExecute target.
        return PathOpenDecision::Blocked;
    };
    if name.contains(':')
        || name.ends_with(['.', ' '])
        || name.chars().any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        // When: `name` contains ADS or reserved syntax, block Windows normalization and alternate-stream ambiguity.
        return PathOpenDecision::Blocked;
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_default();
    const BLOCKED: &[&str] = &[
        ".lnk", ".url", ".scf", ".pif", ".msi", ".msp", ".reg", ".ps1", ".bat", ".cmd", ".com",
        ".exe", ".vbs", ".vbe", ".js", ".jse", ".wsf", ".hta", ".jar", ".cpl", ".inf", ".scr",
        ".iso", ".vhd", ".vhdx",
    ];
    let pathext_blocked = pathext.is_some_and(|value| {
        value.split(';').any(|item| item.trim().eq_ignore_ascii_case(&extension))
    });
    if pathext_blocked || BLOCKED.contains(&extension.as_str()) {
        PathOpenDecision::Blocked
    } else {
        // When: neither `pathext_blocked` nor `BLOCKED` claims `extension`, preserve regular-file eligibility.
        PathOpenDecision::Openable(PathKind::File)
    }
}

#[cfg(target_os = "windows")]
fn classify_windows_target(path: &Path) -> PathOpenDecision {
    use std::os::windows::fs::MetadataExt;

    let (metadata, kind) = match classify_nonsymlink_metadata(path) {
        Ok(classified) => classified,
        Err(decision) => {
            // When: `classify_nonsymlink_metadata` returns `Err`, retain its missing-or-blocked decision unchanged.
            return decision;
        }
    };
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        // When: `metadata.file_attributes()` contains `FILE_ATTRIBUTE_REPARSE_POINT`, block redirected identity.
        return PathOpenDecision::Blocked;
    }
    let pathext = std::env::var("PATHEXT").ok();
    if windows_path_policy(path, pathext.as_deref()).is_blocked() {
        PathOpenDecision::Blocked
    } else {
        // When: `windows_path_policy` does not block `path`, restore the metadata-derived file-or-directory `kind`.
        PathOpenDecision::Openable(kind)
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_file_policy(path: &Path, prefix: &[u8]) -> PathOpenDecision {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    if extension.eq_ignore_ascii_case("desktop")
        || extension.eq_ignore_ascii_case("appimage")
        || prefix.starts_with(b"\x7fELF")
        || prefix.starts_with(b"#!")
        || prefix.starts_with(b"MZ")
    {
        PathOpenDecision::Blocked
    } else {
        // When: `extension` and `prefix` identify no launcher or executable format, allow the regular file.
        PathOpenDecision::Openable(PathKind::File)
    }
}

fn select_openable_candidate(
    candidates: &[PathProbeCandidate],
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
) -> Option<PathProbeSelection> {
    let mut index = 0;
    while index < candidates.len() {
        let span_len = candidates[index].span_len();
        let mut actionable = Vec::new();
        let mut blocked = false;
        while index < candidates.len() && candidates[index].span_len() == span_len {
            let candidate = &candidates[index];
            match classify(&candidate.resolved_path) {
                decision @ PathOpenDecision::Openable(_) => {
                    actionable.push(PathProbeSelection { candidate: candidate.clone(), decision });
                }
                #[cfg(any(target_os = "macos", test))]
                decision @ PathOpenDecision::Revealable(_) => {
                    // When: `decision` is `Revealable`, retain its exact Finder-only action in this candidate tier.
                    actionable.push(PathProbeSelection { candidate: candidate.clone(), decision });
                }
                PathOpenDecision::Blocked => blocked = true,
                PathOpenDecision::Missing => {
                    // When: `classify` returns `PathOpenDecision::Missing`, keep searching shorter candidate tiers.
                }
            }
            index += 1;
        }
        if blocked || actionable.len() > 1 {
            // When: the longest existing tier is blocked or ambiguous, fail closed instead of falling back to a shorter path.
            return None;
        }
        if let Some(selection) = actionable.pop() {
            // When: `actionable.pop()` returns the sole longest candidate, authorize its exact action and span.
            return Some(selection);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathOpenRequest {
    path: PathBuf,
    expected_decision: PathOpenDecision,
}

/// App-owned handles for the bounded path probe and open workers.
pub(crate) struct PathWorkers {
    probe: PathProbeMailbox,
    open: Sender<PathOpenRequest>,
}

impl PathWorkers {
    /// Start one coalescing openability worker and one serialized target-open worker.
    pub(super) fn start(
        proxy: winit::event_loop::EventLoopProxy<super::UserEvent>,
    ) -> io::Result<Self> {
        let (probe, wake) = PathProbeMailbox::new();
        let latest_probe = Arc::clone(&probe.latest);
        let probe_proxy = proxy.clone();
        std::thread::Builder::new()
            .name("sonicterm-path-probe".into())
            .spawn(move || {
                while wake.recv().is_ok() {
                    let Some(request) =
                        latest_probe.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
                    else {
                        // When: a coalesced wake has no `request`, resume waiting without probing stale state.
                        continue;
                    };
                    let selection =
                        select_openable_candidate(&request.key.candidates, classify_local_target);
                    if probe_proxy
                        .send_event(super::UserEvent::PathProbeFinished(PathProbeResult {
                            request,
                            selection,
                        }))
                        .is_err()
                    {
                        // When: `probe_proxy.send_event` fails, the event loop is gone and the worker must terminate.
                        break;
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("spawn path probe worker: {error}")))?;

        let (open, open_rx) = crossbeam_channel::bounded::<PathOpenRequest>(1);
        std::thread::Builder::new()
            .name("sonicterm-path-open".into())
            .spawn(move || {
                while let Ok(request) = open_rx.recv() {
                    if let Err(error) = open_path(&request.path, request.expected_decision) {
                        tracing::warn!(path = ?request.path, %error, "path open failed");
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("spawn path open worker: {error}")))?;

        Ok(Self { probe, open })
    }

    pub(super) fn probe(&self, request: PathProbeRequest) -> io::Result<()> {
        self.probe
            .submit(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "path probe worker stopped"))
    }

    /// Queue a target open without blocking the event loop.
    ///
    /// `Ok(false)` means the bounded worker already has one running and one
    /// waiting request; the click is still consumed and the extra open drops.
    pub(super) fn open(
        &self,
        path: PathBuf,
        expected_decision: PathOpenDecision,
    ) -> io::Result<bool> {
        match self.open.try_send(PathOpenRequest { path, expected_decision }) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "path open worker stopped"))
            }
        }
    }
}

/// Platform-neutral command description used by opener tests without spawning handlers.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandSpec {
    pub(super) program: PathBuf,
    pub(super) args: Vec<String>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_command(spec: CommandSpec) -> io::Result<()> {
    let status = Command::new(spec.program)
        .args(spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        // When: the fixed native opener returns a failed `status`, surface it instead of reporting a successful click.
        Err(io::Error::other(format!("path opener exited with {status}")))
    }
}

#[cfg(target_os = "macos")]
fn macos_validated_open_spec(
    path: &Path,
    expected_decision: PathOpenDecision,
) -> io::Result<CommandSpec> {
    if classify_macos_target(path) != expected_decision {
        // When: `classify_macos_target` no longer matches `expected_decision`, reject changed identity, kind, or action.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked macOS target",
        ));
    }
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    match expected_decision {
        PathOpenDecision::Openable(_) => macos_open_spec(text),
        PathOpenDecision::Revealable(_) => macos_reveal_spec(text),
        PathOpenDecision::Blocked | PathOpenDecision::Missing => None,
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid macOS path"))
}

#[cfg(target_os = "macos")]
fn open_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    run_command(macos_validated_open_spec(path, expected_decision)?)
}

#[cfg(target_os = "windows")]
fn open_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    let PathOpenDecision::Openable(expected_kind) = expected_decision else {
        // When: `expected_decision` is not `Openable`, Windows has no reveal-only dispatch contract.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Windows action"));
    };
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if classify_windows_target(path) != PathOpenDecision::Openable(expected_kind) {
        // When: `classify_windows_target` no longer returns `expected_kind`, reject a changed or newly blocked target.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked Windows target",
        ));
    }
    let verb = "open\0".encode_utf16().collect::<Vec<_>>();
    let target = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: this dedicated worker owns its COM apartment; all UTF-16 buffers
    // remain live through the synchronous SEE_MASK_NOASYNC call.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(io::Error::other)?;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(target.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = ShellExecuteExW(&mut info).map_err(io::Error::other);
        CoUninitialize();
        result
    }
}

#[cfg(any(target_os = "linux", test))]
fn with_opened_target<T>(
    path: &Path,
    open: impl FnOnce(&mut std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(target_os = "linux")]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?
    };
    #[cfg(not(target_os = "linux"))]
    let mut file = std::fs::File::open(path)?;
    open(&mut file)
}

#[cfg(target_os = "linux")]
fn classify_linux_file(path: &Path, file: &mut std::fs::File) -> io::Result<PathOpenDecision> {
    use std::os::unix::fs::FileExt;

    let metadata = file.metadata()?;
    if metadata.is_dir() {
        // When: `metadata.is_dir()` proves directory identity, preserve that kind for activation-time revalidation.
        return Ok(PathOpenDecision::Openable(PathKind::Directory));
    }
    if !metadata.is_file() {
        // When: `metadata.is_file()` is false after directory rejection, block sockets, devices, and other special entries.
        return Ok(PathOpenDecision::Blocked);
    }
    let mut prefix = [0u8; 8];
    let read = file.read_at(&mut prefix, 0)?;
    Ok(linux_file_policy(path, &prefix[..read]))
}

#[cfg(target_os = "linux")]
fn linux_portal_unavailable(error: &ashpd::Error) -> bool {
    use ashpd::zbus;

    match error {
        ashpd::Error::PortalNotFound(_) => true,
        ashpd::Error::Zbus(
            zbus::Error::Address(_)
            | zbus::Error::Handshake(_)
            | zbus::Error::InputOutput(_)
            | zbus::Error::InterfaceNotFound
            | zbus::Error::Unsupported,
        ) => true,
        ashpd::Error::Zbus(zbus::Error::FDO(error)) => matches!(
            error.as_ref(),
            zbus::fdo::Error::ServiceUnknown(_)
                | zbus::fdo::Error::NameHasNoOwner(_)
                | zbus::fdo::Error::NoServer(_)
                | zbus::fdo::Error::Disconnected(_)
        ),
        _ => false,
    }
}

#[cfg(target_os = "linux")]
fn open_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    use ashpd::desktop::open_uri::OpenFileRequest;

    let PathOpenDecision::Openable(expected_kind) = expected_decision else {
        // When: `expected_decision` is not `Openable`, Linux has no reveal-only dispatch contract.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Linux action"));
    };
    let portal = with_opened_target(path, |file| {
        if classify_linux_file(path, file)? != PathOpenDecision::Openable(expected_kind) {
            // When: `classify_linux_file` differs from `expected_kind`, reject identity or type changes before portal handoff.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "changed or blocked Linux target",
            ));
        }
        async_io::block_on(async {
            let request = match OpenFileRequest::default()
                .writeable(false)
                .ask(false)
                .send_file(file)
                .await
            {
                Ok(request) => request,
                Err(error) if linux_portal_unavailable(&error) => {
                    // When: `linux_portal_unavailable` accepts `error`, signal the narrowly permitted fixed-path fallback.
                    return Err(io::Error::new(io::ErrorKind::NotFound, error.to_string()));
                }
                Err(error) => {
                    // When: `send_file` returns another `error`, preserve it and never bypass portal rejection with a fallback.
                    return Err(io::Error::other(error.to_string()));
                }
            };
            request.response().map_err(|error| io::Error::other(error.to_string()))
        })
    });
    match portal {
        Ok(()) => {
            // When: `portal` succeeds, the target was submitted once and no fallback may run.
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // When: portal `error.kind()` is `NotFound`, try only the fixed executable fallback after revalidation.
        }
        Err(error) => {
            // When: `portal` fails for any other reason, return `error` without risking a second open or bypass.
            return Err(error);
        }
    }

    let spec = linux_xdg_open_spec(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "xdg-open unavailable"))?;
    with_opened_target(path, |file| {
        if classify_linux_file(path, file)? != PathOpenDecision::Openable(expected_kind) {
            // When: fallback `classify_linux_file` no longer returns `expected_kind`, reject a raced or reclassified target.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "changed or blocked Linux target",
            ));
        }
        run_command(spec)
    })
}

#[cfg(any(target_os = "linux", test))]
fn linux_xdg_open_spec(path: &Path) -> Option<CommandSpec> {
    let target = path.to_str()?;
    if !target.starts_with('/') {
        // When: `target` lacks an absolute POSIX root, never pass process-relative state to `xdg-open`.
        return None;
    }
    let program = ["/usr/bin/xdg-open", "/bin/xdg-open"]
        .into_iter()
        .find(|candidate| Path::new(candidate).is_file())?;
    Some(CommandSpec { program: PathBuf::from(program), args: vec![target.to_string()] })
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn open_path(_path: &Path, _expected_decision: PathOpenDecision) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "path open is unsupported"))
}

fn row_target_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    lookup: impl FnOnce(&str, usize, PathStyle) -> Option<TargetMatch>,
) -> Option<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    cells.get(col)?;
    let (token_start, token_end) = token_bounds(&cells, col);
    if cells[token_start..token_end].iter().any(unsafe_path_cell) {
        // When: any cell in the token is wide or combining, reject the whole token rather than exposing an ASCII suffix.
        return None;
    }

    let mut text = String::with_capacity(cells.len());
    let mut byte_ranges = Vec::with_capacity(cells.len());
    for cell in cells {
        let start = text.len();
        // When: `cell.flags` contains `WIDE_CONT`, preserve its column with a non-path sentinel; otherwise keep the cell character.
        let ch = if cell.flags.contains(CellFlags::WIDE_CONT) { '\u{fdd0}' } else { cell.ch };
        text.push(ch);
        byte_ranges.push((start, text.len()));
    }
    let matched = lookup(&text, col, style)?;
    let start_col = byte_ranges.iter().position(|(start, _)| *start == matched.start)?;
    let end_col =
        byte_ranges.iter().position(|(start, _)| *start >= matched.end).unwrap_or(row.len());
    if start_col < token_start || end_col > token_end {
        // When: the detected span escapes `token_start..token_end`, reject cross-token reconstruction.
        return None;
    }
    let start_col = u16::try_from(start_col).ok()?;
    let end_col = u16::try_from(end_col).ok()?;
    Some(RowTarget { matched, start_col, end_col })
}

pub(super) fn target_at_row_cell(row: &Row, col: u16, style: PathStyle) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, |text, col, style| {
        let clicked_byte = text.char_indices().nth(col)?.0;
        find_targets_for_style(text, style)
            .into_iter()
            .find(|matched| clicked_byte >= matched.start && clicked_byte < matched.end)
    })
}

pub(super) fn bare_target_at_row_cell(row: &Row, col: u16, style: PathStyle) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, bare_name_at_char_col_for_style)
}

fn row_target_candidates_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    include_bare_names: bool,
) -> Vec<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    if cells.get(col).is_none() {
        // When: `col` lies beyond the materialized row, no scanner span can own it.
        return Vec::new();
    }
    let mut text = String::with_capacity(cells.len());
    let mut byte_ranges = Vec::with_capacity(cells.len());
    for cell in &cells {
        let start = text.len();
        // When: `cell.flags` contains `WIDE_CONT`, retain its column with a non-path sentinel; otherwise retain `cell.ch`.
        let ch = if cell.flags.contains(CellFlags::WIDE_CONT) { '\u{fdd0}' } else { cell.ch };
        text.push(ch);
        byte_ranges.push((start, text.len()));
    }

    target_candidates_at_char_col_for_style(&text, col, style, include_bare_names)
        .into_iter()
        .filter_map(|matched| {
            let start_col = byte_ranges.iter().position(|(start, _)| *start == matched.start)?;
            let end_col = byte_ranges
                .iter()
                .position(|(start, _)| *start >= matched.end)
                .unwrap_or(cells.len());
            let mut source_end_col = end_col;
            while source_end_col < cells.len()
                && matches!(cells[source_end_col].ch, ',' | ';' | '.' | ':' | '!' | '?')
            {
                source_end_col += 1;
            }
            if start_col >= end_col
                || start_col > col
                || end_col <= col
                || source_end_col < end_col
                || cells[start_col..source_end_col]
                    .iter()
                    .any(|cell| unsafe_path_cell(cell) || cell.hyperlink().is_some())
            {
                // When: the visible or literal source span misses ownership or crosses unsafe cells, reject the candidate.
                return None;
            }
            Some(RowTarget {
                matched,
                start_col: u16::try_from(start_col).ok()?,
                end_col: u16::try_from(end_col).ok()?,
            })
        })
        .collect()
}

/// Return the first configured home variable that is valid for the native path grammar.
pub(super) fn native_home_dir() -> Option<PathBuf> {
    let variables = if cfg!(target_os = "windows") {
        // When: `target_os` is Windows, prefer `USERPROFILE` before validating fallback `HOME`.
        ["USERPROFILE", "HOME"]
    } else {
        // When: `target_os` is not Windows, prefer `HOME` before validating fallback `USERPROFILE`.
        ["HOME", "USERPROFILE"]
    };
    variables.into_iter().filter_map(std::env::var_os).find_map(|value| {
        let value = value.to_str()?;
        match PathStyle::native() {
            PathStyle::Posix => normalize_posix_cwd(value).map(PathBuf::from),
            PathStyle::Windows => normalize_windows_home(value).map(PathBuf::from),
        }
    })
}

pub(super) fn resolve_path_candidate(
    candidate: &str,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    let home_relative = is_home_relative(candidate, style);
    let relative =
        is_explicit_relative(candidate, style) || is_contextual_relative(candidate, style);
    let combined = if home_relative {
        let home = home?.to_str()?;
        let suffix = &candidate[2..];
        match style {
            PathStyle::Posix => {
                let home = normalize_posix_cwd(home)?;
                format!("{}/{}", home.trim_end_matches('/'), suffix)
            }
            PathStyle::Windows => {
                let home = normalize_windows_home(home)?;
                format!("{}\\{}", home.trim_end_matches(['/', '\\']), suffix)
            }
        }
    } else if relative {
        // When: `candidate` is dot-relative or separator-relative, resolve it only from this pane's trusted local CWD.
        let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
        match style {
            PathStyle::Posix => format!("{}/{}", cwd.path.trim_end_matches('/'), candidate),
            PathStyle::Windows => {
                let cwd = normalize_windows_cwd(&cwd.path)?;
                format!("{}\\{}", cwd.trim_end_matches(['/', '\\']), candidate)
            }
        }
    } else {
        // When: `candidate` is absolute rather than home-relative or dot-relative, resolve it without contextual state.
        candidate.to_string()
    };
    match style {
        PathStyle::Posix => normalize_posix_absolute(&combined).map(PathBuf::from),
        PathStyle::Windows => normalize_windows_absolute(&combined).map(PathBuf::from),
    }
}

pub(super) fn resolve_detected_path(
    target: &DetectedTarget,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    match target {
        DetectedTarget::Uri(_) => None,
        DetectedTarget::PathCandidate(candidate) => {
            resolve_path_candidate(candidate, style, cwd, home, local_hostname)
        }
        DetectedTarget::BareName(candidate) => {
            // When: `target` is `BareName`, require one safe component and the exact pane's trusted local CWD.
            if candidate.is_empty()
                || candidate.len() > 4096
                || matches!(candidate.as_str(), "." | "..")
                || candidate.contains(['/', '\\'])
                || candidate.chars().any(char::is_control)
            {
                // When: `candidate` is empty, overlong, special, separated, or controlled, reject contextual resolution.
                return None;
            }
            let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
            match style {
                PathStyle::Posix => {
                    let cwd = normalize_posix_cwd(&cwd.path)?;
                    normalize_posix_absolute(&format!("{}/{candidate}", cwd.trim_end_matches('/')))
                        .map(PathBuf::from)
                }
                PathStyle::Windows => {
                    // When: `style` is Windows, apply native component restrictions before joining the trusted CWD.
                    if candidate.contains(':')
                        || candidate.ends_with(['.', ' '])
                        || candidate
                            .chars()
                            .any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
                    {
                        // When: Windows `candidate` contains reserved, ADS, or normalization-sensitive syntax, leave it inert.
                        return None;
                    }
                    let cwd = normalize_windows_cwd(&cwd.path)?;
                    normalize_windows_absolute(&format!(
                        "{}\\{candidate}",
                        cwd.trim_end_matches(['/', '\\'])
                    ))
                    .map(PathBuf::from)
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_open_spec(path: &str) -> Option<CommandSpec> {
    let path = normalize_posix_absolute(path)?;
    Some(CommandSpec {
        program: PathBuf::from("/usr/bin/open"),
        args: vec!["--".to_string(), path],
    })
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_reveal_spec(path: &str) -> Option<CommandSpec> {
    let path = normalize_posix_absolute(path)?;
    Some(CommandSpec {
        program: PathBuf::from("/usr/bin/open"),
        args: vec!["-R".to_string(), "--".to_string(), path],
    })
}

fn token_bounds(cells: &[&Cell], col: usize) -> (usize, usize) {
    let mut start = col;
    while start > 0 && !cell_delimiter(cells[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && !cell_delimiter(cells[end]) {
        end += 1;
    }
    (start, end)
}

fn cell_delimiter(cell: &Cell) -> bool {
    if cell.flags.contains(CellFlags::WIDE_CONT) {
        // When: `cell` is a wide continuation, keep it attached to its lead cell instead of treating its stored space as a delimiter.
        return false;
    }
    cell.ch.is_whitespace()
        || cell.ch.is_control()
        || matches!(cell.ch, '"' | '\'' | '`' | '<' | '>')
}

fn row_fingerprint(row: &Row) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

fn unsafe_path_cell(cell: &&Cell) -> bool {
    cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT)
        || cell.extras().is_some_and(|extras| !extras.is_empty())
}

fn authority_is_local(authority: &str, local_hostname: &str) -> bool {
    authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || (!local_hostname.is_empty() && authority.eq_ignore_ascii_case(local_hostname))
}

fn is_home_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("~/"),
        PathStyle::Windows => candidate.starts_with("~/") || candidate.starts_with("~\\"),
    }
}

fn is_contextual_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => !candidate.starts_with('/') && candidate.contains('/'),
        PathStyle::Windows => {
            let bytes = candidate.as_bytes();
            let drive_absolute = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            !drive_absolute && candidate.contains(['/', '\\'])
        }
    }
}

fn is_explicit_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("./") || candidate.starts_with("../"),
        PathStyle::Windows => {
            candidate.starts_with("./")
                || candidate.starts_with(".\\")
                || candidate.starts_with("../")
                || candidate.starts_with("..\\")
        }
    }
}

fn normalize_posix_cwd(path: &str) -> Option<String> {
    if path == "/" {
        Some(path.to_string())
    } else {
        // When: `path` is not the POSIX root, require an ordinary named absolute CWD.
        normalize_posix_absolute(path)
    }
}

fn normalize_posix_absolute(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        // When: `path` is not a single-root POSIX absolute path, reject relative, network, and cross-platform forms.
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming POSIX segment.
            }
            ".." => {
                // Pop one lexical ancestor while clamping traversal at the root.
                components.pop();
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| format!("/{}", components.join("/")))
}

fn normalize_windows_home(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'/' | b'\\')
    {
        // When: `bytes` lack a drive-root separator, reject drive-relative home input instead of promoting `C:` to `C:\`.
        return None;
    }
    normalize_windows_absolute(path)
}

fn normalize_windows_cwd(path: &str) -> Option<String> {
    let normalized = path.replace('/', "\\");
    let normalized = if normalized.len() >= 4
        && normalized.starts_with('\\')
        && normalized.as_bytes()[1].is_ascii_alphabetic()
        && normalized.as_bytes()[2] == b':'
        && normalized.as_bytes()[3] == b'\\'
    {
        normalized[1..].to_string()
    } else {
        // When: `normalized` is not the OSC 7 `/C:/...` drive form, retain it for ordinary absolute validation.
        normalized
    };
    normalize_windows_absolute(&normalized)
}

fn normalize_windows_absolute(path: &str) -> Option<String> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        // When: `path` starts with a double separator, reject unsupported UNC and network targets.
        return None;
    }
    let path = path.replace('/', "\\");
    let bytes = path.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // When: `bytes` are exactly a drive designator, normalize the Windows root with its trailing separator.
        return Some(format!("{}:\\", bytes[0] as char));
    }
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        // When: `bytes` do not form a drive-rooted absolute path, reject relative and malformed Windows input.
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut components = Vec::new();
    for component in path[3..].split('\\') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming Windows segment.
            }
            ".." => {
                components.pop();
            }
            value
                if value.contains(':')
                    || value.chars().any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*')) =>
            {
                // When: `value` contains a reserved Windows path character, reject the component.
                return None;
            }
            value if value.ends_with(['.', ' ']) => {
                // When: `value` ends in a dot or space, reject Windows normalization aliases before probing.
                return None;
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Some(format!("{drive}:\\"))
    } else {
        // When: `components` contains named segments, append them beneath the normalized drive root.
        Some(format!("{drive}:\\{}", components.join("\\")))
    }
}

fn detected_target_enabled(
    target: &DetectedTarget,
    clickable_local_targets: bool,
    clickable_bare_names: bool,
) -> bool {
    match target {
        DetectedTarget::Uri(_) => true,
        DetectedTarget::PathCandidate(_) => clickable_local_targets,
        DetectedTarget::BareName(_) => clickable_local_targets && clickable_bare_names,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedCellTarget {
    Uri(String),
    Path(PathProbeKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellTargetSnapshot {
    pub(super) pane_id: u64,
    pub(super) viewport_row: u16,
    pub(super) start_col: u16,
    pub(super) end_col: u16,
    pub(super) display: String,
    pub(super) explicit_hyperlink: bool,
    pub(super) target: ResolvedCellTarget,
}

impl CellTargetSnapshot {
    fn hovered(&self, active: bool) -> super::hovered_url::HoveredUrl {
        super::hovered_url::HoveredUrl {
            pane_id: self.pane_id,
            row: self.viewport_row,
            start_col: self.start_col,
            end_col: self.end_col,
            url: self.display.clone(),
            active,
        }
    }
}

impl App {
    pub(super) fn cell_target_at(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> Option<CellTargetSnapshot> {
        let window = self.windows.get(&window_id)?;
        let pane = window.panes.get(&pane_id)?;
        let parser = pane.parser.try_lock()?;
        let grid = parser.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        let absolute_row = view_top.checked_add(u64::from(viewport_row))?;
        let row = grid.row_at_abs(absolute_row)?;
        let cell = row.iter().nth(usize::from(col))?;
        if let Some(hyperlink_id) = cell.hyperlink() {
            // When: `cell` carries `hyperlink_id`, preserve its OSC 8 URI provenance instead of scanning the displayed text as a path.
            let uri = parser.hyperlinks().lookup(hyperlink_id)?.uri.clone();
            return Some(CellTargetSnapshot {
                pane_id,
                viewport_row,
                start_col: col,
                end_col: col.saturating_add(1),
                display: uri.clone(),
                explicit_hyperlink: true,
                target: ResolvedCellTarget::Uri(uri),
            });
        }

        let style = PathStyle::native();
        let clickable_local_targets = self.config.terminal.clickable_local_targets;
        let clickable_bare_names = self.config.terminal.clickable_bare_names;
        let legacy_target = target_at_row_cell(row, col, style).or_else(|| {
            (clickable_local_targets && clickable_bare_names)
                .then(|| bare_target_at_row_cell(row, col, style))
                .flatten()
        });
        if let Some(RowTarget {
            matched: TargetMatch { target: DetectedTarget::Uri(uri), .. },
            start_col,
            end_col,
        }) = legacy_target
        {
            // When: `legacy_target` is an allow-listed URI, preserve its precedence over every filesystem candidate.
            return Some(CellTargetSnapshot {
                pane_id,
                viewport_row,
                start_col,
                end_col,
                display: uri.clone(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Uri(uri),
            });
        }
        if !clickable_local_targets {
            // When: `clickable_local_targets` is false, leave non-URI text inert before resolving any candidate set.
            return None;
        }

        let cwd = parser.osc7_cwd().cloned();
        let mut candidates = row_target_candidates_at_cell(row, col, style, clickable_bare_names)
            .into_iter()
            .filter(|row_target| {
                detected_target_enabled(
                    &row_target.matched.target,
                    clickable_local_targets,
                    clickable_bare_names,
                )
            })
            .filter_map(|row_target| {
                let resolved_path = resolve_detected_path(
                    &row_target.matched.target,
                    style,
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                Some(PathProbeCandidate {
                    start_col: row_target.start_col,
                    end_col: row_target.end_col,
                    target: row_target.matched.target,
                    resolved_path,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .span_len()
                .cmp(&left.span_len())
                .then_with(|| left.start_col.cmp(&right.start_col))
                .then_with(|| left.end_col.cmp(&right.end_col))
        });
        candidates.dedup();
        let display = candidates.first()?.display().to_string();
        let key = PathProbeKey {
            window_id,
            pane_id,
            viewport_row,
            absolute_row,
            view_top,
            pointed_col: col,
            candidates,
            cwd,
            cwd_revision: parser.cwd_revision(),
            row_fingerprint: row_fingerprint(row),
            scrollback_evicted: grid.scrollback_evicted(),
            alt_screen: grid.is_alt(),
        };
        Some(CellTargetSnapshot {
            pane_id,
            viewport_row,
            start_col: col,
            end_col: col.saturating_add(1),
            display,
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key),
        })
    }

    fn pointer_target(&self, window_id: WindowId) -> Option<CellTargetSnapshot> {
        let window = self.windows.get(&window_id)?;
        let (x, y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let (rendered_pane, row, col) = window.renderer.as_ref()?.pixel_to_pane_cell(x, y)?;
        let pane_id = if rendered_pane == 0 {
            // When: `rendered_pane` is zero, use `main_window_id` geometry for the main `window_id` and child geometry otherwise.
            if Some(window_id) == self.main_window_id {
                self.pane_at_cursor(x, y)?
            } else {
                super::pane_id_at_point(&Self::compute_pane_rects_for(window), x, y)?
            }
        } else {
            // When: `rendered_pane` is nonzero, retain the renderer-owned pane identity paired with `row` and `col`.
            rendered_pane
        };
        self.cell_target_at(window_id, pane_id, row, col)
    }

    pub(super) fn refresh_target_hover(&mut self, window_id: WindowId) {
        let target = self.pointer_target(window_id);
        let modifier_held = self.open_modifier_held(window_id);
        let mut probe_request = None;
        let mut hovered = None;
        let explicit_hyperlink = target.as_ref().is_some_and(|target| target.explicit_hyperlink);
        let mut visual_changed = false;

        if let Some(window) = self.windows.get_mut(&window_id) {
            let previous_hover = window.hovered_url.clone();
            let previous_link = window.hover_link;
            match target.as_ref() {
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Uri(_), .. }) => {
                    window.path_probe.invalidate();
                    if !target.explicit_hyperlink {
                        hovered = Some(target.hovered(modifier_held));
                    }
                }
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Path(key), .. }) => {
                    probe_request = window.path_probe.request(key.clone());
                    if let Some(selection) =
                        window.path_probe.authorized_selection(key, modifier_held)
                    {
                        hovered = Some(super::hovered_url::HoveredUrl {
                            pane_id: target.pane_id,
                            row: target.viewport_row,
                            start_col: selection.candidate.start_col,
                            end_col: selection.candidate.end_col,
                            url: selection.candidate.display().to_string(),
                            active: true,
                        });
                    }
                }
                None => {
                    window.path_probe.invalidate();
                }
            }
            window.hovered_url = hovered;
            window.hover_link =
                window.hovered_url.as_ref().is_some_and(|hover| hover.active) || explicit_hyperlink;
            visual_changed =
                previous_hover != window.hovered_url || previous_link != window.hover_link;
        }

        if let Some(request) = probe_request {
            if let Some(workers) = &self.path_workers {
                if let Err(error) = workers.probe(request) {
                    tracing::warn!(%error, "path openability probe unavailable");
                }
            }
        }
        if let Some(window) = self.windows.get(&window_id) {
            if visual_changed {
                if let Some(native) = window.window.as_ref() {
                    native.set_cursor(if window.hover_link {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                window.request_redraw();
            }
        }
    }

    pub(super) fn clear_target_hover(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: `window_id` no longer identifies a live window, there is no hover or probe state left to clear.
            return;
        };
        let probe_changed = window.path_probe.invalidate();
        let hover_changed = window.hovered_url.take().is_some();
        let link_changed = window.hover_link;
        let changed = probe_changed | hover_changed | link_changed;
        window.hover_link = false;
        if changed {
            if let Some(native) = window.window.as_ref() {
                native.set_cursor(winit::window::CursorIcon::Default);
            }
            window.request_redraw();
        }
    }

    pub(super) fn handle_path_probe_finished(&mut self, result: PathProbeResult) {
        let window_id = result.request.key.window_id;
        let fresh = self.pointer_target(window_id).and_then(|target| match target.target {
            ResolvedCellTarget::Path(key) => Some(key),
            ResolvedCellTarget::Uri(_) => None,
        });
        let accepted = self
            .windows
            .get_mut(&window_id)
            .is_some_and(|window| window.path_probe.accept(&result, fresh.as_ref()));
        if accepted {
            self.refresh_target_hover(window_id);
        }
    }

    pub(super) fn open_modifier_held(&self, window_id: WindowId) -> bool {
        let modifiers =
            self.windows.get(&window_id).map(|window| window.modifiers).unwrap_or_default();
        if cfg!(target_os = "macos") {
            // When: `target_os` is macOS, Cmd is the platform-native target activation modifier.
            modifiers.super_key()
        } else {
            // When: `target_os` is not macOS, Ctrl is the platform-native target activation modifier.
            modifiers.control_key()
        }
    }

    pub(super) fn activate_target_at(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> bool {
        let modifier_held = self.open_modifier_held(window_id);
        if !modifier_held {
            // When: `modifier_held` is false at click time, never activate a target authorized by earlier hover state.
            return false;
        }
        let Some(target) = self.cell_target_at(window_id, pane_id, viewport_row, col) else {
            // When: the clicked pane cell has no current `target`, leave the click available to ordinary selection handling.
            return false;
        };
        match target.target {
            ResolvedCellTarget::Uri(uri) => {
                if let Err(error) = sonicterm_cfg::url_open::open(&uri) {
                    tracing::warn!(%error, "URL open failed");
                }
                true
            }
            ResolvedCellTarget::Path(key) => {
                // When: `target.target` is `Path`, activation requires the current typed probe result and bounded opener.
                let Some(selection) = self.windows.get(&window_id).and_then(|window| {
                    window.path_probe.authorized_selection(&key, modifier_held).cloned()
                }) else {
                    // When: `authorized_selection` returns no target, leave a missing, blocked, or stale path to normal selection.
                    return false;
                };
                if !selection.decision.is_actionable() {
                    // When: `selection.decision` is not actionable, never enqueue a blocked or missing target.
                    return false;
                }
                let Some(workers) = &self.path_workers else {
                    // When: `path_workers` yields no `workers`, do not consume a click that cannot reach the bounded opener.
                    return false;
                };
                match workers.open(selection.candidate.resolved_path, selection.decision) {
                    Ok(true) => true,
                    Ok(false) => {
                        tracing::warn!("path open queue full; request dropped");
                        false
                    }
                    Err(error) => {
                        tracing::warn!(%error, "path open unavailable");
                        false
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "path_target_tests.rs"]
mod path_target_tests;
