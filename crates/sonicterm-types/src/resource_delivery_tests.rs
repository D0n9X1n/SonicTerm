use super::*;
use crate::ResourceOwnerId;
use std::time::{Duration, Instant};

#[test]
fn retry_token_exposes_deadline_and_wakeup_contract() {
    let deadline = Instant::now() + Duration::from_millis(250);
    for wakeup in [
        RetryWakeup::AtDeadline,
        RetryWakeup::CapacityAvailable,
        RetryWakeup::ConnectionStateChanged,
    ] {
        let token = RetryToken::new(deadline, wakeup);
        assert_eq!(token.deadline(), deadline);
        assert_eq!(token.wakeup(), wakeup);
    }
}

#[test]
fn pressure_outcomes_preserve_caller_owned_values() {
    let deadline = Instant::now();
    let retry = RetryToken::new(deadline, RetryWakeup::CapacityAvailable);
    let outcome = PressureOutcome::Backpressured { value: vec![1, 2, 3], retry };
    match outcome {
        PressureOutcome::Backpressured { value, retry } => {
            assert_eq!(value, vec![1, 2, 3]);
            assert_eq!(retry.deadline(), deadline);
        }
        other => panic!("unexpected pressure outcome: {other:?}"),
    }

    let owner = ResourceOwnerId::new(7).unwrap();
    let rejected = PressureOutcome::Rejected {
        value: String::from("payload"),
        error: BudgetError::OwnerNotFound(owner),
    };
    assert!(matches!(
        rejected,
        PressureOutcome::Rejected { value, error: BudgetError::OwnerNotFound(id) }
            if value == "payload" && id == owner
    ));
}
