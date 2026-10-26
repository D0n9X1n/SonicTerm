use crate::governor_test_support::*;
use crate::*;

#[test]
fn multi_owner_transfer_refuses_the_entire_batch_before_any_reattribution() {
    // A refusal in the second target cannot leave the first pane attributed away from its restored tab.
    let governor = governor(1000);
    let first = app_pane(&governor, 1000);
    let second = app_pane(&governor, 1000);
    let destination = governor
        .create_child(governor.root_owner(), OwnerKind::Window, owner_limits(1000))
        .unwrap();
    let good = governor.create_child(destination, OwnerKind::AppPane, owner_limits(1000)).unwrap();
    let small = governor.create_child(destination, OwnerKind::AppPane, owner_limits(1)).unwrap();
    let amount = ResourceAmount { bytes: 20, items: 2 };
    let mut first_reservation = governor
        .try_reserve(first, ResourceClass::GridVisible, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    let mut second_reservation = governor
        .try_reserve(second, ResourceClass::InlineMediaRetained, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    let root_before = governor.snapshot(governor.root_owner()).unwrap();
    assert!(CommittedReservation::transfer_many([
        (&mut first_reservation, good),
        (&mut second_reservation, small)
    ])
    .is_err());
    assert_eq!(governor.snapshot(first).unwrap().owner_amount, amount);
    assert_eq!(governor.snapshot(second).unwrap().owner_amount, amount);
    assert_eq!(governor.snapshot(destination).unwrap().owner_amount, ResourceAmount::default());
    assert_eq!(
        governor.snapshot(governor.root_owner()).unwrap().class_epochs,
        root_before.class_epochs
    );
    let other = governor.create_child(destination, OwnerKind::AppPane, owner_limits(1000)).unwrap();
    CommittedReservation::transfer_many([
        (&mut first_reservation, good),
        (&mut second_reservation, other),
    ])
    .unwrap();
    assert_eq!(governor.snapshot(first).unwrap().owner_amount, ResourceAmount::default());
    assert_eq!(governor.snapshot(second).unwrap().owner_amount, ResourceAmount::default());
    assert_eq!(
        governor.snapshot(destination).unwrap().owner_amount,
        ResourceAmount { bytes: 40, items: 4 }
    );
    assert_eq!(
        governor.snapshot(governor.root_owner()).unwrap().process_amount,
        root_before.process_amount
    );
    drop(first_reservation);
    drop(second_reservation);
    assert_eq!(governor.snapshot(destination).unwrap().owner_amount, ResourceAmount::default());
}

#[test]
fn multi_owner_transfer_accepts_final_limit_swaps_without_transient_growth() {
    // Opposite moves can exchange full-budget owners because only final balances govern admission.
    let governor = governor(100);
    let first = app_pane(&governor, 50);
    let second = app_pane(&governor, 50);
    let amount = ResourceAmount { bytes: 50, items: 1 };
    let mut first_reservation = governor
        .try_reserve(first, ResourceClass::GridVisible, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    let mut second_reservation = governor
        .try_reserve(second, ResourceClass::GridVisible, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    CommittedReservation::transfer_many([
        (&mut first_reservation, second),
        (&mut second_reservation, first),
    ])
    .unwrap();
    assert_eq!(governor.snapshot(first).unwrap().owner_amount, amount);
    assert_eq!(governor.snapshot(second).unwrap().owner_amount, amount);
    drop(first_reservation);
    assert_eq!(governor.snapshot(second).unwrap().owner_amount, ResourceAmount::default());
    drop(second_reservation);
    assert_eq!(
        governor.snapshot(governor.root_owner()).unwrap().process_amount,
        ResourceAmount::default()
    );
}
