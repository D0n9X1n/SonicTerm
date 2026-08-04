//! One aggregate line answering "what is this process holding right now".
//!
//! Every other memory report in this crate is detail: [`super::retention`]
//! emits a line per pane, [`super::renderer_retention`] a line per renderer.
//! Both sit at DEBUG, which is the correct level for someone already
//! investigating and the wrong one for the situation this module exists for —
//! a session that grew to several gigabytes and was then killed, where the
//! only evidence is whatever the log already contained.
//!
//! A user who has to know to set `level = "debug"` *before* the problem
//! happens has already lost the session they wanted to explain. So this module
//! emits a single INFO line on the same thirty-second cadence, carrying the
//! whole picture in one record.
//!
//! ## Why one line rather than several
//!
//! The per-pane and per-renderer lines are separate records because a reader
//! greps for the one pane or the one window they care about. This line is the
//! opposite question — the totals, in one place, diffable against the previous
//! sample — so splitting it across records would force a reader to correlate
//! timestamps to reconstruct a single instant. The per-renderer breakdown is
//! therefore carried as one composed field rather than as N records.
//!
//! ## What it is allowed to contain
//!
//! Class names, roles, counts, and byte sizes. No pane titles, no window
//! titles, no command text, no environment, nothing typed or displayed. The
//! renderer breakdown carries a window's *identifier* and *role*, never its
//! title.
//!
//! ## Three kinds of "no number"
//!
//! A figure can be absent for reasons that are not interchangeable, and
//! collapsing them would put an invented measurement in the one report whose
//! entire value is being trustworthy:
//!
//! - the platform exposes no such figure — [`MemoryMetric::Unsupported`];
//! - there is no earlier sample to compare against, or the figure itself is
//!   unsupported — [`MemoryDelta::Unavailable`];
//! - a pane's lock was held, so it was skipped rather than waited on — counted
//!   and reported as `panes_contended`.
//!
//! The last one matters more than it looks. Measurement here uses `try_lock`
//! and skips what it cannot read, so a session under heavy output can report a
//! total assembled from a subset of its panes. Reporting the total without the
//! skip count would understate the session at exactly the moment it is largest,
//! and a reader would have no way to know.

use sonicterm_logging::process_memory::{self, MemoryDelta, MemoryMetric, ProcessMemory};

use super::retention::PaneRetention;

/// One renderer's contribution, with the identity needed to act on it.
///
/// `role` distinguishes a renderer the user can see from one held ready in the
/// warm pool. Both hold a full-size glyph atlas, but the remedy differs: a
/// visible renderer goes away when its window closes, while a warm one is
/// governed by the warm-pool size. A line that did not say which it was would
/// point a user at a window they cannot close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererSummary {
    /// Window identifier for a visible renderer, or the pool slot for a warm
    /// one. An identifier, never a window title.
    pub label: String,
    /// `visible` or `warm`.
    pub role: &'static str,
    /// CPU-side glyph atlas.
    pub glyph_atlas: sonicterm_types::ResourceAmount,
    /// CPU-side image atlas.
    pub image_atlas: sonicterm_types::ResourceAmount,
    /// Windows software presentation buffer; zero elsewhere.
    pub software_frame: sonicterm_types::ResourceAmount,
}

impl RendererSummary {
    /// Sum of this renderer's parts.
    #[must_use]
    pub fn total(&self) -> sonicterm_types::ResourceAmount {
        [self.glyph_atlas, self.image_atlas, self.software_frame].into_iter().fold(
            sonicterm_types::ResourceAmount::default(),
            |acc, part| sonicterm_types::ResourceAmount {
                bytes: acc.bytes.saturating_add(part.bytes),
                items: acc.items.saturating_add(part.items),
            },
        )
    }

    /// Render as one field of the composed renderer breakdown.
    ///
    /// Deliberately terse and positional: this string appears once per
    /// renderer inside a single log field, so a verbose form would push the
    /// interesting totals off the end of a line an operator is skimming.
    fn render(&self) -> String {
        let total = self.total();
        format!(
            "{}[{}] glyph={}/{} image={}/{} software={}/{} total={}/{}",
            self.role,
            self.label,
            self.glyph_atlas.bytes,
            self.glyph_atlas.items,
            self.image_atlas.bytes,
            self.image_atlas.items,
            self.software_frame.bytes,
            self.software_frame.items,
            total.bytes,
            total.items,
        )
    }
}

/// Everything one sampling cycle measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// What the OS says about the process as a whole.
    pub process: ProcessMemory,
    /// Pane retention summed across every window, by seam.
    pub session: PaneRetention,
    /// Every pane visited in this sampling cycle.
    pub panes_total: usize,
    /// Panes that contributed to `session`.
    pub panes_sampled: usize,
    /// Panes skipped because their parser lock was held.
    ///
    /// Non-zero means `session` is a partial total. Reported rather than
    /// hidden: a busy pane holds *more* memory than an idle one, so silently
    /// omitting it understates the session exactly when it matters.
    pub panes_contended: usize,
    /// Every renderer, visible and warm.
    pub renderers: Vec<RendererSummary>,
    /// `GpuRenderer` instances alive process-wide.
    ///
    /// Read from the renderer crate's own counter rather than derived from
    /// `renderers.len()`. The two agreeing is the useful signal: a renderer
    /// that leaked is alive without being reachable from any window, so it
    /// raises this count while contributing no summary line.
    pub live_renderers: usize,
}

impl MemorySnapshot {
    /// Session bytes across every pane seam.
    #[must_use]
    pub fn session_bytes(&self) -> usize {
        self.session.total().bytes
    }

    /// Bytes held by every renderer, visible and warm.
    #[must_use]
    pub fn renderer_bytes(&self) -> usize {
        self.renderers
            .iter()
            .fold(0usize, |acc, renderer| acc.saturating_add(renderer.total().bytes))
    }

    /// Items held by every renderer.
    #[must_use]
    pub fn renderer_items(&self) -> usize {
        self.renderers
            .iter()
            .fold(0usize, |acc, renderer| acc.saturating_add(renderer.total().items))
    }

    /// The per-renderer breakdown, as one field.
    ///
    /// `none` rather than an empty string when no renderer exists, so the
    /// field is never blank — a blank field reads as a bug in the logger
    /// rather than as a session with no window yet.
    #[must_use]
    pub fn render_renderers(&self) -> String {
        if self.renderers.is_empty() {
            // When: `renderers` is empty, emit an explicit sentinel instead of an ambiguous blank field.
            return "none".to_string();
        }
        self.renderers.iter().map(RendererSummary::render).collect::<Vec<_>>().join("; ")
    }

    /// The figures a later sample diffs against.
    ///
    /// Only the totals are retained, not the whole snapshot: a delta is
    /// computed against process, session, and renderer figures, and holding
    /// the per-renderer vector between samples would keep a string per
    /// renderer alive for the life of the session to serve a report that never
    /// reads it.
    #[must_use]
    pub fn totals(&self) -> MemoryTotals {
        MemoryTotals {
            process: self.process,
            session_bytes: self.session_bytes(),
            renderer_bytes: self.renderer_bytes(),
        }
    }
}

/// The previous sample's totals, kept to compute deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryTotals {
    /// OS-reported process figures.
    pub process: ProcessMemory,
    /// Session bytes across every pane seam.
    pub session_bytes: usize,
    /// Bytes held by every renderer.
    pub renderer_bytes: usize,
}

/// A signed change in a figure this process counts itself.
///
/// Distinct from [`MemoryDelta`], which describes an OS figure that may be
/// unsupported. A session or renderer total is always measured, so the only
/// reason one is unavailable is that there is no previous sample.
#[must_use]
fn counted_delta(previous: Option<usize>, current: usize) -> MemoryDelta {
    match previous {
        Some(previous) => {
            let previous = i64::try_from(previous).unwrap_or(i64::MAX);
            let current = i64::try_from(current).unwrap_or(i64::MAX);
            MemoryDelta::Changed(current.saturating_sub(previous))
        }
        None => MemoryDelta::Unavailable,
    }
}

/// Emit the aggregate snapshot at INFO on the `memory` target.
///
/// `previous` is the preceding cycle's totals, or `None` on the first sample
/// of a session — in which case every delta reports `unavailable` rather than
/// `+0`, which would claim the process had not moved.
///
/// Every field is written on every sample, including zero-valued pane classes.
/// A class omitted when empty makes successive samples structurally different,
/// which defeats the diffing this line exists for and leaves a reader unable to
/// tell "held nothing" from "stopped being reported".
pub fn emit_memory_snapshot(snapshot: &MemorySnapshot, previous: Option<MemoryTotals>) {
    let session = &snapshot.session;
    let session_bytes = snapshot.session_bytes();
    let renderer_bytes = snapshot.renderer_bytes();

    tracing::info!(
        target: "memory",
        // Process, as the OS sees it. Each renders as a byte count or the
        // literal `unsupported`; never a fabricated zero.
        process_private_committed_bytes = %snapshot.process.private_committed,
        process_resident_bytes = %snapshot.process.resident,
        process_virtual_bytes = %snapshot.process.virtual_bytes,
        process_private_committed_delta =
            %MemoryDelta::between(
                previous.map_or(MemoryMetric::Unsupported, |p| p.process.private_committed),
                snapshot.process.private_committed,
            ),
        process_resident_delta =
            %MemoryDelta::between(
                previous.map_or(MemoryMetric::Unsupported, |p| p.process.resident),
                snapshot.process.resident,
            ),
        process_virtual_delta =
            %MemoryDelta::between(
                previous.map_or(MemoryMetric::Unsupported, |p| p.process.virtual_bytes),
                snapshot.process.virtual_bytes,
            ),
        // Session, summed across panes. Every seam, including the empty ones.
        session_total_bytes = session_bytes,
        session_delta = %counted_delta(previous.map(|p| p.session_bytes), session_bytes),
        grid_visible_bytes = session.grid_visible.bytes,
        grid_history_bytes = session.grid_history.bytes,
        grid_alternate_bytes = session.grid_alternate.bytes,
        parser_bytes = session.parser.bytes,
        hyperlink_bytes = session.hyperlinks.bytes,
        inline_media_bytes = session.inline_media.bytes,
        pty_output_bytes = session.pty_output.bytes,
        pty_input_bytes = session.pty_input.bytes,
        panes_total = snapshot.panes_total,
        panes_sampled = snapshot.panes_sampled,
        // Non-zero means the session total above is partial.
        panes_contended = snapshot.panes_contended,
        // Renderers, visible and warm.
        renderer_total_bytes = renderer_bytes,
        renderer_total_items = snapshot.renderer_items(),
        renderer_delta = %counted_delta(previous.map(|p| p.renderer_bytes), renderer_bytes),
        live_renderers = snapshot.live_renderers,
        renderers = %snapshot.render_renderers(),
        "memory snapshot"
    );
}

impl super::App {
    /// Measure the process, every pane, and every renderer as one cycle.
    ///
    /// Panes are measured with `try_lock` and skipped when contended, exactly
    /// as [`super::retention::measure_pane`] does — a diagnostic must never
    /// stall the thread it reports from. Skips are counted so the emitted line
    /// can say the total is partial.
    pub(super) fn build_memory_snapshot(&self) -> MemorySnapshot {
        let mut session = PaneRetention::default();
        let mut panes_sampled = 0usize;
        let mut panes_contended = 0usize;

        for window in self.windows.values() {
            for pane in window.panes.values() {
                match super::retention::measure_pane(pane) {
                    Some(retention) => {
                        session = add_retention(session, &retention);
                        panes_sampled += 1;
                    }
                    None => panes_contended += 1,
                }
            }
        }

        let mut renderers = Vec::new();
        for (window_id, window) in &self.windows {
            let Some(renderer) = window.renderer.as_ref() else {
                // When: this window has no renderer yet, omit a zero summary that would look like measured retention.
                continue;
            };
            renderers.push(summarize(
                format!("{window_id:?}"),
                "visible",
                &renderer.retained_amounts(),
            ));
        }
        for (index, warm) in self.warm_window_pool.iter().enumerate() {
            renderers.push(summarize(
                format!("{index}"),
                "warm",
                &warm.renderer.retained_amounts(),
            ));
        }

        MemorySnapshot {
            process: process_memory::sample(),
            session,
            panes_total: panes_sampled.saturating_add(panes_contended),
            panes_sampled,
            panes_contended,
            renderers,
            live_renderers: sonicterm_gpu::core::live_renderer_count(),
        }
    }
}

fn summarize(
    label: String,
    role: &'static str,
    retention: &sonicterm_gpu::core::RendererRetention,
) -> RendererSummary {
    RendererSummary {
        label,
        role,
        glyph_atlas: retention.glyph_atlas,
        image_atlas: retention.image_atlas,
        software_frame: retention.software_frame,
    }
}

fn add_retention(session: PaneRetention, pane: &PaneRetention) -> PaneRetention {
    PaneRetention {
        grid_visible: add(session.grid_visible, pane.grid_visible),
        grid_history: add(session.grid_history, pane.grid_history),
        grid_alternate: add(session.grid_alternate, pane.grid_alternate),
        parser: add(session.parser, pane.parser),
        hyperlinks: add(session.hyperlinks, pane.hyperlinks),
        inline_media: add(session.inline_media, pane.inline_media),
        pty_output: add(session.pty_output, pane.pty_output),
        pty_input: add(session.pty_input, pane.pty_input),
    }
}

fn add(
    left: sonicterm_types::ResourceAmount,
    right: sonicterm_types::ResourceAmount,
) -> sonicterm_types::ResourceAmount {
    sonicterm_types::ResourceAmount {
        bytes: left.bytes.saturating_add(right.bytes),
        items: left.items.saturating_add(right.items),
    }
}

#[cfg(test)]
#[path = "memory_snapshot_tests.rs"]
mod memory_snapshot_tests;
