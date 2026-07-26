//! Tests for [`format_snapshot`].
//!
//! Every snapshot here comes from a **real governor** via
//! `sonicterm-resource`'s `test-util` seam, not from a hand-built struct.
//! `ResourceSnapshot` is `#[non_exhaustive]`, so a literal is not even
//! constructible outside its crate — but the better reason is that a
//! hand-built one would encode what I *expect* the ledger to produce, and the
//! formatter's entire job is to render what it *actually* produces.

use super::*;

use enum_map::enum_map;
use sonicterm_resource::test_support::unlimited_governor;
use sonicterm_resource::ResourceGovernor;
use sonicterm_types::{
    OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass, ResourceOwnerId,
};

fn governor() -> ResourceGovernor {
    unlimited_governor(ProcessKind::Gui)
}

fn unlimited_owner_limits() -> OwnerLimits {
    // Explicit rather than defaulted: `OwnerLimits` has no `Default`, and a
    // child with real ceilings would make these tests about admission rather
    // than about formatting.
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

fn child_of(governor: &ResourceGovernor, parent: ResourceOwnerId) -> ResourceOwnerId {
    governor
        .create_child(parent, OwnerKind::Window, unlimited_owner_limits())
        .expect("an unlimited governor admits a child")
}

/// An idle owner reports zero without inventing detail.
///
/// A report that lists thirty zeroed classes is one an operator skims, which
/// is the same as no report at all.
#[test]
fn an_idle_owner_reports_nothing_held() {
    let governor = governor();
    let snapshot = governor.snapshot(governor.root_owner()).expect("root always snapshots");

    let text = format_snapshot(&snapshot);

    assert!(
        text.contains("classes holding anything: none"),
        "an idle owner must say so plainly, got:\n{text}"
    );
    assert!(
        !text.contains("LEDGER INCONSISTENT"),
        "a healthy ledger must not be reported as inconsistent:\n{text}"
    );
    assert!(dominant_class(&snapshot).is_none(), "nothing held means no dominant class");
    assert!(!is_ledger_inconsistent(&snapshot));
}

/// A held charge appears, and the class holding it is named.
#[test]
fn a_held_charge_names_its_class() {
    let governor = governor();
    let root = governor.root_owner();
    let _reservation = governor
        .try_reserve(root, ResourceClass::GridHistory, ResourceAmount { bytes: 4096, items: 2 })
        .expect("an unlimited governor admits a reservation");

    let snapshot = governor.snapshot(root).expect("root snapshots");
    let text = format_snapshot(&snapshot);

    assert!(text.contains("GridHistory"), "the holding class must be named:\n{text}");
    assert!(text.contains("4096"), "the held bytes must appear:\n{text}");
    assert_eq!(
        dominant_class(&snapshot),
        Some((ResourceClass::GridHistory, 4096)),
        "the only class holding anything is the dominant one"
    );
}

/// The dominant class is the largest, not the first or the last.
///
/// An operator reads this line to decide which subsystem to look at, so
/// picking the wrong one sends them to the wrong place with full confidence.
#[test]
fn the_dominant_class_is_the_largest_one() {
    let governor = governor();
    let root = governor.root_owner();

    let _small = governor
        .try_reserve(root, ResourceClass::ParserCapture, ResourceAmount { bytes: 1024, items: 1 })
        .expect("reserved");
    let _large = governor
        .try_reserve(
            root,
            ResourceClass::InlineMediaRetained,
            ResourceAmount { bytes: 64 * 1024 * 1024, items: 16 },
        )
        .expect("reserved");
    let _medium = governor
        .try_reserve(root, ResourceClass::GlyphAtlas, ResourceAmount { bytes: 16 * 1024, items: 4 })
        .expect("reserved");

    let snapshot = governor.snapshot(root).expect("root snapshots");

    let (class, bytes) = dominant_class(&snapshot).expect("three classes hold something");
    assert_eq!(class, ResourceClass::InlineMediaRetained);
    assert_eq!(bytes, 64 * 1024 * 1024);

    let text = format_snapshot(&snapshot);
    assert!(
        text.contains("dominant class: InlineMediaRetained"),
        "the report must name the largest class:\n{text}"
    );
    // And the smaller classes are still listed — the dominant line is a
    // summary, not a filter.
    assert!(text.contains("ParserCapture"), "smaller classes must still appear:\n{text}");
    assert!(text.contains("GlyphAtlas"), "smaller classes must still appear:\n{text}");
}

/// Classes holding nothing are omitted.
#[test]
fn empty_classes_are_omitted_from_the_report() {
    let governor = governor();
    let root = governor.root_owner();
    let _held = governor
        .try_reserve(root, ResourceClass::PtyOutput, ResourceAmount { bytes: 8192, items: 1 })
        .expect("reserved");

    let text = format_snapshot(&governor.snapshot(root).expect("root snapshots"));

    assert!(text.contains("PtyOutput"), "the held class must appear:\n{text}");
    assert!(
        !text.contains("MuxSubscriber"),
        "a class holding nothing is not evidence and must be omitted:\n{text}"
    );
}

/// A child owner reports its own charge distinctly from the process total.
///
/// Conflating them would tell an operator a window is responsible for
/// everything the process holds.
#[test]
fn a_child_owner_is_distinguished_from_the_process_total() {
    let governor = governor();
    let root = governor.root_owner();
    let child = child_of(&governor, root);

    let _root_charge = governor
        .try_reserve(root, ResourceClass::GridVisible, ResourceAmount { bytes: 10_000, items: 1 })
        .expect("reserved");
    let _child_charge = governor
        .try_reserve(child, ResourceClass::GridVisible, ResourceAmount { bytes: 3_000, items: 1 })
        .expect("reserved");

    let snapshot = governor.snapshot(child).expect("child snapshots");
    let text = format_snapshot(&snapshot);

    assert!(
        snapshot.process_amount.bytes > snapshot.owner_amount.bytes,
        "the process must hold more than this one child: process {} owner {}",
        snapshot.process_amount.bytes,
        snapshot.owner_amount.bytes
    );
    assert!(text.contains("3000"), "the child's own charge must appear:\n{text}");
    assert!(text.contains("13000"), "the process total must appear:\n{text}");
    assert!(text.contains("parent="), "a child must report its parent:\n{text}");
}

/// The process root reports that it has no parent, rather than omitting it.
#[test]
fn the_process_root_says_it_has_no_parent() {
    let governor = governor();
    let text = format_snapshot(&governor.snapshot(governor.root_owner()).expect("snapshots"));

    assert!(
        text.contains("parent=none (process root)"),
        "an absent parent must be stated, not left blank:\n{text}"
    );
}

/// The report carries no user content.
///
/// Redaction is enforced crate-wide by `redaction_tests`, but that scans
/// tracing call sites. This formatter builds a string directly, so it needs
/// its own check: nothing it emits may come from a URL, path, or command.
#[test]
fn the_report_contains_only_classes_counts_and_sizes() {
    let governor = governor();
    let root = governor.root_owner();
    let _held = governor
        .try_reserve(
            root,
            ResourceClass::InlineMediaDecode,
            ResourceAmount { bytes: 2048, items: 1 },
        )
        .expect("reserved");

    let text = format_snapshot(&governor.snapshot(root).expect("snapshots"));

    for forbidden in ["http", "://", "/Users/", "\\Users\\", ".com", "$", "~/"] {
        assert!(
            !text.contains(forbidden),
            "the report must never carry user content; found {forbidden:?} in:\n{text}"
        );
    }
}

/// Every line of a healthy report is accounted for.
///
/// Guards against the formatter silently emitting less than it claims — the
/// vacuous-pass failure mode that has already produced three
/// non-discriminating tests in this milestone.
#[test]
fn the_report_has_the_lines_the_other_tests_rely_on() {
    let governor = governor();
    let root = governor.root_owner();
    let _held = governor
        .try_reserve(root, ResourceClass::GridHistory, ResourceAmount { bytes: 512, items: 1 })
        .expect("reserved");

    let text = format_snapshot(&governor.snapshot(root).expect("snapshots"));
    let lines: Vec<&str> = text.lines().collect();

    assert!(lines.len() >= 5, "a report with a held charge needs at least five lines:\n{text}");
    assert!(lines[0].starts_with("owner "), "the first line identifies the owner:\n{text}");
    assert!(
        lines.iter().any(|line| line.trim_start().starts_with("owner   ")),
        "an owner-total line must exist:\n{text}"
    );
    assert!(
        lines.iter().any(|line| line.trim_start().starts_with("process ")),
        "a process-total line must exist:\n{text}"
    );
}

/// An inconsistent ledger says so first, and says why it matters.
///
/// `release_failures != 0` means a release could not be applied: the process
/// ceiling is over-counted for the life of the process and no owner can reach
/// zero. Every other figure on the snapshot is then measured against a ceiling
/// that is already wrong, which is why this leads rather than appearing as one
/// field among thirty.
///
/// The state is unreachable through the public API — a correct ledger never
/// produces it — so this uses the `test-util` seam that constructs a charge
/// the ledger never issued. Without that seam the most important branch in
/// this formatter would ship untested.
#[test]
fn an_inconsistent_ledger_is_reported_first_and_explained() {
    let governor = governor();
    let owner = sonicterm_resource::test_support::corrupt_ledger_accounting(&governor);

    let snapshot = governor.snapshot(owner).expect("a corrupt ledger still snapshots");
    assert!(
        is_ledger_inconsistent(&snapshot),
        "precondition: the ledger must actually be inconsistent, release_failures={}",
        snapshot.release_failures
    );

    let text = format_snapshot(&snapshot);
    let first_line = text.lines().next().expect("the report is not empty");

    assert!(
        first_line.contains("LEDGER INCONSISTENT"),
        "the inconsistency must be the first thing an operator reads, not buried \
         among the accounting lines. First line was:\n  {first_line}\nfull report:\n{text}"
    );
    assert!(
        first_line.contains("cannot") || first_line.contains("no owner can reach zero"),
        "the banner must say what it means, not just name the field:\n  {first_line}"
    );
    assert!(
        text.contains("owner "),
        "the rest of the report must still be present after the banner:\n{text}"
    );
}

/// A healthy ledger carries no banner.
///
/// Without this the banner could be printed unconditionally and the test above
/// would still pass — an alarm that is always on is an alarm nobody reads.
#[test]
fn a_healthy_ledger_carries_no_inconsistency_banner() {
    let governor = governor();
    let root = governor.root_owner();
    let _held = governor
        .try_reserve(root, ResourceClass::GridVisible, ResourceAmount { bytes: 2048, items: 1 })
        .expect("reserved");

    let text = format_snapshot(&governor.snapshot(root).expect("snapshots"));

    assert!(
        !text.contains("LEDGER INCONSISTENT"),
        "a consistent ledger must not raise the alarm:\n{text}"
    );
    assert!(text.lines().next().expect("non-empty").starts_with("owner "));
}
