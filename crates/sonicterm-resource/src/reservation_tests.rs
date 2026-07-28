use super::*;

#[test]
fn reservation_and_committed_tokens_are_not_clone() {
    // An inherent associated const is preferred over the blanket trait const, so
    // `IS_CLONE` resolves to `true` only when the probed type actually implements
    // `Clone`. A future `#[derive(Clone)]` on either token would duplicate every
    // charge and is caught here rather than at review time.
    struct Probe<T>(core::marker::PhantomData<T>);

    trait NotClone {
        const IS_CLONE: bool = false;
    }

    impl<T> NotClone for Probe<T> {}

    impl<T: Clone> Probe<T> {
        const IS_CLONE: bool = true;
    }

    const { assert!(<Probe<u32>>::IS_CLONE, "probe must detect a Clone type") };
    const { assert!(!<Probe<Reservation>>::IS_CLONE) };
    const { assert!(!<Probe<CommittedReservation>>::IS_CLONE) };
}

#[test]
fn reservation_and_committed_tokens_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Reservation>();
    assert_send_sync::<CommittedReservation>();
}

#[test]
fn ownership_preserving_errors_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<CommitError>();
    assert_send::<TransferError>();
    assert_send::<CommittedTransferError>();
}

#[test]
fn an_unappliable_release_is_counted_rather_than_lost() {
    // Injects the failure the counter exists for: a charge that claims more
    // than the ledger holds. Dropping it must record the fault instead of
    // panicking, so the same behavior is observable in test and shipped
    // builds rather than only where debug assertions run.
    use crate::{ledger::Ledger, reservation::Charge};
    use enum_map::enum_map;
    use sonicterm_types::{
        GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
    };

    let ledger = Ledger::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let window = ledger
        .create_child(
            ledger.root,
            OwnerKind::Window,
            OwnerLimits {
                owner_bytes: usize::MAX,
                class_bytes: enum_map! { _ => usize::MAX },
                class_items: enum_map! { _ => None },
            },
        )
        .unwrap();

    assert_eq!(ledger.snapshot(ledger.root).unwrap().release_failures, 0);
    let overstated = Reservation::new(Charge {
        ledger: ledger.clone(),
        owner: window,
        class: ResourceClass::PtyOutput,
        amount: ResourceAmount { bytes: 4096, items: 1 },
    });
    drop(overstated);

    let snapshot = ledger.snapshot(ledger.root).unwrap();
    assert_eq!(snapshot.release_failures, 1, "an unappliable release must be counted");
    assert_eq!(
        snapshot.process_amount,
        ResourceAmount::default(),
        "a failed release must not push totals negative"
    );
}
