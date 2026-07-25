use super::*;

#[test]
fn limit_candidate_uses_checked_arithmetic() {
    assert_eq!(
        Ledger::validate_limit(
            BudgetScope::Process,
            BudgetDimension::Bytes,
            usize::MAX,
            1,
            usize::MAX
        ),
        Err(BudgetError::Overflow)
    );
}

#[test]
fn class_shards_start_empty() {
    let ledger = Ledger::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: 0,
            class_bytes: enum_map! { _ => 0 },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let snapshot = ledger.snapshot(ledger.root).unwrap();
    assert!(snapshot.process_class_bytes.values().all(|bytes| *bytes == 0));
    assert!(snapshot.process_class_items.values().all(|items| *items == 0));
}
