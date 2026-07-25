use super::*;

#[test]
fn reservation_and_committed_tokens_are_not_clone() {
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
