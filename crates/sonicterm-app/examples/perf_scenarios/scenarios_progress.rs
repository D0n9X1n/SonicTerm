//! Scenario progress requirements for status, program and parked input.

use super::*;

/// Whether a `status-line` phase kept the strip's idle contract, or why not.
///
/// `idle` and `uncovered` hold `hold_ms`: samples follow the 2 s interval with no extra wake, so
/// at most `hold_ms / interval + 1` samples (the `+ 1` is a sample at the phase edge), and a strip
/// redraw needs a changed sample, so redraws never exceed samples plus one. `uncovered` must take
/// at least one sample, the resume sample. `covered`, when the App received `Occluded(true)`
/// (`occluded`), takes at most one sample and one wake: the one that can land between the cover
/// and its delivery. Without delivered occlusion (Windows outside the short trim) the covered
/// phase is not checked, because the platform reports no occlusion to pause on. `select` drags a
/// selection: it must count at least once and read at most 200,000 cells per count; `selected`
/// holds that selection unchanged, so it must not count again. `print` floods the linked history
/// with `history` and `size` listed: `History: trimmed` appears exactly once, and the strip redraws
/// only for a sample or that appearance, never per output frame. `input` types into the idle shell:
/// every accepted key arms an outstanding-work deadline, which finds the write complete, so `held`
/// never appears and the strip redraws only for a sample. `held` pastes behind a stalled reader:
/// `Input: held` may appear at most once on any host ([`status_line_posix_progress`] requires it).
/// `glyphs` prints U+0378: `Glyphs:` appears at most once and the strip redraws only for a
/// sample or that appearance, never again after the fallback worker's publication.
/// `accessibility` holds with every terminal window's adapter installed and dormant and no
/// assistive technology: it keeps the idle cadence and redraw bound and builds no tree.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn status_line_progress(
    phase: &str,
    hold_ms: u64,
    occluded: bool,
    deltas: StatusLineDeltas,
) -> Result<(), String> {
    let cadence_bound = hold_ms / STATUS_LINE_INTERVAL_MS + 1;
    match phase {
        "covered" if occluded && (deltas.samples > 1 || deltas.wakes > 1) => Err(format!(
            "status line sampled {} times with {} wakes while covered",
            deltas.samples, deltas.wakes
        )),
        "covered" => Ok(()),
        "uncovered" if deltas.samples == 0 => {
            Err("status line took no sample after the window was uncovered".to_owned())
        }
        "accessibility" if deltas.tree_builds > 0 => Err(format!(
            "{} accessibility trees were built with no assistive technology connected",
            deltas.tree_builds
        )),
        "idle" | "uncovered" | "accessibility"
            if deltas.samples > cadence_bound || deltas.wakes > cadence_bound =>
        {
            Err(format!(
                "status line sampled {} times with {} wakes in {hold_ms} ms, more than {cadence_bound}",
                deltas.samples, deltas.wakes
            ))
        }
        "idle" | "uncovered" | "accessibility" if deltas.redraws > deltas.samples + 1 => Err(format!(
            "status line requested {} redraws for {} samples",
            deltas.redraws, deltas.samples
        )),
        "print" if deltas.history_shows != 1 => Err(format!(
            "`History: trimmed` appeared {} times while the linked history printed, not once",
            deltas.history_shows
        )),
        "print" if deltas.redraws > deltas.samples + deltas.history_shows + 1 => Err(format!(
            "status line requested {} redraws for {} samples during output",
            deltas.redraws, deltas.samples
        )),
        "select" if deltas.sel_counts == 0 => {
            Err("the drag selected nothing the status line counted".to_owned())
        }
        "select" if deltas.sel_cells > deltas.sel_counts * SEL_CELLS_PER_COUNT => Err(format!(
            "status line read {} cells in {} selection counts, more than {SEL_CELLS_PER_COUNT} each",
            deltas.sel_cells, deltas.sel_counts
        )),
        "input" if deltas.held_shows > 0 => Err(format!(
            "`Input: held` appeared {} times while typing into an idle shell",
            deltas.held_shows
        )),
        "input" if deltas.redraws > deltas.samples + 1 => Err(format!(
            "status line requested {} redraws for {} samples while typing",
            deltas.redraws, deltas.samples
        )),
        "glyphs" if deltas.glyph_shows > 1 => Err(format!(
            "`Glyphs:` appeared {} times for one uncovered character",
            deltas.glyph_shows
        )),
        "glyphs" if deltas.redraws > deltas.samples + deltas.glyph_shows + 1 => Err(format!(
            "status line requested {} redraws for {} samples and {} appearances",
            deltas.redraws, deltas.samples, deltas.glyph_shows
        )),
        "held" if deltas.held_shows > 1 => Err(format!(
            "`Input: held` appeared {} times for one stalled paste",
            deltas.held_shows
        )),
        "selected" if deltas.sel_counts > 0 => Err(format!(
            "status line recounted an unchanged selection {} times",
            deltas.sel_counts
        )),
        _ => Ok(()),
    }
}

/// `accessibility` holds with every adapter dormant: every present, from any path, is one the strip
/// asked for or the slack allows. `presented` is `None` for a base whose App predates the counter.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn accessibility_phase_presents(
    redraws: u64,
    presented: Option<u64>,
) -> Result<(), String> {
    let Some(presented) = presented else {
        // When: `presented` is None, the base has no counter to check.
        return Ok(());
    };
    let allowed = redraws + ACCESSIBILITY_PRESENT_SLACK;
    if presented > allowed {
        // When: `presented` exceeds `allowed`, a path other than the strip drew frames.
        return Err(format!(
            "{presented} frames presented during `accessibility`, more than the strip's {redraws} \
             redraws allow ({allowed})"
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn program_phase_progress(
    strip: StatusLineDeltas,
    program: Option<ProgramDeltas>,
    delivered: Option<&[Delivered]>,
    posix: bool,
    steps: u64,
) -> Result<(), String> {
    if strip.redraws > strip.samples + 1 {
        return Err(format!(
            "program: the strip redrew {} times for {} samples",
            strip.redraws, strip.samples
        ));
    }
    let Some(program) = program else {
        // When: program is None, this side's App predates the program counters; nothing to check.
        return Ok(());
    };
    if program.toast_redraws > program.notifies {
        return Err(format!(
            "program: {} toast redraws for {} notification batches",
            program.toast_redraws, program.notifies
        ));
    }
    if program.deliveries > program.progress {
        return Err(format!(
            "program: {} surface deliveries for {} progress changes",
            program.deliveries, program.progress
        ));
    }
    // Every present needs a cause: a strip redraw or sample, a toast redraw, or a committed
    // step's echo, output and prompt. This holds on Windows too, where GDI presents count alike.
    let present_bound = strip.redraws
        + strip.samples
        + program.toast_redraws
        + steps.saturating_mul(PRESENTS_PER_STEP);
    if program.presented > present_bound {
        return Err(format!(
            "program: {} frames presented, more than the {present_bound} its {} strip redraws, \
             {} samples, {} toast redraws and {steps} steps explain",
            program.presented, strip.redraws, strip.samples, program.toast_redraws
        ));
    }
    if let Some(delivered) = delivered {
        if delivered.len() as u64 != program.deliveries {
            return Err(format!(
                "program: the surface recorded {} values but {} deliveries were counted",
                delivered.len(),
                program.deliveries
            ));
        }
    }
    if !posix {
        // When: posix is false, the role's `cmd` printed no program sequence to count.
        return Ok(());
    }
    if program.notifies == 0 || program.toast_redraws != program.notifies {
        return Err(format!(
            "program: {} notification batches with {} toast redraws",
            program.notifies, program.toast_redraws
        ));
    }
    if program.presented < program.toast_redraws {
        return Err(format!(
            "program: {} toast redraws but only {} frames presented",
            program.toast_redraws, program.presented
        ));
    }
    // The set, 50 and 70 (one change when the burst shares a batch) and the clear.
    if !(3..=4).contains(&program.progress) {
        return Err(format!(
            "program: {} progress changes, not the set, the burst and the clear",
            program.progress
        ));
    }
    let Some(delivered) = delivered else {
        // When: delivered is None, the comparison cannot read the surface's record.
        return Ok(());
    };
    delivered_sequence_problem(delivered).map_or(Ok(()), Err)
}

/// Why the counting surface's record breaks `program`'s delivery contract, if it does.
#[cfg(any(target_os = "macos", windows, test))]
pub(super) fn delivered_sequence_problem(delivered: &[Delivered]) -> Option<String> {
    let values: Vec<Option<u8>> = delivered.iter().map(|delivery| delivery.percent).collect();
    let allowed = [
        vec![Some(40), Some(50), Some(70), None],
        // The PTY can hand 50 and 70 over in one read, so 50 never reaches the App.
        vec![Some(40), Some(70), None],
    ];
    if !allowed.contains(&values) {
        return Some(format!(
            "program: the surface received {values:?}, not 40, the burst's 70, then the clear"
        ));
    }
    if let Some(pair) =
        delivered.windows(2).find(|pair| pair[1].at_ms < pair[0].at_ms + DELIVERY_INTERVAL_MIN_MS)
    {
        return Some(format!(
            "program: deliveries at {} ms and {} ms are closer than the 250 ms interval",
            pair[0].at_ms, pair[1].at_ms
        ));
    }
    None
}

/// The checks that hold only where the role shell is POSIX: there the 256 KiB paste in `held` blocks
/// the writer, so `Input: held` appears exactly once, and `printf` draws U+0378, so `Glyphs:` does.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn status_line_posix_progress(
    phase: &str,
    deltas: StatusLineDeltas,
) -> Result<(), String> {
    match phase {
        "held" if deltas.held_shows != 1 => Err(format!(
            "`Input: held` appeared {} times while the reader was stalled, not once",
            deltas.held_shows
        )),
        "glyphs" if deltas.glyph_shows != 1 => Err(format!(
            "`Glyphs:` appeared {} times after U+0378 was printed, not once",
            deltas.glyph_shows
        )),
        _ => Ok(()),
    }
}

/// Whether a parked phase measured what it claims, or why not.
///
/// Every parked phase must present frames and see the streamed rows advance, or it measured an
/// idle screen; its screen and viewport must stay the ones it started on, and a repaint of its
/// fixture rows makes the run invalid rather than passing. `rest` claims the pointer rests over
/// unchanged rows, so with counters on it must hit the hover cache and run no detection and
/// submit no probe after warm-up. `tail` rests on streaming rows, so its pointed row must change
/// and, with counters on, it must report detections: the cache misses there by design.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn park_progress(phase: &str, evidence: &ParkEvidence) -> Result<(), String> {
    if evidence.presented_frames == 0 {
        // When: no frame presented, the phase measured nothing the pointer could rest over.
        return Err(format!("S6/rest phase {phase} presented no frame"));
    }
    if !evidence.streamed_rows_advanced {
        // When: the streamed rows never changed, the scroll region did not stream.
        return Err(format!("S6/rest phase {phase} saw no streamed row advance"));
    }
    if !evidence.screen_unchanged {
        // When: the screen identity or viewport changed, the rows are not the ones the phase started on.
        return Err(format!("S6/rest phase {phase} changed its screen or viewport"));
    }
    if !evidence.fixture_unchanged {
        // When: a fixture row was rewritten, the pointer no longer rests over unchanged rows.
        return Err(format!("S6/rest phase {phase} saw its fixture rows repainted"));
    }
    if phase == "tail" {
        if !evidence.pointed_row_changed {
            // When: the tail's pointed row never changed, the phase rested on a stationary row.
            return Err("S6/rest phase tail rested on a row that never changed".to_owned());
        }
        if evidence.hover.is_some_and(|hover| hover.detections == 0) {
            // When: the tail rests on changing rows yet ran no detection, the hover counters are broken.
            return Err("S6/rest phase tail ran no detection over streaming rows".to_owned());
        }
    }
    if let (true, Some(hover)) = (phase == "rest", evidence.hover) {
        // When: the rest phase has counters, its warm cache must answer every lookup.
        if hover.detections > 0 || hover.probes_submitted > 0 {
            // When: detections or probes_submitted is nonzero, the rest phase did work after warm-up.
            return Err(format!(
                "S6/rest phase rest ran {} detections and {} probe submissions after warm-up",
                hover.detections, hover.probes_submitted
            ));
        }
        if hover.cache_hits == 0 {
            // When: cache_hits is zero, no lookup reused the warm result.
            return Err("S6/rest phase rest never hit the hover cache".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "scenarios_progress_tests.rs"]
mod scenarios_progress_tests;
