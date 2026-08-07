use super::*;

/// An unsupported figure never renders as a number.
///
/// The whole value of this type is that a reader can tell "this platform does
/// not expose it" from "this process holds nothing". If `Unsupported` ever
/// formatted as `0`, every conclusion drawn from a macOS private/committed
/// reading would be wrong in the same direction — the process would look
/// smaller than it is, in the one report a growth investigation trusts.
#[test]
fn an_unsupported_metric_never_renders_as_a_number() {
    assert_eq!(MemoryMetric::Unsupported.to_string(), "unsupported");
    assert_eq!(MemoryMetric::Bytes(0).to_string(), "0");
    assert_ne!(
        MemoryMetric::Unsupported.to_string(),
        MemoryMetric::Bytes(0).to_string(),
        "a measured zero and an unavailable figure must be distinguishable in the log"
    );
}

/// A measured zero is a measurement, and reports itself as one.
///
/// The pair to the test above: `bytes()` returns `Some(0)` for a real zero so
/// a delta against it is computable, and `None` for unsupported so a delta
/// against it is refused.
#[test]
fn a_measured_zero_is_distinguishable_from_an_absent_figure() {
    assert_eq!(MemoryMetric::Bytes(0).bytes(), Some(0));
    assert_eq!(MemoryMetric::Unsupported.bytes(), None);
}

/// A delta needs two measurements; anything else is unavailable.
///
/// Reporting `+0` for a figure that was never measured would claim the process
/// did not move, which is a stronger statement than the data supports and is
/// indistinguishable from a genuinely flat sample.
#[test]
fn a_delta_against_an_unmeasured_figure_is_unavailable() {
    let measured = MemoryMetric::Bytes(100);
    let absent = MemoryMetric::Unsupported;

    assert_eq!(MemoryDelta::between(absent, measured), MemoryDelta::Unavailable);
    assert_eq!(MemoryDelta::between(measured, absent), MemoryDelta::Unavailable);
    assert_eq!(MemoryDelta::between(absent, absent), MemoryDelta::Unavailable);
    assert_eq!(
        MemoryDelta::between(measured, measured),
        MemoryDelta::Changed(0),
        "two equal measurements are a real zero delta, not an absent one"
    );
}

/// Growth and shrinkage both carry an explicit sign.
///
/// A growth curve is read by eye from successive snapshots, so the sign is
/// always written rather than implied by column position.
#[test]
fn a_delta_carries_direction_in_its_rendering() {
    let grew = MemoryDelta::between(MemoryMetric::Bytes(100), MemoryMetric::Bytes(250));
    let shrank = MemoryDelta::between(MemoryMetric::Bytes(250), MemoryMetric::Bytes(100));

    assert_eq!(grew, MemoryDelta::Changed(150));
    assert_eq!(shrank, MemoryDelta::Changed(-150));
    assert_eq!(grew.to_string(), "+150", "a positive delta states its sign");
    assert_eq!(shrank.to_string(), "-150");
    assert_eq!(MemoryDelta::Unavailable.to_string(), "unavailable");
}

/// A delta between enormous figures reports rather than panicking.
///
/// The snapshot exists to be read when a process is alarmingly large. A
/// diagnostic that overflows and panics on the exact input it was written for
/// is absent precisely when it is needed.
#[test]
fn an_enormous_delta_saturates_rather_than_panicking() {
    let delta = MemoryDelta::between(MemoryMetric::Bytes(0), MemoryMetric::Bytes(u64::MAX));
    assert!(matches!(delta, MemoryDelta::Changed(_)), "must produce a figure, not panic");
}

/// The pressure shape carries only fixed-cost process figures.
///
/// Exhaustive destructuring makes any added field update this test, so a costly
/// virtual-address-space metric cannot enter the frequent sample unnoticed.
#[test]
fn process_pressure_has_no_virtual_address_space_field() {
    let ProcessPressure { private_committed, resident } = ProcessPressure::unsupported();
    assert_eq!(private_committed, MemoryMetric::Unsupported);
    assert_eq!(resident, MemoryMetric::Unsupported);
}

/// The Windows pressure path never starts the address-space walk.
///
/// A thread-local counter surrounds both call orders to distinguish the cheap
/// pressure query from the full sampler's single `VirtualQuery` traversal.
#[cfg(windows)]
#[test]
fn pressure_sampling_skips_reserved_address_space_in_both_call_orders() {
    reset_reserved_address_space_calls();
    let _ = sample_pressure();
    assert_eq!(reserved_address_space_calls(), 0);
    let _ = sample();
    assert_eq!(reserved_address_space_calls(), 1);

    reset_reserved_address_space_calls();
    let _ = sample();
    assert_eq!(reserved_address_space_calls(), 1);
    let _ = sample_pressure();
    assert_eq!(reserved_address_space_calls(), 1);
}

/// macOS pressure sampling reports the supported resident figure honestly.
///
/// The live platform query must yield resident bytes while preserving the unavailable commit field.
#[cfg(target_os = "macos")]
#[test]
fn macos_pressure_measures_resident_and_declares_private_unsupported() {
    let pressure = sample_pressure();
    assert!(matches!(pressure.resident, MemoryMetric::Bytes(bytes) if bytes > 0));
    assert_eq!(pressure.private_committed, MemoryMetric::Unsupported);
}

/// The sampler answers, and its answer is internally consistent.
///
/// Deliberately not asserting a byte range: this runs on developer machines
/// and CI runners with wildly different memory profiles, and a threshold that
/// held on one would be flaky on another. What is asserted is the property
/// that must hold on every platform — each figure is either a real
/// measurement or an explicit `Unsupported`, and never a fabricated zero.
#[test]
fn sampling_produces_measurements_or_explicit_unavailability() {
    let sample = sample();

    for (name, metric) in [
        ("private_committed", sample.private_committed),
        ("resident", sample.resident),
        ("virtual", sample.virtual_bytes),
    ] {
        match metric {
            MemoryMetric::Bytes(bytes) => {
                assert!(
                    bytes > 0,
                    "{name} reported a measured zero; a live process holds something, so this \
                     is a failed query being reported as a measurement"
                );
            }
            MemoryMetric::Unsupported => {}
        }
    }
}

/// macOS reports resident and virtual, and says so about the rest.
///
/// This pins the platform contract rather than a number. `proc_pidinfo`
/// populates resident and virtual for any live task, so a run that reports
/// either as unavailable means the query path broke — which would otherwise
/// look identical to a platform that simply does not support it.
///
/// Private/committed is asserted unsupported deliberately. The honest macOS
/// figure is `phys_footprint` in `task_vm_info`, which `libc` does not expose
/// at this MSRV; reading it would mean hand-declaring a kernel struct layout,
/// where a mismatch returns a plausible wrong number rather than failing to
/// build. If a future change makes it genuinely available, this assertion is
/// the one to update — and it will fail loudly rather than silently drift.
#[cfg(target_os = "macos")]
#[test]
fn macos_measures_resident_and_virtual_and_declares_private_unsupported() {
    let sample = sample();

    assert!(
        matches!(sample.resident, MemoryMetric::Bytes(bytes) if bytes > 0),
        "macOS exposes resident size for any live task; got {:?}",
        sample.resident
    );
    assert!(
        matches!(sample.virtual_bytes, MemoryMetric::Bytes(bytes) if bytes > 0),
        "macOS exposes virtual size for any live task; got {:?}",
        sample.virtual_bytes
    );
    assert_eq!(
        sample.private_committed,
        MemoryMetric::Unsupported,
        "macOS private/committed must be reported unavailable rather than guessed from a \
         hand-declared kernel struct layout"
    );
}

/// Reserved address space is at least the resident set.
///
/// An arithmetic sanity check on the pair, which catches the failure mode a
/// range assertion cannot: the two fields being swapped, or one being read
/// from the wrong offset of the kernel structure. Every resident page occupies
/// address space, so virtual can never be the smaller of the two.
#[test]
fn virtual_address_space_is_never_smaller_than_the_resident_set() {
    let sample = sample();
    let (Some(resident), Some(virtual_bytes)) =
        (sample.resident.bytes(), sample.virtual_bytes.bytes())
    else {
        // A platform that does not measure both has nothing to compare.
        return;
    };

    assert!(
        virtual_bytes >= resident,
        "virtual ({virtual_bytes}) < resident ({resident}); every resident page occupies \
         address space, so this means the two figures are transposed or misread"
    );
}

/// Successive samples of a live process both answer.
///
/// A sampler that worked once and then returned `Unsupported` — a consumed
/// handle, a one-shot initialisation — would look correct in a single-shot
/// test and produce exactly one useful snapshot per session in production.
#[test]
fn sampling_is_repeatable_within_a_session() {
    let first = sample();
    let second = sample();

    assert_eq!(
        first.resident.bytes().is_some(),
        second.resident.bytes().is_some(),
        "a second sample must have the same availability as the first; a sampler that \
         degrades after one call yields one useful snapshot per session"
    );
    assert_eq!(
        first.virtual_bytes.bytes().is_some(),
        second.virtual_bytes.bytes().is_some(),
        "virtual availability must not change between samples"
    );
}

/// The unsupported constructor claims nothing about any figure.
///
/// Used by every failed-query path, so a field that leaked a zero through it
/// would put an invented measurement in the report on exactly the paths where
/// nothing was measured.
#[test]
fn the_unsupported_constructor_measures_nothing() {
    let none = ProcessMemory::unsupported();
    assert_eq!(none.private_committed, MemoryMetric::Unsupported);
    assert_eq!(none.resident, MemoryMetric::Unsupported);
    assert_eq!(none.virtual_bytes, MemoryMetric::Unsupported);
}
