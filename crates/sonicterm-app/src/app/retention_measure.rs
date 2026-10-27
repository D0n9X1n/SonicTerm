//! Pane retained-memory measurements and sampled retention diagnostics.

use super::*;

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
        [
            self.grid_visible,
            self.grid_history,
            self.grid_alternate,
            self.parser,
            self.hyperlinks,
            self.inline_media,
            self.pty_output,
            self.pty_input,
        ]
        .into_iter()
        .fold(ResourceAmount::default(), |acc, part| ResourceAmount {
            bytes: acc.bytes.saturating_add(part.bytes),
            items: acc.items.saturating_add(part.items),
        })
    }

    /// The seam holding the most bytes, for reporting the dominant term first.
    #[must_use]
    pub fn largest_seam(&self) -> (&'static str, ResourceAmount) {
        [
            ("grid_visible", self.grid_visible),
            ("grid_history", self.grid_history),
            ("grid_alternate", self.grid_alternate),
            ("parser", self.parser),
            ("hyperlinks", self.hyperlinks),
            ("inline_media", self.inline_media),
            ("pty_output", self.pty_output),
            ("pty_input", self.pty_input),
        ]
        .into_iter()
        .max_by_key(|(_, amount)| amount.bytes)
        .unwrap_or(("grid_visible", ResourceAmount::default()))
    }
}

/// Measure what `pane` retains right now.
///
/// Takes the parser and inline-media locks briefly. Callers on the render path
/// must not block on either — this is for diagnostics and periodic reporting,
/// not per-frame use. Returns `None` when either lock is held, so a busy pane
/// delays a complete measurement rather than reporting a partial one as exact.
#[must_use]
pub fn measure_pane(pane: &PaneState) -> Option<PaneRetention> {
    let parser = pane.parser.try_lock()?;
    let regions = parser.grid().retained_amount_by_region();
    let hyperlink_bytes = parser.hyperlinks().retained_bytes();
    let hyperlink_items = parser.hyperlinks().len();
    let parser_amount = parser.retained_amount();
    drop(parser);

    let inline_images = pane.inline_images.try_lock()?;
    let inline_media = super::super::media::retained_inline_media(&inline_images);
    drop(inline_images);

    // Ring memory, not payload: the queued views are windows into the reader's
    // reused 64 KiB ring, so a pane holding 64 bytes of keystroke echo is
    // holding 64 KiB. `items` is the queued chunk count, which is what the
    // slot bound is expressed in.
    let pty_output = pane.pty.as_ref().map_or_else(ResourceAmount::default, |pty| ResourceAmount {
        bytes: sonicterm_io::pty::queued_output_bytes(pty),
        items: pty.out_rx.len(),
    });

    let pty_input = pane.pty.as_ref().map_or_else(ResourceAmount::default, |pty| {
        let bytes = pty.queued_input_bytes();
        ResourceAmount { bytes, items: usize::from(bytes > 0) }
    });

    Some(PaneRetention {
        grid_visible: regions.visible,
        grid_history: regions.history,
        grid_alternate: regions.alternate,
        parser: parser_amount,
        hyperlinks: ResourceAmount { bytes: hyperlink_bytes, items: hyperlink_items },
        inline_media,
        pty_output,
        pty_input,
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
            grid_visible: add(acc.grid_visible, pane.grid_visible),
            grid_history: add(acc.grid_history, pane.grid_history),
            grid_alternate: add(acc.grid_alternate, pane.grid_alternate),
            parser: add(acc.parser, pane.parser),
            hyperlinks: add(acc.hyperlinks, pane.hyperlinks),
            inline_media: add(acc.inline_media, pane.inline_media),
            pty_output: add(acc.pty_output, pane.pty_output),
            pty_input: add(acc.pty_input, pane.pty_input),
        }
    })
}

pub(super) fn add(left: ResourceAmount, right: ResourceAmount) -> ResourceAmount {
    ResourceAmount {
        bytes: left.bytes.saturating_add(right.bytes),
        items: left.items.saturating_add(right.items),
    }
}

#[cfg(test)]
#[path = "retention_measure_tests.rs"]
mod retention_measure_tests;
