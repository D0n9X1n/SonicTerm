//! Deterministic helpers for contract and downstream tests.

use crate::reservation::Charge;
use crate::{Reservation, ResourceGovernor};
use enum_map::enum_map;
use sonicterm_types::{
    GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
    ResourceOwnerId,
};

/// Create a real governor with effectively unlimited immutable limits.
///
/// Accounting, owner state, and exact-once RAII behavior remain enabled.
pub fn unlimited_governor(kind: ProcessKind) -> ResourceGovernor {
    ResourceGovernor::new(
        kind,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .expect("unlimited test governor")
}

/// Owner limits that admit anything, for tests about something other than
/// admission.
pub fn unlimited_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

/// Drive `governor` into the permanently-inconsistent state and return the
/// owner the unappliable release was attributed to.
///
/// `release_failures` becomes non-zero only when a release cannot be applied,
/// which a correct ledger never produces — so the state is unreachable through
/// the public API by design. That leaves any code path that *reports* the
/// inconsistency untestable from outside this crate, including the diagnostic
/// banner whose whole purpose is to make it impossible to miss.
///
/// This constructs a [`Charge`] the ledger never issued, so dropping it
/// attempts to release bytes that were never reserved. `Charge` is
/// `pub(crate)`, which is why the helper has to live here rather than in the
/// crate that formats the result.
///
/// Feature-gated behind `test-util` and therefore unreachable from a release
/// build: nothing here should ever run in production, since its only purpose
/// is to corrupt accounting on purpose.
pub fn corrupt_ledger_accounting(governor: &ResourceGovernor) -> ResourceOwnerId {
    let owner = governor
        .create_child(governor.root_owner(), OwnerKind::Window, unlimited_owner_limits())
        .expect("an unlimited governor admits a child");

    let never_reserved = Reservation::new(Charge {
        ledger: governor.ledger_for_test_support(),
        owner,
        class: ResourceClass::PtyOutput,
        amount: ResourceAmount { bytes: 4096, items: 1 },
    });
    drop(never_reserved);

    owner
}

#[cfg(test)]
#[path = "test_support_tests.rs"]
mod test_support_tests;
