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

use std::time::{Duration, Instant};

use sonicterm_types::{ResourceAmount, ResourceClass};

use super::PaneState;

/// One pane's retention, split by the seam that owns each part.
///
/// Kept as separate fields rather than a single total because the total alone
/// does not tell an operator what to do. A pane holding 60 MiB of inline media
/// is behaving as designed; a pane holding 60 MiB of grid is not, and the
/// remedy differs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneRetention {
    /// On-screen cells, their prompt regions, and their rare-attribute boxes.
    pub grid_visible: ResourceAmount,
    /// Scrollback history.
    pub grid_history: ResourceAmount,
    /// The saved primary screen held while an alternate screen is showing.
    ///
    /// Separated from history because the remedy differs: history is memory
    /// the user asked for and can shrink by lowering `scrollback`, while a
    /// saved primary is memory held for a screen they are not looking at and
    /// which disappears when the alternate-screen program exits.
    pub grid_alternate: ResourceAmount,
    /// In-flight escape/media capture buffers held by the parser.
    pub parser: ResourceAmount,
    /// Interned OSC 8 hyperlink strings.
    pub hyperlinks: ResourceAmount,
    /// Decoded inline images retained for display.
    pub inline_media: ResourceAmount,
    /// Bytes waiting in this pane's PTY output channel.
    ///
    /// Bounded at 64 slots over a reused 64 KiB ring. Measured at 512 KiB per
    /// pane when full — the ring hands out views into one allocation rather
    /// than 64 independent buffers, so the figure is 8x below what the slot
    /// count alone would suggest. Ten MiB across twenty panes: small next to
    /// the grid, large enough that leaving it uncharged would be a gap rather
    /// than a rounding error.
    pub pty_output: ResourceAmount,
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
        [
            self.grid_visible,
            self.grid_history,
            self.grid_alternate,
            self.parser,
            self.hyperlinks,
            self.inline_media,
            self.pty_output,
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
        ]
        .into_iter()
        .max_by_key(|(_, amount)| amount.bytes)
        .unwrap_or(("grid_visible", ResourceAmount::default()))
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
    let regions = parser.grid().retained_amount_by_region();
    let hyperlink_bytes = parser.hyperlinks().retained_bytes();
    let hyperlink_items = parser.hyperlinks().len();
    let parser_amount = parser.retained_amount();
    drop(parser);

    let inline_media = pane
        .inline_images
        .try_lock()
        .map(|images| super::media::retained_inline_media(&images))
        .unwrap_or_default();

    let pty_output = pane.pty.as_ref().map_or_else(ResourceAmount::default, |pty| {
        let bytes = sonicterm_io::pty::queued_output_bytes(pty);
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
        grid_visible_bytes = retention.grid_visible.bytes,
        grid_history_bytes = retention.grid_history.bytes,
        grid_alternate_bytes = retention.grid_alternate.bytes,
        parser_bytes = retention.parser.bytes,
        hyperlink_bytes = retention.hyperlinks.bytes,
        inline_media_bytes = retention.inline_media.bytes,
        pty_output_bytes = retention.pty_output.bytes,
        largest_seam = seam,
        largest_seam_bytes = largest.bytes,
        "pane retention"
    );
}

/// How often pane retention is sampled for the memory log.
///
/// A sample walks every pane's seams and briefly takes each parser lock, so it
/// is far too costly to run per frame. Thirty seconds is frequent enough to
/// show a growth curve across a session — the thing a memory report needs —
/// and rare enough that its cost is not measurable against idle CPU.
pub const RETENTION_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

/// Whether a sample is due, advancing the timestamp when it is.
///
/// Split out from [`sample_retention_if_due`] so the cadence is testable
/// without a tracing subscriber. Folding it into the caller made both its
/// tests pass vacuously: with no subscriber installed the level guard is
/// false, so every assertion sat behind an early return and the tests stayed
/// green against any interval bug.
#[must_use]
pub fn retention_sample_due(last_sample: &mut Option<Instant>, now: Instant) -> bool {
    if last_sample.is_some_and(|last| now.duration_since(last) < RETENTION_SAMPLE_INTERVAL) {
        return false;
    }
    *last_sample = Some(now);
    true
}

/// The governor class each retention seam is charged to.
///
/// Explicit rather than inferred so the mapping is reviewable in one place: a
/// seam charged to the wrong class makes the dominant-class line point at the
/// wrong subsystem, which is worse than not reporting it.
///
/// `hyperlinks` maps to `ProtocolMetadata` rather than a grid class because
/// the registry owns those strings independently of any cell — that
/// disjointness is what makes summing the seams valid at all.
#[must_use]
pub fn seam_classes(retention: &PaneRetention) -> [(ResourceClass, ResourceAmount); 7] {
    [
        (ResourceClass::GridVisible, retention.grid_visible),
        (ResourceClass::GridHistory, retention.grid_history),
        (ResourceClass::GridAlternate, retention.grid_alternate),
        (ResourceClass::ParserCapture, retention.parser),
        (ResourceClass::ProtocolMetadata, retention.hyperlinks),
        (ResourceClass::InlineMediaRetained, retention.inline_media),
        (ResourceClass::PtyOutput, retention.pty_output),
    ]
}

/// Move a pane's governor charges to match what it currently retains.
///
/// Resizes live charges rather than releasing and re-reserving. A pane's
/// retention moves continuously, and a release/re-reserve pair on every sample
/// leaves the ledger briefly disagreeing with reality — the same window that
/// made the inline-media charge undercount when it was released before the
/// pixels were freed.
///
/// A charge that cannot grow is left at its current size rather than dropped.
/// Under an unlimited governor that cannot happen; if limits are ever
/// introduced, reporting a stale-but-live figure beats reporting nothing while
/// the pane still holds the memory.
pub fn charge_pane_retention(
    governor: &sonicterm_resource::ResourceGovernor,
    owner: sonicterm_types::ResourceOwnerId,
    charges: &mut std::collections::HashMap<
        ResourceClass,
        sonicterm_resource::CommittedReservation,
    >,
    retention: &PaneRetention,
) {
    charge_classes(governor, owner, charges, seam_classes(retention));
}

/// Move an owner's charges to match a set of class-tagged amounts.
///
/// Shared by every owner that charges the governor rather than written once
/// per owner kind. The resize-in-place behaviour below is the part worth not
/// duplicating: a release/re-reserve pair on every sample would pass through
/// zero, and a concurrent reservation could take the budget in that window and
/// leave this owner unable to re-open its own charge.
pub fn charge_classes(
    governor: &sonicterm_resource::ResourceGovernor,
    owner: sonicterm_types::ResourceOwnerId,
    charges: &mut std::collections::HashMap<
        ResourceClass,
        sonicterm_resource::CommittedReservation,
    >,
    classes: impl IntoIterator<Item = (ResourceClass, ResourceAmount)>,
) {
    for (class, amount) in classes {
        match charges.entry(class) {
            std::collections::hash_map::Entry::Occupied(mut held) => {
                let current = held.get().committed_amount();
                let outcome = if amount.component_le(current) {
                    held.get_mut().shrink(amount)
                } else {
                    held.get_mut().try_grow(amount)
                };
                if let Err(error) = outcome {
                    tracing::debug!(
                        target: "memory",
                        ?error,
                        ?class,
                        "charge could not be resized; the reported figure lags reality"
                    );
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                if amount.bytes == 0 && amount.items == 0 {
                    continue;
                }
                match governor
                    .try_reserve(owner, class, amount)
                    .and_then(|reservation| reservation.commit(amount).map_err(|error| error.error))
                {
                    Ok(committed) => {
                        slot.insert(committed);
                    }
                    Err(error) => {
                        tracing::debug!(
                            target: "memory",
                            ?error,
                            ?class,
                            "charge could not be opened; this class is omitted from the ledger"
                        );
                    }
                }
            }
        }
    }
}

/// Sample and log retention if the interval has elapsed.
///
/// Returns `true` when a sample was taken, so callers can pin the cadence in
/// tests without waiting on a wall clock.
///
/// Two guards keep this off the hot path. It runs only when the `memory`
/// target is actually recording at debug level, so a default session pays one
/// level check and nothing else; and it samples on an interval rather than
/// every call, because the caller is the idle-wake path that governs idle CPU.
///
/// Panes whose parser lock is contended are skipped rather than waited on: a
/// diagnostic must never stall the thread it is reporting from.
pub fn sample_retention_if_due<'a>(
    last_sample: &mut Option<Instant>,
    now: Instant,
    panes: impl IntoIterator<Item = (&'a str, &'a PaneState)>,
) -> bool {
    if !tracing::enabled!(target: "memory", tracing::Level::DEBUG) {
        return false;
    }
    if !retention_sample_due(last_sample, now) {
        return false;
    }
    log_sampled_panes(panes);
    true
}

/// Measure and log each pane, then the session total.
///
/// Separate from the gating so it can be exercised directly: contended panes
/// must be skipped rather than waited on, which is a property of this walk
/// rather than of the interval.
pub fn log_sampled_panes<'a>(
    panes: impl IntoIterator<Item = (&'a str, &'a PaneState)>,
) -> PaneRetention {
    let mut session = PaneRetention::default();
    let mut sampled = 0usize;
    for (label, pane) in panes {
        let Some(retention) = measure_pane(pane) else { continue };
        log_pane_retention(label, &retention);
        session = PaneRetention {
            grid_visible: add(session.grid_visible, retention.grid_visible),
            grid_history: add(session.grid_history, retention.grid_history),
            grid_alternate: add(session.grid_alternate, retention.grid_alternate),
            parser: add(session.parser, retention.parser),
            hyperlinks: add(session.hyperlinks, retention.hyperlinks),
            inline_media: add(session.inline_media, retention.inline_media),
            pty_output: add(session.pty_output, retention.pty_output),
        };
        sampled += 1;
    }

    // The session line is the one that matters for the growth this milestone
    // exists to explain: every pane can be inside its own ceiling while the
    // sum is not, and only this figure shows that.
    let total = session.total();
    tracing::debug!(
        target: "memory",
        panes = sampled,
        total_bytes = total.bytes,
        grid_visible_bytes = session.grid_visible.bytes,
        grid_history_bytes = session.grid_history.bytes,
        grid_alternate_bytes = session.grid_alternate.bytes,
        parser_bytes = session.parser.bytes,
        hyperlink_bytes = session.hyperlinks.bytes,
        inline_media_bytes = session.inline_media.bytes,
        pty_output_bytes = session.pty_output.bytes,
        "session retention"
    );
    session
}

impl super::App {
    /// Sample every window's panes into the memory log, at most once per
    /// [`RETENTION_SAMPLE_INTERVAL`].
    ///
    /// Called from the idle-wake path, which is also what governs idle CPU, so
    /// both guards in [`sample_retention_if_due`] matter here: a default
    /// session pays a single level check per wake and nothing more.
    ///
    /// Labels carry the window and pane id so a line identifies which pane it
    /// describes — a total with no owner tells an operator a number without
    /// telling them where to look.
    /// Charge every owned pane's retention into the governor.
    ///
    /// Measure and charge are the same figure by construction: the amount
    /// charged comes from `measure_pane`, which is what the log lines report.
    /// Computing them separately would produce two numbers that are supposed
    /// to be equal and are maintained apart — the drift that let a media cap
    /// silently stop capping.
    ///
    /// Panes whose parser lock is held are skipped, exactly as measurement
    /// skips them. Their charge stays at its last value rather than dropping
    /// to zero, because a busy pane holds *more* memory than an idle one and
    /// zeroing it would understate the process at the worst moment.
    #[doc(hidden)]
    pub fn __test_charge_pane_owners(&mut self) {
        self.charge_pane_owners();
    }

    /// Test-only: enter the **gated** production sampling path.
    ///
    /// Distinct from [`Self::__test_charge_pane_owners`], which calls the
    /// charging step directly. Production reaches charging only through
    /// `sample_pane_retention`, whose `enabled!(target: "memory", …)` gate is
    /// the part that was closed in every shipped session. A test that enters
    /// below the gate cannot observe that, which is why one that enters above
    /// it exists.
    ///
    /// Clears the rate limiter so the sample is due.
    #[doc(hidden)]
    pub fn __test_sample_pane_retention_now(&mut self) -> bool {
        self.last_retention_sample = None;
        self.sample_pane_retention(Instant::now())
    }

    fn charge_pane_owners(&mut self) {
        let window_ids: Vec<super::WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let Some(window) = self.windows.get(&window_id) else { continue };
            let pane_ids: Vec<u64> = window.panes.keys().copied().collect();

            for pane_id in pane_ids {
                let Some(pane) = self.windows.get(&window_id).and_then(|w| w.panes.get(&pane_id))
                else {
                    continue;
                };
                let Some(owner) = pane.owner else { continue };
                let Some(retention) = measure_pane(pane) else { continue };

                let Some(pane) =
                    self.windows.get_mut(&window_id).and_then(|w| w.panes.get_mut(&pane_id))
                else {
                    continue;
                };
                charge_pane_retention(&self.governor, owner, &mut pane.charges, &retention);
            }
        }
    }

    pub(super) fn sample_pane_retention(&mut self, now: Instant) -> bool {
        if !tracing::enabled!(target: "memory", tracing::Level::DEBUG) {
            return false;
        }
        // Owners are reconciled on the same interval: both walk every pane,
        // and doing them together means the hierarchy cannot drift from the
        // figures reported beside it.
        // Reattribution subsumes reconciliation: it re-parents moved panes and
        // then registers anything still unowned.
        self.reattribute_pane_owners();

        // Charge what each pane retains into its governor owner, so the
        // hierarchy reports real memory rather than only structure. Done in
        // the same pass as the log lines: two passes computing the same
        // figures would be two figures that must agree.
        self.charge_pane_owners();

        let labelled: Vec<(String, &PaneState)> = self
            .windows
            .iter()
            .flat_map(|(win_id, ws)| {
                ws.panes.iter().map(move |(pane_id, pane)| (format!("{win_id:?}/{pane_id}"), pane))
            })
            .collect();
        let borrowed: Vec<(&str, &PaneState)> =
            labelled.iter().map(|(label, pane)| (label.as_str(), *pane)).collect();
        sample_retention_if_due(&mut self.last_retention_sample, now, borrowed)
    }
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod retention_tests;
