use super::*;
use crate::app::osc52_read::MAX_TICKETS;
use crate::app::osc52_stream::ClipboardStaging;
use sonicterm_cfg::config::Osc52ReadPolicy;

/// A private staging under `policy`, so no sibling test's tickets fill the process gate.
fn staging(policy: Osc52ReadPolicy) -> Arc<ClipboardStaging> {
    let staging = ClipboardStaging::with_budget(1 << 20, 1 << 20);
    staging.set_read_policy(policy);
    staging
}

/// Admit one read of `kinds` from pane 7 at the identity's current command epoch.
fn admit(staging: &Arc<ClipboardStaging>, identity: &Arc<ReadIdentity>, kinds: &[u8]) -> Admission {
    admit_read(staging, identity, 7, identity.command_epoch(), kinds, Instant::now())
}

/// Contract: one read per pane and `MAX_TICKETS` per process hold tickets; a read past either
/// bound is `Busy` before anything is posted (the VT worker answers it empty), and dropping a
/// ticket frees both bounds.
#[test]
fn gate_full_empty_reply_before_queue() {
    let staging = staging(Osc52ReadPolicy::Allow);
    let first = Arc::new(ReadIdentity::default());
    let held = admit(&staging, &first, b"c");
    assert!(matches!(held, Admission::Ticket(_)));
    assert!(matches!(admit(&staging, &first, b"c"), Admission::Busy), "one read per pane");
    let mut others: Vec<Admission> = (1..MAX_TICKETS)
        .map(|_| admit(&staging, &Arc::new(ReadIdentity::default()), b"c"))
        .collect();
    assert!(others.iter().all(|admission| matches!(admission, Admission::Ticket(_))));
    assert_eq!(staging.ticket_slots_in_use(), MAX_TICKETS);
    let ninth = admit(&staging, &Arc::new(ReadIdentity::default()), b"c");
    assert!(matches!(ninth, Admission::Busy), "eight per process");
    others.clear();
    drop(held);
    assert_eq!(staging.ticket_slots_in_use(), 0);
    assert!(matches!(admit(&staging, &first, b"c"), Admission::Ticket(_)), "the pane slot is free");
}

/// Contract: under `deny` a read is silent and takes no slot.
#[test]
fn deny_silent_no_reservation() {
    let staging = staging(Osc52ReadPolicy::Deny);
    let identity = Arc::new(ReadIdentity::default());
    assert!(matches!(admit(&staging, &identity, b"c"), Admission::Silent));
    assert_eq!(staging.ticket_slots_in_use(), 0);
}

/// Contract: `allow` admits an `AutoAllowed` ticket, which has no consent session; `ask` admits a
/// `Waiting` one. Only `AutoAllowed` and `Approved` tickets may start a native read.
#[test]
fn auto_allowed_has_no_session_and_ask_waits() {
    let identity = Arc::new(ReadIdentity::default());
    let Admission::Ticket(allowed) = admit(&staging(Osc52ReadPolicy::Allow), &identity, b"c")
    else {
        panic!("allow admits a ticket");
    };
    assert_eq!(allowed.state(), TicketState::AutoAllowed);
    assert!(allowed.readable());
    drop(allowed);
    let Admission::Ticket(mut asked) = admit(&staging(Osc52ReadPolicy::Ask), &identity, b"c")
    else {
        panic!("ask admits a ticket");
    };
    assert_eq!(asked.state(), TicketState::Waiting);
    assert!(!asked.readable(), "a waiting ticket reads nothing");
    // Only the consent page approves a ticket; this stands in for its Apply.
    asked.state = TicketState::Approved;
    assert!(asked.readable());
}

/// Contract: a ticket binds the pane, PTY incarnation, owner and command epochs and the policy
/// generation at admission; later bumps leave the binding as it was, so revalidation sees them.
#[test]
fn binding_captures_identity_at_admission() {
    let staging = staging(Osc52ReadPolicy::Allow);
    let identity = Arc::new(ReadIdentity::default());
    identity.set_incarnation(11);
    identity.bump_owner();
    identity.set_command_epoch(4);
    let now = Instant::now();
    let Admission::Ticket(ticket) = admit_read(&staging, &identity, 7, 4, b"cp", now) else {
        panic!("a free gate admits a ticket");
    };
    let binding = ticket.binding().clone();
    assert_eq!(
        (binding.pane_id, binding.incarnation, binding.owner_epoch, binding.command_epoch),
        (7, 11, 1, 4)
    );
    assert_eq!(binding.policy_generation, staging.policy_generation());
    assert_eq!(binding.created_at, now);
    assert_eq!(ticket.labels(), b"cp");
    identity.bump_owner();
    identity.set_command_epoch(5);
    assert_eq!(ticket.binding(), &binding, "the binding keeps the admission values");
}

/// Contract: a handle dropped untaken, as when the event loop has closed and the post fails,
/// frees its pane and process slots once its last clone is gone.
#[test]
fn ticket_drop_frees_on_closed_loop() {
    let staging = staging(Osc52ReadPolicy::Allow);
    let identity = Arc::new(ReadIdentity::default());
    let Admission::Ticket(ticket) = admit(&staging, &identity, b"c") else {
        panic!("a free gate admits a ticket");
    };
    let handle = ClipboardTicketHandle::new(ticket);
    let clone = handle.clone();
    assert_eq!(handle, clone);
    drop(handle);
    assert_eq!(staging.ticket_slots_in_use(), 1, "a clone still holds the ticket");
    drop(clone);
    assert_eq!(staging.ticket_slots_in_use(), 0);
    assert!(matches!(admit(&staging, &identity, b"c"), Admission::Ticket(_)));
}

/// Contract: a cancelled ticket records why and can never start a native read.
#[test]
fn cancel_records_reason_and_blocks_reading() {
    let staging = staging(Osc52ReadPolicy::Allow);
    let Admission::Ticket(mut ticket) = admit(&staging, &Arc::new(ReadIdentity::default()), b"c")
    else {
        panic!("a free gate admits a ticket");
    };
    ticket.cancel(CancelReason::CommandChanged);
    assert_eq!(ticket.cancel_reason(), Some(CancelReason::CommandChanged));
    assert!(!ticket.readable());
}
