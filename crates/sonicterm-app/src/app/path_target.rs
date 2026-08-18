//! Contextual filesystem-path detection, validation, and reveal contracts.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use sonicterm_cfg::url_scan::{find_targets_for_style, DetectedTarget, PathStyle, TargetMatch};
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

/// Monotonic identity that prevents stale probe results from surviving ABA transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeEpoch(u64);

impl ProbeEpoch {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Immutable identity of one raw path at one rendered pane cell range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathProbeKey {
    pub(crate) window_id: WindowId,
    pub(crate) pane_id: u64,
    pub(crate) viewport_row: u16,
    pub(crate) absolute_row: u64,
    pub(crate) view_top: u64,
    pub(crate) start_col: u16,
    pub(crate) end_col: u16,
    pub(crate) candidate: String,
    pub(crate) resolved_path: PathBuf,
    pub(crate) cwd: Option<Osc7Cwd>,
    pub(crate) cwd_revision: u64,
    pub(crate) content_seq: u64,
    pub(crate) scrollback_evicted: u64,
    pub(crate) alt_screen: bool,
}

/// One existence request sent to the bounded probe worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeRequest {
    pub(crate) epoch: ProbeEpoch,
    pub(crate) key: PathProbeKey,
}

/// Existence result returned to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeResult {
    pub(crate) request: PathProbeRequest,
    pub(crate) exists: bool,
}

/// Per-window authorization state for the raw path currently under the pointer.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathProbeState {
    epoch: ProbeEpoch,
    current: Option<PathProbeKey>,
    decision: Option<bool>,
}

impl Default for ProbeEpoch {
    fn default() -> Self {
        Self::INITIAL
    }
}

impl PathProbeState {
    pub(super) fn invalidate(&mut self) -> bool {
        let changed = self.current.is_some() || self.decision.is_some();
        if changed {
            self.epoch = self.epoch.next();
            self.current = None;
            self.decision = None;
        }
        changed
    }

    pub(super) fn request(&mut self, key: PathProbeKey) -> Option<PathProbeRequest> {
        if self.current.as_ref() == Some(&key) {
            // When: `key` is already current, retain its epoch and avoid duplicating an in-flight existence probe.
            return None;
        }
        self.epoch = self.epoch.next();
        self.current = Some(key.clone());
        self.decision = None;
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
        if fresh != Some(&result.request.key) {
            // When: `fresh` no longer reproduces the current key, revoke it so a transient read failure can re-probe.
            self.epoch = self.epoch.next();
            self.current = None;
            self.decision = None;
            return false;
        }
        self.decision = Some(result.exists);
        true
    }

    pub(super) fn authorized(&self, key: &PathProbeKey, modifier_held: bool) -> bool {
        modifier_held && self.current.as_ref() == Some(key) && self.decision == Some(true)
    }

    #[cfg(test)]
    pub(super) fn decision_for(&self, key: &PathProbeKey) -> Option<bool> {
        (self.current.as_ref() == Some(key)).then_some(self.decision).flatten()
    }
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

fn path_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// App-owned handles for the bounded path probe and reveal workers.
pub(crate) struct PathWorkers {
    probe: PathProbeMailbox,
    reveal: Sender<PathBuf>,
}

impl PathWorkers {
    /// Start one coalescing existence worker and one serialized reveal worker.
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
                    let exists = path_exists(&request.key.resolved_path);
                    if probe_proxy
                        .send_event(super::UserEvent::PathProbeFinished(PathProbeResult {
                            request,
                            exists,
                        }))
                        .is_err()
                    {
                        // When: `probe_proxy.send_event` fails, the event loop is gone and the worker must terminate.
                        break;
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("spawn path probe worker: {error}")))?;

        let (reveal, reveal_rx) = crossbeam_channel::bounded::<PathBuf>(1);
        std::thread::Builder::new()
            .name("sonicterm-path-reveal".into())
            .spawn(move || {
                while let Ok(path) = reveal_rx.recv() {
                    if let Err(error) = reveal_path(&path) {
                        tracing::warn!(?path, %error, "path reveal failed");
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("spawn path reveal worker: {error}")))?;

        Ok(Self { probe, reveal })
    }

    pub(super) fn probe(&self, request: PathProbeRequest) -> io::Result<()> {
        self.probe
            .submit(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "path probe worker stopped"))
    }

    /// Queue a reveal without blocking the event loop.
    ///
    /// `Ok(false)` means the bounded worker already has one running and one
    /// waiting request; the click is still consumed and the extra reveal drops.
    pub(super) fn reveal(&self, path: PathBuf) -> io::Result<bool> {
        match self.reveal.try_send(path) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "path reveal worker stopped"))
            }
        }
    }
}

/// Platform-neutral command description used by reveal tests without spawning handlers.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandSpec {
    pub(super) program: PathBuf,
    pub(super) args: Vec<String>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
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
        // When: the native reveal command returns a failed `status`, surface it instead of reporting a successful click.
        Err(io::Error::other(format!("reveal command exited with {status}")))
    }
}

#[cfg(target_os = "macos")]
fn reveal_path(path: &Path) -> io::Result<()> {
    std::fs::metadata(path)?;
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    let spec = macos_reveal_spec(text)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid macOS path"))?;
    run_command(spec)
}

#[cfg(target_os = "windows")]
fn reveal_path(path: &Path) -> io::Result<()> {
    std::fs::metadata(path)?;
    let system_windows = system_windows_directory()?;
    let target = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    let spec = windows_reveal_spec(&system_windows, target)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid Windows path"))?;
    run_command(spec)
}

#[cfg(target_os = "windows")]
fn system_windows_directory() -> io::Result<String> {
    use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

    query_system_windows_directory(|buffer| {
        // SAFETY: `buffer` is writable for its reported length and remains
        // alive for the call; the helper validates the API's returned size.
        unsafe { GetSystemWindowsDirectoryW(Some(buffer)) }
    })
}

#[cfg(any(target_os = "windows", test))]
fn query_system_windows_directory(mut query: impl FnMut(&mut [u16]) -> u32) -> io::Result<String> {
    let mut capacity = 260usize;
    loop {
        let mut buffer = vec![0u16; capacity];
        let length = query(&mut buffer) as usize;
        if length == 0 {
            // When: `length` is zero, preserve the Windows directory query's operating-system error.
            return Err(io::Error::last_os_error());
        }
        if length >= capacity {
            // When: `length` reaches `capacity`, retry with the API-reported required buffer size.
            capacity = length.saturating_add(1);
            if capacity > 32_768 {
                // When: the required `capacity` exceeds the Windows path ceiling, reject an untrusted allocation size.
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Windows directory too long",
                ));
            }
            continue;
        }
        if buffer.get(length).copied() != Some(0) {
            // When: the Windows directory buffer lacks a NUL at `length`, reject the malformed API result.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows directory was not terminated",
            ));
        }
        buffer.truncate(length);
        let raw = String::from_utf16(&buffer)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Windows directory"))?;
        return normalize_windows_absolute(&raw).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "untrusted Windows directory")
        });
    }
}

#[cfg(any(target_os = "linux", test))]
fn with_opened_reveal_target<T>(
    path: &Path,
    reveal: impl FnOnce(&std::fs::File) -> io::Result<T>,
) -> io::Result<T> {
    let file = std::fs::File::open(path)?;
    reveal(&file)
}

#[cfg(target_os = "linux")]
fn reveal_path(path: &Path) -> io::Result<()> {
    use ashpd::desktop::open_uri::OpenDirectoryRequest;

    with_opened_reveal_target(path, |file| {
        async_io::block_on(async {
            let request = OpenDirectoryRequest::default()
                .send(file)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            request.response().map_err(|error| io::Error::other(error.to_string()))
        })
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn reveal_path(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "path reveal is unsupported"))
}

pub(super) fn target_at_row_cell(row: &Row, col: u16, style: PathStyle) -> Option<RowTarget> {
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
    let clicked_byte = byte_ranges.get(col)?.0;
    let matched = find_targets_for_style(&text, style)
        .into_iter()
        .find(|matched| clicked_byte >= matched.start && clicked_byte < matched.end)?;
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

pub(super) fn resolve_path_candidate(
    candidate: &str,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    local_hostname: &str,
) -> Option<PathBuf> {
    let relative = is_explicit_relative(candidate, style);
    let combined = if relative {
        let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
        match style {
            PathStyle::Posix => format!("{}/{}", cwd.path.trim_end_matches('/'), candidate),
            PathStyle::Windows => {
                let cwd = normalize_windows_cwd(&cwd.path)?;
                format!("{}\\{}", cwd.trim_end_matches(['/', '\\']), candidate)
            }
        }
    } else {
        // When: `candidate` is absolute rather than `relative`, resolve it without consulting OSC 7 state.
        candidate.to_string()
    };
    match style {
        PathStyle::Posix => normalize_posix_absolute(&combined).map(PathBuf::from),
        PathStyle::Windows => normalize_windows_absolute(&combined).map(PathBuf::from),
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_reveal_spec(path: &str) -> Option<CommandSpec> {
    let path = normalize_posix_absolute(path)?;
    Some(CommandSpec {
        program: PathBuf::from("/usr/bin/open"),
        args: vec!["-R".to_string(), path],
    })
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn windows_reveal_spec(
    system_windows_directory: &str,
    path: &str,
) -> Option<CommandSpec> {
    let windows_dir = normalize_windows_absolute(system_windows_directory)?;
    let target = normalize_windows_absolute(path)?;
    let program_text = if windows_dir.ends_with('\\') {
        format!("{windows_dir}explorer.exe")
    } else {
        // When: `windows_dir` lacks a trailing separator, add one before the trusted Explorer basename.
        format!("{windows_dir}\\explorer.exe")
    };
    let program = normalize_windows_absolute(&program_text)?;
    let prefix = windows_dir.trim_end_matches('\\');
    if !program.to_ascii_lowercase().starts_with(&format!("{}\\", prefix.to_ascii_lowercase()))
        && !windows_dir.ends_with('\\')
    {
        // When: normalized `program` escapes the trusted `windows_dir` prefix, reject executable construction.
        return None;
    }
    Some(CommandSpec { program: PathBuf::from(program), args: vec![format!("/select,{target}")] })
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

fn unsafe_path_cell(cell: &&Cell) -> bool {
    cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT)
        || cell.extras().is_some_and(|extras| !extras.is_empty())
}

fn authority_is_local(authority: &str, local_hostname: &str) -> bool {
    authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || (!local_hostname.is_empty() && authority.eq_ignore_ascii_case(local_hostname))
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

        let row_target = target_at_row_cell(row, col, PathStyle::native())?;
        match row_target.matched.target {
            DetectedTarget::Uri(uri) => Some(CellTargetSnapshot {
                pane_id,
                viewport_row,
                start_col: row_target.start_col,
                end_col: row_target.end_col,
                display: uri.clone(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Uri(uri),
            }),
            DetectedTarget::PathCandidate(candidate) => {
                let cwd = parser.osc7_cwd().cloned();
                let resolved_path = resolve_path_candidate(
                    &candidate,
                    PathStyle::native(),
                    cwd.as_ref(),
                    &self.local_hostname,
                )?;
                let key = PathProbeKey {
                    window_id,
                    pane_id,
                    viewport_row,
                    absolute_row,
                    view_top,
                    start_col: row_target.start_col,
                    end_col: row_target.end_col,
                    candidate: candidate.clone(),
                    resolved_path,
                    cwd,
                    cwd_revision: parser.cwd_revision(),
                    content_seq: grid.content_seq(),
                    scrollback_evicted: grid.scrollback_evicted(),
                    alt_screen: grid.is_alt(),
                };
                Some(CellTargetSnapshot {
                    pane_id,
                    viewport_row,
                    start_col: row_target.start_col,
                    end_col: row_target.end_col,
                    display: candidate,
                    explicit_hyperlink: false,
                    target: ResolvedCellTarget::Path(key),
                })
            }
        }
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
                    if window.path_probe.authorized(key, modifier_held) {
                        hovered = Some(target.hovered(true));
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
                    tracing::warn!(%error, "path existence probe unavailable");
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
                // When: `target` is a raw path, require the current existence-probe key before queueing reveal work.
                let authorized = self
                    .windows
                    .get(&window_id)
                    .is_some_and(|window| window.path_probe.authorized(&key, modifier_held));
                if !authorized {
                    // When: `authorized` is false, reject a missing or stale raw-path click.
                    return false;
                }
                if let Some(workers) = &self.path_workers {
                    // When: `path_workers` is available, queue reveal-only work without blocking the event loop.
                    match workers.reveal(key.resolved_path) {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!("path reveal queue full; request dropped"),
                        Err(error) => tracing::warn!(%error, "path reveal unavailable"),
                    }
                }
                true
            }
        }
    }
}

#[cfg(test)]
#[path = "path_target_tests.rs"]
mod path_target_tests;
