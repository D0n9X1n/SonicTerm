pub(super) use super::linux::{linux_xdg_open_spec, local_file_uri, with_opened_target};
pub(super) use super::macos::{macos_directory_policy, macos_open_spec, macos_reveal_spec};
pub(super) use super::windows_os::windows_path_policy;
use super::*;
pub(super) use sonicterm_cfg::{
    config::Config, keymap::Keymap, theme::Theme, url_scan::DetectedTarget,
};
pub(super) use sonicterm_grid::grid::{Cell, CellFlags, Color, Row};
pub(super) use sonicterm_types::HyperlinkId;

// Temp roots can include a symlink (macOS) or a junction (Windows); resolve the fixture root on
// every platform, never the target under test.
pub(super) fn native_test_root() -> PathBuf {
    plain_test_root(std::env::temp_dir().canonicalize().unwrap())
}

// Unix canonical paths are already in the plain form the classifier accepts.
#[cfg(not(windows))]
pub(super) fn plain_test_root(root: PathBuf) -> PathBuf {
    root
}

// Windows canonical paths carry a verbatim `\\?\C:\` prefix, which the classifier rejects as a
// UNC form; rebuild the ordinary drive path from the same resolved components.
#[cfg(windows)]
pub(super) fn plain_test_root(root: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    let drive = match root.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => Some(letter),
            _ => None,
        },
        _ => None,
    };
    let Some(drive) = drive else {
        return root;
    };
    let rest =
        root.components().skip(1).filter(|component| !matches!(component, Component::RootDir));
    let mut plain = PathBuf::from(format!("{}:\\", drive as char));
    plain.extend(rest);
    plain
}

/// Creates `~/.claude/settings.json` with `create_settings` and `~/.claude.json` as a regular file in a scratch
/// home, then checks both paths in the Chinese sentence in a main and a child window at two widths.
pub(super) fn assert_home_prose_paths_resolve(prefix: &str, create_settings: impl Fn(&Path)) {
    let root = native_test_root().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    create_settings(&root);
    std::fs::write(root.join(".claude.json"), "inert fixture").unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.home_dir = Some(root.clone());
    app.__test_seed_tab("home prose main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["home prose child"]);
    let text = "同步时会将客户端设置写入 ~/.claude/settings.json，将 MCP 配置块写入顶层的 ~/.claude.json，并将权限";
    for window in [main, child] {
        let pane = *app.windows[&window].panes.keys().next().unwrap();
        app.frontmost_window = Some(window);
        app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
            winit::keyboard::ModifiersState::SUPER
        } else {
            winit::keyboard::ModifiersState::CONTROL
        };
        for width in [160, 40] {
            let parser = app.windows[&window].panes[&pane].parser.clone();
            {
                let mut parser = parser.lock();
                parser.resize(width, 8);
                parser.advance(format!("\x1b[2J\x1b[H{text}").as_bytes());
                assert!(parser.osc7_cwd().is_none());
            }
            let cells = parser
                .lock()
                .grid()
                .rows_iter()
                .enumerate()
                .flat_map(|(row, line)| {
                    line.iter().enumerate().map(move |(col, cell)| {
                        (AbsoluteCell { row: row as u64, col: col as u16 }, cell.ch)
                    })
                })
                .collect::<Vec<_>>();
            let starts = cells
                .iter()
                .enumerate()
                .filter_map(|(index, (_, character))| (*character == '~').then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(starts.len(), 2);
            for (start, (path, suffix)) in starts
                .into_iter()
                .zip([(".claude/settings.json", "，将"), (".claude.json", "，并将权限")])
            {
                let literal = root.join(format!("{path}{suffix}"));
                let expected = root.join(path);
                let end = start + path.len() + 2;
                for (pointed, _) in &cells[start..end] {
                    let snapshot = app
                        .cell_target_at(window, pane, pointed.row as u16, pointed.col)
                        .expect("home path must have candidates without OSC 7");
                    let ResolvedCellTarget::Path(key) = snapshot.target.clone() else {
                        panic!("path target")
                    };
                    let selection = probe_candidates(&key.candidates, classify_local_target)
                        .expect("the shortest existing home file is selected");
                    assert_eq!(selection.candidate.resolved_path, expected);
                    assert_eq!(selection.candidate.char_len(), path.len() + 2);
                    assert!(!selection
                        .candidate
                        .spans
                        .iter()
                        .any(|span| span.contains(cells[end].0)));
                    let request =
                        app.windows.get_mut(&window).unwrap().path_probe.request(key.clone());
                    if let Some(request) = request {
                        let result =
                            PathProbeResult { request, selection: Some(selection), failure: None };
                        assert!(app
                            .windows
                            .get_mut(&window)
                            .unwrap()
                            .path_probe
                            .accept(&result, Some(&key)));
                    }
                    app.apply_target_hover(window, Some(snapshot));
                    assert!(app.windows[&window].path_probe.authorized(&key, true));
                    assert_eq!(
                        PathBuf::from(&app.windows[&window].link_preview.as_ref().unwrap().uri),
                        expected
                    );
                    // A blocked or existing longer literal cannot displace the shorter existing file.
                    assert_eq!(
                        probe_candidates(&key.candidates, |candidate| {
                            if candidate == literal {
                                PathOpenDecision::Blocked
                            } else {
                                classify_local_target(candidate)
                            }
                        })
                        .unwrap()
                        .candidate
                        .resolved_path,
                        expected
                    );
                    std::fs::write(&literal, "literal fixture").unwrap();
                    assert_eq!(
                        probe_candidates(&key.candidates, classify_local_target)
                            .unwrap()
                            .candidate
                            .resolved_path,
                        expected
                    );
                    std::fs::remove_file(&literal).unwrap();
                }
                // Both cells of the wide comma and the following prose must not borrow the shorter path's authority.
                for (pointed, _) in &cells[end..end + suffix.chars().count() * 2] {
                    if let Some(snapshot) =
                        app.cell_target_at(window, pane, pointed.row as u16, pointed.col)
                    {
                        if let ResolvedCellTarget::Path(key) = snapshot.target {
                            assert!(
                                probe_candidates(&key.candidates, classify_local_target).is_err()
                            );
                            assert!(!key
                                .candidates
                                .iter()
                                .any(|candidate| candidate.resolved_path == expected));
                        }
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Clone, Default)]
pub(super) struct PathDiagnosticLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for PathDiagnosticLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Capture only this thread's configured path diagnostics without uncaptured siblings disabling their call sites.
pub(super) fn capture_path_diagnostics(
    level: sonicterm_logging::LogLevel,
    action: impl FnOnce(),
) -> String {
    let log = PathDiagnosticLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(sonicterm_logging::filter_for_level(level))
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let output = log.0.lock().unwrap().clone();
    String::from_utf8(output).unwrap()
}

/// Build a child pane of `cols` by `rows`, feed `output`, and resolve the target at one cell.
pub(super) fn target_after(
    cols: u16,
    rows: u16,
    output: &str,
    row: u16,
    col: u16,
) -> Option<CellTargetSnapshot> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["multiplexer"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.cell_target_at(window, pane, row, col)
}

/// Resolve the target at one cell of a fed pane and return its hover spans.
pub(super) fn hover_spans_after(
    cols: u16,
    rows: u16,
    output: &str,
    row: u16,
    col: u16,
) -> Vec<sonicterm_render_model::inputs::HoveredUrlSpan> {
    target_after(cols, rows, output, row, col).unwrap().hover_cells.unwrap().spans().to_vec()
}

pub(super) fn hover_span(
    row: u16,
    start_col: u16,
    end_col: u16,
) -> sonicterm_render_model::inputs::HoveredUrlSpan {
    sonicterm_render_model::inputs::HoveredUrlSpan { row, start_col, end_col }
}

/// Draw a vertical pane border down one 1-based column on every row, as tmux does for a split.
pub(super) fn pane_border(column: u16, rows: u16) -> String {
    (1..=rows).map(|row| format!("\x1b[{row};{column}H\u{2502}")).collect()
}

/// Return a native absolute path ending in `tail`, in this build's path grammar.
pub(super) fn native_path(tail: &str) -> String {
    let root = if cfg!(windows) { "C:/" } else { "/" };
    format!("{root}{tail}")
}

pub(super) fn ascii_row(text: &str) -> Row {
    Row::from_flat(
        text.chars()
            .map(|character| {
                Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
            })
            .collect(),
    )
}

pub(super) fn probe_candidate_at(
    display: &str,
    path: &str,
    start_col: u16,
    row: u64,
) -> PathProbeCandidate {
    PathProbeCandidate {
        spans: smallvec::smallvec![AbsoluteCellSpan {
            row,
            start_col,
            end_col: start_col + u16::try_from(display.chars().count()).unwrap(),
        }],
        target: DetectedTarget::PathCandidate(display.into()),
        resolved_path: PathBuf::from(path),
        joined: false,
    }
}

pub(super) fn probe_candidate(display: &str, path: &str, start_col: u16) -> PathProbeCandidate {
    probe_candidate_at(display, path, start_col, 22)
}

pub(super) fn probe_key(path: &str, view_top: u64) -> PathProbeKey {
    let row = view_top + 2;
    PathProbeKey {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        pointed: AbsoluteCell { row, col: 4 },
        view_top,
        candidates: vec![probe_candidate_at("./file", path, 4, row)],
        rows: smallvec::smallvec![PathRowIdentity { row, fingerprint: 11 }],
        cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
        cwd_revision: 3,
        link_destination: None,
        scrollback_evicted: 0,
        screen_epoch: 0,
        alt_screen: false,
    }
}

pub(super) fn openable_result(request: PathProbeRequest) -> PathProbeResult {
    PathProbeResult {
        failure: None,
        selection: Some(PathProbeSelection {
            candidate: request.key.candidates[0].clone(),
            decision: PathOpenDecision::Openable(PathKind::File),
        }),
        request,
    }
}

// Generous bound for a loaded CI host; a healthy worker answers in microseconds.
pub(super) const HELD_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
// Short settle window used only to prove that no further probe starts.
pub(super) const IDLE_PROBE_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// Production worker loop whose classifier parks each probe until the test releases it.
///
/// Holding one execution lets a test queue work from several windows behind it
/// and then observe exactly which request the worker serves next.
pub(super) struct HeldProbeWorker {
    pub(super) mailbox: PathProbeMailbox,
    pub(super) started: Receiver<PathBuf>,
    release: Sender<()>,
    results: Receiver<PathProbeResult>,
}

impl HeldProbeWorker {
    pub(super) fn spawn() -> Self {
        let (mailbox, wake) = PathProbeMailbox::new();
        let queue = Arc::clone(&mailbox.queue);
        let (started_tx, started) = crossbeam_channel::unbounded();
        let (release, release_rx) = crossbeam_channel::unbounded::<()>();
        let (results_tx, results) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            run_probe_worker(
                &wake,
                &queue,
                |path| {
                    let _ = started_tx.send(path.to_path_buf());
                    // A dropped harness releases the worker so its thread can exit.
                    if release_rx.recv().is_ok() {
                        PathOpenDecision::Openable(PathKind::File)
                    } else {
                        PathOpenDecision::Missing
                    }
                },
                |result| results_tx.send(result).is_ok(),
                None,
            );
        });
        Self { mailbox, started, release, results }
    }

    /// Wait until the worker parks on `path`, proving it chose that request next.
    pub(super) fn expect_started(&self, path: &str) {
        let started = self
            .started
            .recv_timeout(HELD_PROBE_TIMEOUT)
            .unwrap_or_else(|_| panic!("worker never started a probe; expected {path}"));
        assert_eq!(started.as_path(), Path::new(path), "worker served the wrong request next");
    }

    /// Release the parked probe and return its delivered result.
    pub(super) fn finish(&self) -> PathProbeResult {
        self.release.send(()).unwrap();
        self.results.recv_timeout(HELD_PROBE_TIMEOUT).expect("held probe delivers a result")
    }

    /// Confirm the worker takes `path` next, then complete it.
    pub(super) fn serve(&self, path: &str) -> PathProbeResult {
        self.expect_started(path);
        self.finish()
    }

    /// Prove nothing is waiting and no further probe starts once the queue drains.
    pub(super) fn assert_idle(&self) {
        assert_eq!(self.mailbox.waiting_len(), 0);
        assert!(self.started.recv_timeout(IDLE_PROBE_WAIT).is_err(), "no further probe runs");
    }
}

/// Probe key owned by synthetic window `window`, so several windows can race one worker.
pub(super) fn window_probe_key(window: u64, path: &str) -> PathProbeKey {
    PathProbeKey { window_id: WindowId::from(window), ..probe_key(path, 0) }
}

/// Probe key owned by a seeded App window.
pub(super) fn seeded_probe_key(window: WindowId, path: &str) -> PathProbeKey {
    PathProbeKey { window_id: window, ..probe_key(path, 0) }
}

/// One redraw's hover refresh for a stationary local path under the pointer.
pub(super) fn hover_probe_key(app: &mut App, window: WindowId, key: &PathProbeKey) {
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            label: None,
            pane_id: key.pane_id,
            hover_cells: None,
            display: "./file".into(),
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key.clone()),
        }),
    );
}

/// Route an App's probe handle to `worker`; these tests never dispatch an open.
pub(super) fn install_held_worker(app: &mut App, worker: &HeldProbeWorker) {
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers {
        probe: worker.mailbox.clone(),
        open,
        busy: Arc::new(crate::app::jobs::PathBusy::new()),
    });
}

// Seed one child pane at `cols`x`rows` and feed `output` through the real parser.
pub(super) fn wrapped_url_pane(
    app: &mut App,
    cols: u16,
    rows: u16,
    output: &[u8],
) -> (WindowId, u64) {
    let window = app.__test_seed_child_window(&["wrapped url"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    (window, pane)
}

pub(super) const ISSUE_URI: &str = "https://github.com/D0n9X1n/SonicTerm/issues/1349";

// Emit `lines` as application hard rows (CRLF) into one pane `cols` wide and `rows` tall.
pub(super) fn hardwrap_pane_sized(
    app: &mut App,
    cols: u16,
    rows: u16,
    lines: &[String],
) -> (WindowId, u64) {
    let window = app.__test_seed_child_window(&["hard wrap"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, lines.join("\r\n").as_bytes()));
    (window, pane)
}

pub(super) fn hardwrap_pane(app: &mut App, cols: u16, lines: &[String]) -> (WindowId, u64) {
    hardwrap_pane_sized(app, cols, 10, lines)
}

// Fill each non-final row exactly, leaving the closing wrapper on the final row.
pub(super) fn hardwrap_lines(
    cols: u16,
    head: &str,
    uri: &str,
    indents: &[usize],
    closer: char,
) -> Vec<String> {
    let cols = usize::from(cols);
    assert!(head.is_ascii() && uri.is_ascii());
    assert!(head.len() < cols, "head must leave room for the first fragment");
    let (first, mut rest) = uri.split_at(cols - head.len());
    let mut lines = vec![format!("{head}{first}")];
    for (index, indent) in indents.iter().enumerate() {
        let pad = " ".repeat(*indent);
        if index + 1 == indents.len() {
            assert!(indent + rest.len() < cols, "final fragment and closer must fit");
            lines.push(format!("{pad}{rest}{closer}"));
        } else {
            assert!(rest.len() > cols - indent, "uri too short for {} rows", indents.len() + 1);
            let (chunk, tail) = rest.split_at(cols - indent);
            lines.push(format!("{pad}{chunk}"));
            rest = tail;
        }
    }
    lines
}

pub(super) const HARDWRAP_HEAD: &str = "* Done: #1349 (";
pub(super) const LONG_URI: &str = "https://example.com/aaaaaaaaaa/bbbbbbbbbb/cccccccccc/dddddddddd";

/// Lines written into the trimmed-history grid; history keeps the last 1,000 that scrolled out.
pub(super) const TRIMMED_HISTORY_LINES: usize = 1_100;

/// Generated line `line`: words, CJK wide pairs, a URL, a path, or a prompt with a coloured tail.
pub(super) fn trimmed_history_line(line: usize) -> String {
    match line % 5 {
        0 => format!("alpha beta {line} gamma"),
        1 => format!("中文{line}字"),
        2 => format!("see https://example.com/page{line} now"),
        3 => format!("edit ./src/file{line}.rs now"),
        _ => format!("prompt{line}$"),
    }
}

/// A 200x24 grid whose 1,000 history rows are the generated short lines, stored trimmed.
pub(super) fn grid_with_trimmed_history() -> Grid {
    let coloured = Cell::plain(' ', Color::Default, Color::Indexed(4), CellFlags::empty());
    let mut grid = Grid::new(200, 24);
    for line in 0..TRIMMED_HISTORY_LINES {
        for character in trimmed_history_line(line).chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
        if line % 5 == 4 {
            grid.erase_line_to_end_with(coloured.clone());
        }
        grid.carriage_return();
        grid.linefeed();
    }
    grid
}

/// The absolute row whose text starts with `prefix`, searched over history and the live screen.
pub(super) fn absolute_row_starting_with(grid: &Grid, prefix: &str) -> u64 {
    let stored_rows = grid.scrollback_len() as u64 + u64::from(grid.rows);
    (0..stored_rows)
        .find(|row| {
            grid.row_at_abs(*row).is_some_and(|line| {
                line.iter().map(|cell| cell.ch).collect::<String>().starts_with(prefix)
            })
        })
        .expect("a row with the prefix")
}

/// A child-window pane of `cols` × `rows` cells that has parsed `output`.
pub(super) fn pane_after(cols: u16, rows: u16, output: &str) -> (App, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["frame scope"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    (app, window, pane)
}

/// An App that counts hover work, with an injected probe mailbox the test drains itself, and one
/// child window whose pane is `cols` × `rows` cells.
/// The injected probe mailbox and its wake receiver, kept alive so submits reach a live worker
/// and stay queued for the test to count.
pub(super) struct HoverMailbox {
    pub(super) mailbox: PathProbeMailbox,
    _wake: Receiver<()>,
}

impl HoverMailbox {
    /// The next waiting request, as the worker would take it.
    pub(super) fn take_next(&self) -> Option<PathProbeRequest> {
        self.mailbox.take_next()
    }
}

pub(super) fn counted_hover_app(cols: u16, rows: u16) -> (App, WindowId, u64, HoverMailbox) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let (mailbox, wake) = PathProbeMailbox::new();
    let (open, _) = crossbeam_channel::bounded(1);
    app.path_workers = Some(PathWorkers {
        probe: mailbox.clone(),
        open,
        busy: Arc::new(crate::app::jobs::PathBusy::new()),
    });
    let window = app.__test_seed_child_window(&["hover cache"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(cols, rows);
    (app, window, pane, HoverMailbox { mailbox, _wake: wake })
}

/// One hover lookup of `pane`'s cell (`row`, `col`) as a frame runs it from its held parser, at
/// the pane's own viewport or at `viewport`, then the apply that always follows.
pub(super) fn frame_hover_at(
    app: &mut App,
    window: WindowId,
    pane: u64,
    (row, col): (u16, u16),
    viewport: Option<u64>,
) {
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    let guard = parser.lock();
    let viewport =
        viewport.or_else(|| app.windows[&window].panes[&pane].resolved_viewport(guard.grid()));
    let target = app.hover_target_from_parser(window, pane, row, col, &guard, viewport);
    drop(guard);
    app.apply_target_hover(window, target);
}

/// [`frame_hover_at`] at the pane's own viewport.
pub(super) fn frame_hover(app: &mut App, window: WindowId, pane: u64, cell: (u16, u16)) {
    frame_hover_at(app, window, pane, cell, None);
}

/// The App's hover counters as `(lookups, detections, cache_hits)`.
pub(super) fn hover_counts(app: &App) -> (u64, u64, u64) {
    let hover = app.frame_counters.as_ref().expect("counters are on").hover;
    (hover.lookups, hover.detections, hover.cache_hits)
}

/// Probe requests hover submitted since the last drain.
pub(super) fn drained_probes(hover: &HoverMailbox) -> usize {
    std::iter::from_fn(|| hover.mailbox.take_next()).count()
}

/// Hold or release the platform's open modifier in `window`.
pub(super) fn set_open_modifier(app: &mut App, window: WindowId, held: bool) {
    let modifier = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    app.windows.get_mut(&window).unwrap().modifiers =
        if held { modifier } else { winit::keyboard::ModifiersState::empty() };
}

#[path = "test_hooks_path_target_corpus_fixture.rs"]
mod test_hooks_path_target_corpus_fixture;

#[path = "test_hooks_path_target_recording_fixture.rs"]
mod test_hooks_path_target_recording_fixture;
