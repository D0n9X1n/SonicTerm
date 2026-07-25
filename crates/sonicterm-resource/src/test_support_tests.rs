use super::*;
use sonicterm_types::{OwnerKind, OwnerLimits, ResourceAmount, ResourceClass};

#[test]
fn unlimited_test_governor_keeps_real_accounting() {
    let governor = unlimited_governor(sonicterm_types::ProcessKind::Gui);
    let limits = OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    };
    let window =
        governor.create_child(governor.root_owner(), OwnerKind::Window, limits.clone()).unwrap();
    let owner = governor.create_child(window, OwnerKind::AppPane, limits).unwrap();
    let reservation = governor
        .try_reserve(owner, ResourceClass::GridVisible, ResourceAmount { bytes: 4, items: 1 })
        .unwrap();
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 4);
    drop(reservation);
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount, ResourceAmount::default());
}
