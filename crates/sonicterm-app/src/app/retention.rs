//! What a pane actually retains, summed across its independent seams.
//!
//! Each subsystem meters its own memory: [`Grid::retained_amount`],
//! [`Parser::retained_amount`], `HyperlinkRegistry::retained_bytes`, and the
//! pane's decoded inline media. Those figures are deliberately **disjoint** —
//! each seam counts what it alone owns, so no allocation is charged twice —
//! which also means no single one of them answers the question a user asks
//! when a session grows: *what is this pane holding?*
//!
//! Answering it requires summing them, and a sum is only meaningful if the
//! parts are disjoint. That property is load-bearing rather than incidental,
//! so it is pinned by tests in both the parser and this module.
//!
//! This is measurement, not enforcement. Nothing here rejects, evicts, or
//! caps: the per-seam limits already do that. What it adds is the ability to
//! see the composition — bounded parts summing without a bound above them —
//! which is the shape behind reported multi-gigabyte growth and the thing no
//! individual seam can reveal.

use sonicterm_types::ResourceAmount;

use super::PaneState;

/// One pane's retention, split by the seam that owns each part.
///
/// Kept as separate fields rather than a single total because the total alone
/// does not tell an operator what to do. A pane holding 60 MiB of inline media
/// is behaving as designed; a pane holding 60 MiB of grid is not, and the
/// remedy differs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneRetention {
    /// Cells, scrollback, saved alternate screen, and prompt regions.
    pub grid: ResourceAmount,
    /// In-flight escape/media capture buffers held by the parser.
    pub parser: ResourceAmount,
    /// Interned OSC 8 hyperlink strings.
    pub hyperlinks: ResourceAmount,
    /// Decoded inline images retained for display.
    pub inline_media: ResourceAmount,
}

impl PaneRetention {
    /// Sum of every seam.
    ///
    /// Saturating rather than checked: this figure exists to be reported, and
    /// a diagnostic that fails to render when the number is alarmingly large
    /// is a diagnostic that is absent exactly when it is needed. The seams are
    /// individually capped well below `usize::MAX`, so saturation is
    /// unreachable in practice.
    #[must_use]
    pub fn total(&self) -> ResourceAmount {
        [self.grid, self.parser, self.hyperlinks, self.inline_media].into_iter().fold(
            ResourceAmount::default(),
            |acc, part| ResourceAmount {
                bytes: acc.bytes.saturating_add(part.bytes),
                items: acc.items.saturating_add(part.items),
            },
        )
    }

    /// The seam holding the most bytes, for reporting the dominant term first.
    #[must_use]
    pub fn largest_seam(&self) -> (&'static str, ResourceAmount) {
        [
            ("grid", self.grid),
            ("parser", self.parser),
            ("hyperlinks", self.hyperlinks),
            ("inline_media", self.inline_media),
        ]
        .into_iter()
        .max_by_key(|(_, amount)| amount.bytes)
        .unwrap_or(("grid", ResourceAmount::default()))
    }
}

/// Measure what `pane` retains right now.
///
/// Takes the parser lock briefly. Callers on the render path must not block on
/// it — this is for diagnostics and periodic reporting, not per-frame use.
/// Returns `None` when the lock is held, so a busy VT thread delays a
/// measurement rather than stalling the caller.
#[must_use]
pub fn measure_pane(pane: &PaneState) -> Option<PaneRetention> {
    let parser = pane.parser.try_lock()?;
    let grid = parser.grid().retained_amount();
    let hyperlink_bytes = parser.hyperlinks().retained_bytes();
    let hyperlink_items = parser.hyperlinks().len();
    let parser_amount = parser.retained_amount();
    drop(parser);

    let inline_media = pane
        .inline_images
        .try_lock()
        .map(|images| super::media::retained_inline_media(&images))
        .unwrap_or_default();

    Some(PaneRetention {
        grid,
        parser: parser_amount,
        hyperlinks: ResourceAmount { bytes: hyperlink_bytes, items: hyperlink_items },
        inline_media,
    })
}

/// Sum retention across panes.
///
/// The per-pane figures are each bounded; this is the number that is not. A
/// session's total is the product of its pane count and per-pane ceilings,
/// with nothing above the pane saying no — which is why it is worth reporting
/// even though every contributing pane is individually compliant.
#[must_use]
pub fn measure_panes<'a>(panes: impl IntoIterator<Item = &'a PaneState>) -> PaneRetention {
    panes.into_iter().filter_map(measure_pane).fold(PaneRetention::default(), |acc, pane| {
        PaneRetention {
            grid: add(acc.grid, pane.grid),
            parser: add(acc.parser, pane.parser),
            hyperlinks: add(acc.hyperlinks, pane.hyperlinks),
            inline_media: add(acc.inline_media, pane.inline_media),
        }
    })
}

fn add(left: ResourceAmount, right: ResourceAmount) -> ResourceAmount {
    ResourceAmount {
        bytes: left.bytes.saturating_add(right.bytes),
        items: left.items.saturating_add(right.items),
    }
}

/// Emit one pane's retention to the memory log.
///
/// Reports the dominant seam explicitly: a total on its own says a pane is
/// large without saying which subsystem to look at, and the remedy differs
/// per seam.
pub fn log_pane_retention(label: &str, retention: &PaneRetention) {
    let total = retention.total();
    let (seam, largest) = retention.largest_seam();
    tracing::debug!(
        target: "memory",
        pane = label,
        total_bytes = total.bytes,
        grid_bytes = retention.grid.bytes,
        parser_bytes = retention.parser.bytes,
        hyperlink_bytes = retention.hyperlinks.bytes,
        inline_media_bytes = retention.inline_media.bytes,
        largest_seam = seam,
        largest_seam_bytes = largest.bytes,
        "pane retention"
    );
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod retention_tests;
