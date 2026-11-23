//! Governor owner close: success and the refusals `OwnerGuard` logs instead of retrying.

use super::*;
use enum_map::enum_map;

fn test_governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap()
}

fn owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

/// `close_owner` removes an uncharged owner and returns the governor's refusal for one that still
/// holds a charge, so the caller can log it rather than lose the ledger record silently.
#[test]
fn owner_cleanup_returns_success_and_governor_refusals() {
    let governor = test_governor();
    let clean =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    close_owner(&governor, clean).unwrap();
    assert!(governor.snapshot(clean).is_err());

    let charged =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let reservation = governor
        .try_reserve(
            charged,
            ResourceClass::GridVisible,
            sonicterm_types::ResourceAmount { bytes: 1, items: 1 },
        )
        .unwrap();

    let error = close_owner(&governor, charged).expect_err("live charges must remain observable");
    assert!(
        matches!(error, sonicterm_types::BudgetError::OwnerHasLiveCharges { owner, .. } if owner == charged)
    );
    drop(reservation);
}
