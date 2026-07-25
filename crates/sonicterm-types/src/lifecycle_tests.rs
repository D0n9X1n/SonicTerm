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

#[test]
fn every_lifecycle_state_maps_to_exactly_one_owner_state() {
    // MM-16: the seam between the lifecycle contract and the ledger's
    // admission contract. Enumerating the full 8x3 space keeps a future
    // lifecycle state from silently defaulting to the wrong admission answer.
    const EXPECTED: [(LifecycleState, OwnerState); 8] = [
        (LifecycleState::Starting, OwnerState::Open),
        (LifecycleState::Running, OwnerState::Open),
        (LifecycleState::Cancelling, OwnerState::Closing),
        (LifecycleState::ClosingTransport, OwnerState::Closing),
        (LifecycleState::Joining, OwnerState::Closing),
        (LifecycleState::Reaped, OwnerState::Closing),
        (LifecycleState::Faulted, OwnerState::Closing),
        (LifecycleState::Closed, OwnerState::Closed),
    ];
    const OWNER_STATES: [OwnerState; 3] =
        [OwnerState::Open, OwnerState::Closing, OwnerState::Closed];

    for state in ALL_STATES {
        let expected = EXPECTED
            .iter()
            .find(|(candidate, _)| *candidate == state)
            .map(|(_, owner)| *owner)
            .expect("every lifecycle state is enumerated");
        assert_eq!(state.owner_state(), expected, "{state}");
        // Exactly one of the three owner states matches, so the mapping is a
        // function rather than an accident of match ordering.
        let matches = OWNER_STATES.iter().filter(|owner| **owner == state.owner_state()).count();
        assert_eq!(matches, 1, "{state} must map to exactly one owner state");
    }
}

#[test]
fn only_running_and_starting_map_to_an_admitting_owner() {
    // A state that stopped admitting work must never report an Open owner:
    // that would let the ledger accept a reservation the transport has already
    // refused.
    for state in ALL_STATES {
        let admits_owner = state.owner_state() == OwnerState::Open;
        let expected = matches!(state, LifecycleState::Starting | LifecycleState::Running);
        assert_eq!(admits_owner, expected, "{state}");
    }
}

#[test]
fn a_faulted_resource_does_not_report_a_settled_owner() {
    // A fault preserves evidence; it does not settle the charges the owner
    // still holds, so it must not present as Closed.
    assert_eq!(LifecycleState::Faulted.owner_state(), OwnerState::Closing);
    assert_ne!(LifecycleState::Faulted.owner_state(), OwnerState::Closed);
    // Only the terminal lifecycle state may report a closed owner.
    for state in ALL_STATES {
        if state.owner_state() == OwnerState::Closed {
            assert_eq!(state, LifecycleState::Closed, "only Closed may report a closed owner");
        }
    }
}
