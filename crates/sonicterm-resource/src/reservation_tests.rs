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
