use super::*;

const ALL_STATES: [LifecycleState; 8] = [
    LifecycleState::Starting,
    LifecycleState::Running,
    LifecycleState::Cancelling,
    LifecycleState::ClosingTransport,
    LifecycleState::Joining,
    LifecycleState::Reaped,
    LifecycleState::Closed,
    LifecycleState::Faulted,
];

/// Every edge the contract permits, as a finite enumerated table.
const LEGAL_EDGES: [(LifecycleState, LifecycleState); 13] = [
    (LifecycleState::Starting, LifecycleState::Running),
    (LifecycleState::Starting, LifecycleState::Cancelling),
    (LifecycleState::Starting, LifecycleState::Faulted),
    (LifecycleState::Running, LifecycleState::Cancelling),
    (LifecycleState::Running, LifecycleState::Faulted),
    (LifecycleState::Cancelling, LifecycleState::ClosingTransport),
    (LifecycleState::Cancelling, LifecycleState::Faulted),
    (LifecycleState::ClosingTransport, LifecycleState::Joining),
    (LifecycleState::ClosingTransport, LifecycleState::Faulted),
    (LifecycleState::Joining, LifecycleState::Reaped),
    (LifecycleState::Joining, LifecycleState::Faulted),
    (LifecycleState::Reaped, LifecycleState::Closed),
    (LifecycleState::Faulted, LifecycleState::Cancelling),
];

#[test]
fn transition_table_is_exactly_the_enumerated_edges() {
    for from in ALL_STATES {
        for to in ALL_STATES {
            let expected = LEGAL_EDGES.contains(&(from, to))
                || (from == LifecycleState::Faulted && to == LifecycleState::Reaped);
            assert_eq!(
                from.can_transition_to(to),
                expected,
                "edge {from} -> {to} disagrees with the enumerated table"
            );
        }
    }
}

#[test]
fn faulted_never_closes_without_settling() {
    // A dirty fault must not shortcut cleanup. It either resumes normal teardown
    // or proves the same preconditions through Reaped.
    assert!(!LifecycleState::Faulted.can_transition_to(LifecycleState::Closed));
    assert!(LifecycleState::Faulted.can_transition_to(LifecycleState::Cancelling));
    assert!(LifecycleState::Faulted.can_transition_to(LifecycleState::Reaped));
    assert!(LifecycleState::Reaped.can_transition_to(LifecycleState::Closed));
}

#[test]
fn no_state_returns_to_running() {
    for from in ALL_STATES {
        if from == LifecycleState::Starting {
            continue;
        }
        assert!(!from.can_transition_to(LifecycleState::Running), "{from} must not resume running");
    }
}

#[test]
fn closed_is_terminal_and_admits_nothing() {
    for to in ALL_STATES {
        assert!(!LifecycleState::Closed.can_transition_to(to), "closed must not leave for {to}");
    }
    assert!(LifecycleState::Closed.is_terminal());
    assert!(!LifecycleState::Closed.admits_work());
}

#[test]
fn only_running_admits_new_work() {
    for state in ALL_STATES {
        assert_eq!(state.admits_work(), state == LifecycleState::Running, "{state}");
    }
}

#[test]
fn no_state_transitions_to_itself() {
    for state in ALL_STATES {
        assert!(!state.can_transition_to(state), "{state} must not self-transition");
    }
}

#[test]
fn every_state_except_closed_can_reach_closed() {
    // Breadth-first over the legal edges: a resource must never strand.
    for start in ALL_STATES {
        let mut seen = vec![start];
        let mut queue = vec![start];
        while let Some(current) = queue.pop() {
            for next in ALL_STATES {
                if current.can_transition_to(next) && !seen.contains(&next) {
                    seen.push(next);
                    queue.push(next);
                }
            }
        }
        assert!(seen.contains(&LifecycleState::Closed), "{start} cannot reach a settled close");
    }
}

#[test]
fn every_state_names_its_entry_requirement() {
    for state in ALL_STATES {
        assert!(!state.entry_requirement().is_empty(), "{state} has no entry requirement");
    }
}

#[test]
fn illegal_transition_reports_owner_and_both_states() {
    let owner = ResourceOwnerId::new(7).unwrap();
    let rejected =
        IllegalTransition { owner, from: LifecycleState::Faulted, to: LifecycleState::Closed };
    let text = rejected.to_string();
    assert!(text.contains('7'), "{text}");
    assert!(text.contains("faulted"), "{text}");
    assert!(text.contains("closed"), "{text}");
}

#[test]
fn timeout_is_an_outcome_not_a_release() {
    assert!(CancelOutcome::Settled.is_settled());
    assert!(!CancelOutcome::TimedOut.is_settled());
    assert!(!CancelOutcome::Pending.is_settled());
}

#[test]
fn only_settled_or_escalated_reaping_releases_a_charge() {
    // An unsettled task keeps its charge so a leak stays visible instead of
    // being forgiven by a terminal status.
    assert!(ReapResult::Settled.releases_charge());
    assert!(ReapResult::Escalated.releases_charge());
    assert!(!ReapResult::TimedOut.releases_charge());
    assert!(!ReapResult::Failed.releases_charge());
}

#[test]
fn only_a_reserved_slot_admits_a_handoff() {
    // Without a reserved slot the caller keeps ownership; it may not enqueue and
    // walk away.
    assert!(ReapAdmission::Reserved.admits());
    assert!(!ReapAdmission::QueueFull.admits());
    assert!(!ReapAdmission::ShuttingDown.admits());
}
