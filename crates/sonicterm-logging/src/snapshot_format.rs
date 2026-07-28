//! Human-readable rendering of a [`ResourceSnapshot`].
//!
//! A snapshot has upward of thirty fields, most of them zero at any moment.
//! Printing all of them produces a wall an operator skims, which is the same
//! as printing nothing — and the one field that means *"this process cannot
//! recover"* reads exactly like the twenty-nine that mean nothing is wrong.
//!
//! So this formatter is opinionated about ordering rather than complete:
//!
//! 1. **Inconsistency first.** `release_failures != 0` means the process
//!    ceiling is permanently over-counted and an owner can never reach zero.
//!    Nothing else on the snapshot matters while that is true, because every
//!    other figure is measured against a ceiling that is already wrong.
//! 2. **Then the dominant class**, because a total says a process is large
//!    without saying which subsystem to look at, and the remedy differs per
//!    class.
//! 3. **Then non-zero classes only.** A class holding nothing is not evidence.
//!
//! Nothing here carries user content — class names, counts, and byte sizes
//! only — which is what the redaction tests in this crate enforce.

use std::fmt::Write as _;

use sonicterm_types::{ResourceClass, ResourceSnapshot};

/// Render `snapshot` as an operator-facing report.
///
/// Multi-line and stable in shape, so successive reports can be diffed to see
/// what moved — the question a growth investigation actually asks.
#[must_use]
pub fn format_snapshot(snapshot: &ResourceSnapshot) -> String {
    let mut out = String::with_capacity(512);

    if snapshot.release_failures != 0 {
        // Deliberately first and deliberately loud: while this is non-zero
        // every other figure below is measured against a ceiling that is
        // already wrong, so reading them as normal accounting is misleading.
        let _ = writeln!(
            out,
            "LEDGER INCONSISTENT: {} release(s) could not be applied. The process \
             ceiling is permanently over-counted and no owner can reach zero. \
             Figures below are measured against that wrong ceiling.",
            snapshot.release_failures
        );
    }

    let _ = writeln!(
        out,
        "owner {:?} kind={:?} state={:?} parent={}",
        snapshot.owner,
        snapshot.owner_kind,
        snapshot.owner_state,
        snapshot.parent.map_or_else(|| "none (process root)".to_string(), |id| format!("{id:?}")),
    );
    let _ = writeln!(
        out,
        "  owner   {:>12} bytes  {:>8} items",
        snapshot.owner_amount.bytes, snapshot.owner_amount.items
    );
    let _ = writeln!(
        out,
        "  process {:>12} bytes  {:>8} items  ({:?})",
        snapshot.process_amount.bytes, snapshot.process_amount.items, snapshot.process_kind
    );

    if let Some((class, bytes)) = dominant_class(snapshot) {
        let share = if snapshot.process_amount.bytes == 0 {
            0.0
        } else {
            (bytes as f64 / snapshot.process_amount.bytes as f64) * 100.0
        };
        let _ =
            writeln!(out, "  dominant class: {class:?} at {bytes} bytes ({share:.1}% of process)");
    }

    let mut any_class = false;
    for (class, bytes) in snapshot.process_class_bytes.iter() {
        if *bytes == 0 && snapshot.process_class_items[class] == 0 {
            continue;
        }
        if !any_class {
            let _ = writeln!(out, "  classes holding anything:");
            any_class = true;
        }
        let owner_bytes = snapshot.owner_class_bytes[class];
        let _ = writeln!(
            out,
            "    {class:?}: process {} bytes / {} items, this owner {} bytes",
            bytes, snapshot.process_class_items[class], owner_bytes
        );
    }
    if !any_class {
        let _ = writeln!(out, "  classes holding anything: none");
    }

    out
}

/// The process-wide class holding the most bytes, if any holds any.
///
/// Returned rather than printed inline so callers can act on it — an alert
/// that names the dominant class is worth more than one that reports a total.
#[must_use]
pub fn dominant_class(snapshot: &ResourceSnapshot) -> Option<(ResourceClass, usize)> {
    snapshot
        .process_class_bytes
        .iter()
        .filter(|(_, bytes)| **bytes > 0)
        .max_by_key(|(_, bytes)| **bytes)
        .map(|(class, bytes)| (class, *bytes))
}

/// Whether the snapshot shows an unrecoverable accounting inconsistency.
///
/// Exposed separately from the formatted text so a caller can branch on it
/// without parsing prose. A ledger in this state does not repair itself: the
/// over-count persists for the life of the process, so the correct response is
/// to report it, not to retry.
#[must_use]
pub fn is_ledger_inconsistent(snapshot: &ResourceSnapshot) -> bool {
    snapshot.release_failures != 0
}

#[cfg(test)]
#[path = "snapshot_format_tests.rs"]
mod snapshot_format_tests;
