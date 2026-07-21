use super::*;
use sonicterm_io::pty::PtyInputError;

#[test]
fn saturated_input_event_retains_rejected_bytes_for_user_notification() {
    let rejected = b"retry me".to_vec();

    let event = pty_input_rejected_event(PtyInputError::QueueFull(rejected.clone()));

    assert!(matches!(
        event,
        UserEvent::PtyInputRejected { bytes, reason }
            if bytes == rejected && reason.contains("queue is full")
    ));
}
