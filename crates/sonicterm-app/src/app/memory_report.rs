//! The Memory Usage report: its data model, its plain-text rendering, and the
//! rules for which actions and keys the report window handles itself.
//!
//! The report is built from the same sampling cycle as the `memory snapshot`
//! log line ([`super::memory_snapshot`]), plus per-pane and per-window rows and
//! OS figures a worker thread collects. It is rendered once into a string; that
//! string is both what the report pane shows and exactly what copy-all copies,
//! so a narrow window that wraps the pane never changes the copied text.
//!
//! Text that comes from outside SonicTerm (tab titles, process names) passes
//! through [`sanitize_external`] before it reaches the report, so nothing it
//! contains can act as a terminal escape sequence when the report is fed to the
//! report pane's parser.

use std::fmt::Write as _;

use sonicterm_cfg::keymap::Action;
use sonicterm_logging::process_memory::MemoryMetric;
use sonicterm_logging::process_report::{
    ProcessFootprint, ProcessTree, ThreadSummary, WalkOutcome, CHILD_MEMORY_LABEL,
    MAX_DESCENDANTS_PER_ROOT,
};
use sonicterm_types::ResourceAmount;

use super::memory_snapshot::{MemorySnapshot, RendererSummary};
use super::retention::PaneRetention;

/// History rows the report pane keeps, independent of `[terminal] scrollback`, so a long
/// report is never trimmed by a small user setting.
pub(crate) const REPORT_HISTORY_ROWS: usize = 10_000;

/// Widest line the thread-name list wraps to.
const THREAD_LINE_WIDTH: usize = 96;

/// Bytes in one binary megabyte, the unit every figure in the report uses.
const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// The report's first line: what, when, and which process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportHeader {
    /// SonicTerm's version.
    pub(crate) version: String,
    /// Local wall-clock time the sample was taken.
    pub(crate) timestamp: String,
    /// This process's id.
    pub(crate) pid: u32,
    /// The platform's spelling of the copy-all chord, shown in the hint line.
    pub(crate) copy_all_hint: &'static str,
}

/// One window's renderer in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowRow {
    /// `window 1`, `window 2`, or `memory report window`.
    pub(crate) label: String,
    /// True for the report window itself, whose renderer the report adds.
    pub(crate) is_report: bool,
    /// The renderer's retained storage, or `None` when the window has no renderer yet.
    pub(crate) renderer: Option<RendererSummary>,
}

/// One pane in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRow {
    /// `tab 1 "zsh" pane 1`, already sanitized.
    pub(crate) label: String,
    /// True for the report's own pane.
    pub(crate) is_report: bool,
    /// What the pane retains, or `None` when its lock was held and it was skipped.
    pub(crate) retention: Option<PaneRetention>,
}

/// One pane shell whose process tree the report walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootRow {
    /// `tab 1 pane 1`, matching the pane's row.
    pub(crate) label: String,
    /// The shell's pid, or `None` when the pane has no live process id.
    pub(crate) pid: Option<u32>,
    /// The shell's start token from spawn time, used to reject a reused pid.
    pub(crate) start: Option<u64>,
}

/// OS figures sampled off the UI thread.
///
/// Its fields are crate-visible so the event that carries it back can be built by the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsSample {
    /// This process's footprint and peak.
    pub(crate) footprint: ProcessFootprint,
    /// Seconds since the process started.
    pub(crate) uptime_secs: Option<u64>,
    /// This process's threads.
    pub(crate) threads: ThreadSummary,
    /// One tree per [`RootRow`] with a pid, in root order.
    pub(crate) trees: Vec<ProcessTree>,
}

/// Where the report's OS figures stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OsFigures {
    /// The worker has not answered yet.
    Pending,
    /// The worker could not be started, so no OS figure exists for this sample.
    Failed,
    /// The worker's answer.
    Sampled(OsSample),
}

/// Everything one report shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryReport {
    /// First line.
    pub(crate) header: ReportHeader,
    /// The shared sampling cycle: process figures, session totals, renderers.
    pub(crate) snapshot: MemorySnapshot,
    /// Visible windows, main first, the report window last.
    pub(crate) windows: Vec<WindowRow>,
    /// Every pane, in window, tab and pane order.
    pub(crate) panes: Vec<PaneRow>,
    /// Pane shells, in pane order.
    pub(crate) roots: Vec<RootRow>,
    /// OS figures, filled in when the worker answers.
    pub(crate) os: OsFigures,
}

/// Replace every control character in text from outside SonicTerm with a space.
///
/// Covers C0 (including ESC and BEL), DEL and C1, so an OSC or CSI sequence in a tab title
/// or a process name renders as inert characters rather than reaching the parser as one.
pub(crate) fn sanitize_external(text: &str) -> String {
    text.chars().map(|character| if character.is_control() { ' ' } else { character }).collect()
}

/// Bytes as megabytes with one decimal, or gigabytes past 1024 MB.
fn format_bytes(bytes: u64) -> String {
    let megabytes = bytes as f64 / BYTES_PER_MB;
    if megabytes >= 1024.0 {
        // When: `megabytes` passes 1024, gigabytes keep the figure readable.
        return format!("{:.1} GB", megabytes / 1024.0);
    }
    format!("{megabytes:.1} MB")
}

/// A counted amount's bytes, in the report's unit.
fn format_amount(amount: ResourceAmount) -> String {
    format_bytes(amount.bytes as u64)
}

/// An OS figure, or `not available` when the platform did not measure it.
fn format_metric(metric: MemoryMetric) -> String {
    metric.bytes().map_or_else(|| "not available".to_string(), format_bytes)
}

/// A signed byte difference in megabytes, always with its sign.
fn format_signed_bytes(bytes: i64) -> String {
    let megabytes = bytes as f64 / BYTES_PER_MB;
    format!("{megabytes:+.1} MB")
}

/// Seconds as `9d 3h`, `3h 12m`, `12m 5s` or `5s`.
fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    if days > 0 {
        // Past a day, hours are the finest unit worth showing.
        format!("{days}d {hours}h")
    } else if hours > 0 {
        // When: `hours` is nonzero under a day, minutes are the finest unit worth showing.
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        // When: `minutes` is nonzero under an hour, seconds complete the figure.
        format!("{minutes}m {seconds}s")
    } else {
        // When: `minutes` is zero, the process is under a minute old, so seconds alone.
        format!("{seconds}s")
    }
}

/// A count with thousands separators, as `9,812`.
fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Append one line and its newline.
fn push_line(text: &mut String, line: &str) {
    text.push_str(line.trim_end());
    text.push('\n');
}

/// A left column padded to `width` followed by `value`.
fn columns(label: &str, width: usize, value: &str) -> String {
    format!("{label:<width$}{value}")
}

impl MemoryReport {
    /// Bytes SonicTerm counts itself: every renderer plus every measured pane.
    fn accounted_bytes(&self) -> u64 {
        (self.snapshot.renderer_bytes() as u64).saturating_add(self.snapshot.session_bytes() as u64)
    }

    /// Render the whole report as plain text, one `\n` per line.
    pub(crate) fn render(&self) -> String {
        let mut text = String::new();
        self.render_header(&mut text);
        self.render_process(&mut text);
        self.render_accounting(&mut text);
        self.render_windows(&mut text);
        self.render_panes(&mut text);
        self.render_threads(&mut text);
        self.render_children(&mut text);
        text
    }

    /// Title line and the usage hint.
    fn render_header(&self, text: &mut String) {
        let header = &self.header;
        let uptime = match &self.os {
            OsFigures::Sampled(sample) => sample.uptime_secs.map(format_uptime),
            OsFigures::Pending | OsFigures::Failed => None,
        };
        let mut title = format!(
            "SonicTerm {} memory report - {} - pid {}",
            header.version, header.timestamp, header.pid
        );
        if let Some(uptime) = uptime {
            // When: `uptime` is known from the OS sample, append it; a String write cannot fail.
            let _ = write!(title, " - uptime {uptime}");
        }
        push_line(text, &title);
        push_line(
            text,
            &format!(
                "(Select text and copy it, or press {} for the whole report. R refreshes, Esc closes.)",
                header.copy_all_hint
            ),
        );
        push_line(text, "");
    }

    /// What the OS says about the whole process.
    fn render_process(&self, text: &mut String) {
        push_line(text, "PROCESS (from the OS)");
        let process = &self.snapshot.process;
        match &self.os {
            OsFigures::Sampled(sample) => {
                let footprint = &sample.footprint;
                push_line(
                    text,
                    &format!(
                        "  {}   {} {}",
                        columns(footprint.current_label, 22, &format_metric(footprint.current)),
                        footprint.peak_label,
                        format_metric(footprint.peak)
                    ),
                );
            }
            OsFigures::Pending => {
                push_line(text, "  footprint             sampling the OS figures...");
            }
            OsFigures::Failed => {
                push_line(
                    text,
                    "  footprint             not available (the OS sample did not start)",
                );
            }
        }
        push_line(
            text,
            &format!("  {}", columns("resident (RSS)", 22, &format_metric(process.resident))),
        );
        push_line(
            text,
            &format!(
                "  {}",
                columns("private committed", 22, &format_metric(process.private_committed))
            ),
        );
        push_line(
            text,
            &format!(
                "  {}   (reserved address space, not consumption)",
                columns("virtual", 22, &format_metric(process.virtual_bytes))
            ),
        );
        push_line(text, &format!("  {}", columns("GPU", 22, "not available yet")));
        push_line(text, "");
    }

    /// SonicTerm's own counted figures, the signed difference, and sample coverage.
    fn render_accounting(&self, text: &mut String) {
        let accounted = self.accounted_bytes();
        push_line(text, &columns("ACCOUNTED BY SONICTERM", 26, &format_bytes(accounted)));
        push_line(
            text,
            &format!(
                "  {}",
                columns("renderers", 24, &format_bytes(self.snapshot.renderer_bytes() as u64))
            ),
        );
        push_line(
            text,
            &format!(
                "  {}",
                columns("panes", 24, &format_bytes(self.snapshot.session_bytes() as u64))
            ),
        );
        let footprint = match &self.os {
            OsFigures::Sampled(sample) => sample.footprint.current.bytes(),
            OsFigures::Pending | OsFigures::Failed => None,
        };
        match footprint {
            Some(os_bytes) => {
                let os_bytes = i64::try_from(os_bytes).unwrap_or(i64::MAX);
                let counted = i64::try_from(accounted).unwrap_or(i64::MAX);
                push_line(
                    text,
                    &format!(
                        "{}   (OS footprint minus SonicTerm's counted capacity: a signed diagnostic,",
                        columns("DIFFERENCE", 26, &format_signed_bytes(os_bytes.saturating_sub(counted)))
                    ),
                );
                push_line(
                    text,
                    &format!(
                        "{}not a reconciliation; allocator slack, GPU, fonts and untracked allocations",
                        " ".repeat(29)
                    ),
                );
                push_line(
                    text,
                    &format!("{}land here, and the figures can overlap)", " ".repeat(29)),
                );
            }
            None => push_line(text, &columns("DIFFERENCE", 26, "not available")),
        }
        let busy = self.snapshot.panes_contended;
        push_line(
            text,
            &columns(
                "SAMPLE",
                26,
                &format!(
                    "{} of {} panes measured, {busy} busy; taken {}",
                    self.snapshot.panes_sampled, self.snapshot.panes_total, self.header.timestamp
                ),
            ),
        );
        if busy > 0 {
            push_line(
                text,
                &format!("{}busy panes were skipped, so the pane total is partial", " ".repeat(26)),
            );
        }
        push_line(text, "");
    }

    /// One block per visible renderer, then any warm renderers.
    fn render_windows(&self, text: &mut String) {
        push_line(text, "WINDOWS / RENDERERS");
        for window in &self.windows {
            let Some(renderer) = window.renderer.as_ref() else {
                // When: the window has no `renderer` yet, say so rather than print zeros.
                push_line(
                    text,
                    &format!("  {}", columns(&window.label, 24, "renderer not created yet")),
                );
                continue;
            };
            let suffix = if window.is_report { "   (this window's own renderer)" } else { "" };
            push_line(
                text,
                &format!(
                    "  {}{suffix}",
                    columns(
                        &window.label,
                        24,
                        &format!("total {}", format_amount(renderer.total()))
                    )
                ),
            );
            render_renderer_parts(text, renderer);
        }
        let warm: Vec<&RendererSummary> =
            self.snapshot.renderers.iter().filter(|renderer| renderer.role == "warm").collect();
        for renderer in warm {
            push_line(
                text,
                &format!(
                    "  {}   (ready for the next new window)",
                    columns(
                        &format!("warm renderer {}", renderer.label),
                        24,
                        &format!("total {}", format_amount(renderer.total()))
                    )
                ),
            );
            render_renderer_parts(text, renderer);
        }
        push_line(text, "");
    }

    /// One block per pane, by seam.
    fn render_panes(&self, text: &mut String) {
        push_line(text, "PANES");
        for pane in &self.panes {
            let suffix = if pane.is_report { "   (this report)" } else { "" };
            let Some(retention) = pane.retention.as_ref() else {
                // When: `retention` is None, the pane's lock was held, so it was skipped, not zero.
                push_line(
                    text,
                    &format!("  {}{suffix}", columns(&pane.label, 40, "busy, skipped")),
                );
                continue;
            };
            push_line(
                text,
                &format!(
                    "  {}{suffix}",
                    columns(
                        &pane.label,
                        40,
                        &format!("total {}", format_amount(retention.total()))
                    )
                ),
            );
            let triple = |left: ResourceAmount, middle: ResourceAmount, right: ResourceAmount| {
                format!(
                    "{} / {} / {}",
                    format_amount(left),
                    format_amount(middle),
                    format_amount(right)
                )
            };
            push_line(
                text,
                &format!(
                    "    {}",
                    columns(
                        "grid visible / history / alternate",
                        38,
                        &triple(
                            retention.grid_visible,
                            retention.grid_history,
                            retention.grid_alternate
                        )
                    )
                ),
            );
            push_line(
                text,
                &format!(
                    "    {}",
                    columns(
                        "parser / hyperlinks / inline media",
                        38,
                        &triple(retention.parser, retention.hyperlinks, retention.inline_media)
                    )
                ),
            );
            push_line(
                text,
                &format!(
                    "    {}",
                    columns(
                        "pty output / input",
                        38,
                        &format!(
                            "{} / {}",
                            format_amount(retention.pty_output),
                            format_amount(retention.pty_input)
                        )
                    )
                ),
            );
        }
        push_line(text, "");
    }

    /// Thread count and names; threads hold no memory of their own.
    fn render_threads(&self, text: &mut String) {
        let OsFigures::Sampled(sample) = &self.os else {
            // When: the OS sample is pending or failed, no thread list exists yet.
            push_line(text, &columns("THREADS", 26, "not available yet"));
            push_line(text, "");
            return;
        };
        let Some(total) = sample.threads.total else {
            // When: the platform cannot list threads, say so instead of printing zero.
            push_line(text, &columns("THREADS", 26, "not available"));
            push_line(text, "");
            return;
        };
        push_line(
            text,
            &columns(
                "THREADS",
                26,
                &format!(
                    "{total} threads share the process's memory; a thread holds none of its own"
                ),
            ),
        );
        let entries: Vec<String> = sample
            .threads
            .names
            .iter()
            .map(|(name, count)| {
                let name = sanitize_external(name);
                if *count > 1 {
                    format!("{name} x{count}")
                } else {
                    // When: `count` is one, the bare name is enough.
                    name
                }
            })
            .collect();
        let mut current = String::from(" ");
        for entry in entries {
            if current.len() + entry.len() + 2 > THREAD_LINE_WIDTH && current.len() > 1 {
                // The next entry would pass the line width, so start a new line.
                push_line(text, current.trim_end_matches(','));
                current = String::from(" ");
            }
            let _ = write!(current, " {entry},");
        }
        if current.len() > 1 {
            push_line(text, current.trim_end_matches(','));
        }
        push_line(text, "");
    }

    /// Each pane shell's process tree, which is outside SonicTerm's footprint.
    fn render_children(&self, text: &mut String) {
        push_line(
            text,
            &format!(
                "CHILD PROCESSES (each pane's shell and descendants, {CHILD_MEMORY_LABEL}; not part of SonicTerm's footprint)"
            ),
        );
        if self.roots.is_empty() {
            push_line(text, "  no pane has a shell process");
        }
        let trees = match &self.os {
            OsFigures::Sampled(sample) => Some(&sample.trees),
            OsFigures::Pending | OsFigures::Failed => None,
        };
        let mut tree_index = 0usize;
        for root in &self.roots {
            let Some(pid) = root.pid else {
                // When: `root.pid` is None, the pane reports no process id, so there is no tree to walk.
                push_line(text, &format!("  {}", columns(&root.label, 16, "no process id")));
                continue;
            };
            let tree = trees.and_then(|trees| trees.get(tree_index));
            tree_index += 1;
            let Some(tree) = tree else {
                // When: no tree arrived for this root, the OS sample is pending or failed.
                push_line(
                    text,
                    &format!("  {}", columns(&root.label, 16, &format!("pid {pid}: not sampled"))),
                );
                continue;
            };
            render_tree(text, &root.label, pid, tree);
        }
        push_line(
            text,
            "  A process that left its shell's tree (a daemonized tmux server, for example) is not listed;",
        );
        push_line(text, "  WSL and remote processes are not visible from here.");
    }
}

/// The parts of one renderer, two figures per line.
fn render_renderer_parts(text: &mut String, renderer: &RendererSummary) {
    let facts = &renderer.glyph_atlas_facts;
    push_line(
        text,
        &format!(
            "    {}",
            columns(
                "glyph atlas",
                22,
                &format!(
                    "{}   {}x{}, {} entries, fit={}",
                    format_amount(renderer.glyph_atlas),
                    facts.dim,
                    facts.dim,
                    format_count(renderer.glyph_atlas.items),
                    sanitize_external(&facts.fit)
                )
            )
        ),
    );
    let parts = [
        ("image atlas", renderer.image_atlas, "images"),
        ("row glyph cache", renderer.row_glyph_cache, "rows"),
        ("row quad cache", renderer.row_quad_cache, "rows"),
        ("row ink", renderer.row_ink, "rows"),
        ("frame scratch", renderer.frame_scratch, "buffers"),
        ("vertex scratch", renderer.vertex_scratch, "buffers"),
        ("software frame", renderer.software_frame, "frames"),
        ("chrome cache", renderer.chrome_cache, "entries"),
    ];
    for (name, amount, unit) in parts {
        push_line(
            text,
            &format!(
                "    {}",
                columns(
                    name,
                    22,
                    &format!("{}   {} {unit}", format_amount(amount), format_count(amount.items))
                )
            ),
        );
    }
}

/// One pane shell's tree, indented by depth.
fn render_tree(text: &mut String, label: &str, pid: u32, tree: &ProcessTree) {
    match tree.outcome {
        WalkOutcome::RootGone => {
            // When: RootGone, the shell exited or its pid now names another process; list nothing.
            push_line(
                text,
                &format!("  {}", columns(label, 16, &format!("pid {pid}: shell exited"))),
            );
            return;
        }
        WalkOutcome::Unsupported => {
            // When: Unsupported, this platform has no process table, so say so instead of listing.
            push_line(
                text,
                &format!(
                    "  {}",
                    columns(label, 16, &format!("pid {pid}: not available on this platform"))
                ),
            );
            return;
        }
        WalkOutcome::Complete | WalkOutcome::Truncated | WalkOutcome::Cancelled => {
            // When: Complete, Truncated or Cancelled, the walk listed processes, so render them below.
        }
    }
    for process in &tree.processes {
        let name = sanitize_external(&process.name);
        let entry = if process.depth == 0 {
            format!("{name} ({})", process.pid)
        } else {
            // When: `depth` is positive, indent and mark the process as a descendant.
            format!("{}- {name} ({})", "  ".repeat(process.depth), process.pid)
        };
        let left = if process.depth == 0 { label } else { "" };
        push_line(
            text,
            &format!(
                "  {}",
                columns(left, 16, &columns(&entry, 32, &format_metric(process.memory)))
            ),
        );
    }
    match tree.outcome {
        WalkOutcome::Truncated => push_line(
            text,
            &format!(
                "{}... more processes; the list stops at {MAX_DESCENDANTS_PER_ROOT}",
                " ".repeat(18)
            ),
        ),
        WalkOutcome::Cancelled => {
            push_line(text, &format!("{}... walk cancelled by a newer refresh", " ".repeat(18)));
        }
        WalkOutcome::Complete | WalkOutcome::RootGone | WalkOutcome::Unsupported => {
            // When: Complete, RootGone or Unsupported, the list needs no trailing note.
        }
    }
}

/// Bytes to feed the report pane's parser: `\n` becomes `\r\n` so each line starts at column 0.
pub(crate) fn report_feed_bytes(report: &str) -> Vec<u8> {
    report.replace('\n', "\r\n").into_bytes()
}

/// What the report window does with an action that targets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportRoute {
    /// The action runs on the report window like any terminal window's.
    Local,
    /// The action closes the report window.
    CloseReport,
    /// The action belongs to a terminal window and runs there.
    Redirect,
    /// The action has no meaning here and does nothing.
    Consume,
}

/// Decide what the report window does with `action`.
///
/// Exhaustive on purpose: a new action does not compile until it is routed here.
/// Pane and tab structure and input (paste, splits, tab switching) are consumed rather
/// than redirected, so they never change a terminal window the user is not looking at.
pub(crate) fn report_route(action: &Action) -> ReportRoute {
    match action {
        Action::CopyToClipboard
        | Action::EnterCopyMode
        | Action::OpenSearch
        | Action::Scroll(_)
        | Action::IncreaseFontSize
        | Action::DecreaseFontSize
        | Action::ResetFontSize
        | Action::IncreaseFontWeight
        | Action::DecreaseFontWeight
        | Action::ResetFontWeight
        | Action::OpenCommandPalette
        | Action::ShowMemoryUsage
        | Action::CopyMemoryReport
        | Action::ToggleFullscreen
        | Action::QuitApp => ReportRoute::Local,
        Action::CloseTab | Action::ClosePane | Action::CloseActivePaneOrTab => {
            ReportRoute::CloseReport
        }
        Action::NewTab
        | Action::NewWindow
        | Action::ReloadConfig
        | Action::ApplyTheme(_)
        | Action::ToggleTabBar
        | Action::EditConfigFile
        | Action::OpenKeymapFile
        | Action::CheckForUpdates
        | Action::SaveCurrentSettings => ReportRoute::Redirect,
        Action::PasteFromClipboard
        | Action::EnterQuickSelect
        | Action::SplitRight
        | Action::SplitDown
        | Action::TogglePaneZoom
        | Action::ToggleBroadcast { .. }
        | Action::FocusPane(_)
        | Action::ResizePaneLeft
        | Action::ResizePaneRight
        | Action::ResizePaneUp
        | Action::ResizePaneDown
        | Action::ResizePane { .. }
        | Action::NextTab
        | Action::PrevTab
        | Action::ActivateTab(_)
        | Action::ActivateLastTab
        | Action::RenameTab
        | Action::UpdateTabColor
        | Action::RenameWindow
        | Action::MoveTabToNewWindow
        | Action::ScrollToPrevPrompt
        | Action::ScrollToNextPrompt => ReportRoute::Consume,
    }
}

/// A key the report window handles before its keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKey {
    /// Copy the whole report.
    CopyAll,
    /// Take a fresh sample.
    Refresh,
    /// Close the report window.
    Close,
}

/// The platform's copy-all chord, in keymap spelling.
pub(crate) const COPY_ALL_CHORD: &str =
    if cfg!(target_os = "macos") { "super+shift+c" } else { "alt+shift+c" };

/// The platform's copy-all chord, as the hint line spells it.
pub(crate) const COPY_ALL_HINT: &str =
    if cfg!(target_os = "macos") { "Cmd+Shift+C" } else { "Alt+Shift+C" };

/// Match a key press's chord spellings against the report window's own keys.
///
/// The caller reaches this only when no IME composition, search field, or copy mode owns
/// the window, so those consume R and Esc first.
pub(crate) fn report_key(chords: &[String]) -> Option<ReportKey> {
    chords.iter().find_map(|chord| match chord.as_str() {
        chord if chord == COPY_ALL_CHORD => Some(ReportKey::CopyAll),
        "r" | "shift+r" => Some(ReportKey::Refresh),
        "escape" => Some(ReportKey::Close),
        _ => None,
    })
}

#[cfg(test)]
#[path = "memory_report_tests.rs"]
mod memory_report_tests;
