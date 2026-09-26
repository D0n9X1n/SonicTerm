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

/// Retired GUI transports belong directly to the process, never to the window or pane whose close they may outlive.
#[test]
fn gui_process_owns_retired_pty_transport() {
    let ledger = Ledger::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let limits = || OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let transport = ledger
        .create_child(ledger.root, OwnerKind::PtyTransport, limits())
        .expect("process-owned retired transport");
    let window = ledger.create_child(ledger.root, OwnerKind::Window, limits()).unwrap();
    let pane = ledger.create_child(window, OwnerKind::AppPane, limits()).unwrap();
    let local = ledger.create_child(pane, OwnerKind::LocalPty, limits()).unwrap();
    for parent in [window, pane, local] {
        assert!(matches!(
            ledger.create_child(parent, OwnerKind::PtyTransport, limits()),
            Err(BudgetError::InvalidOwnerHierarchy { .. })
        ));
    }
    let amount = ResourceAmount { bytes: 0, items: 1 };
    ledger.reserve(transport, ResourceClass::ReaperWork, amount).unwrap();
    assert_eq!(
        ledger.snapshot(ledger.root).unwrap().process_class_items[ResourceClass::ReaperWork],
        1
    );
    assert_eq!(ledger.snapshot(window).unwrap().owner_class_items[ResourceClass::ReaperWork], 0);
    ledger.release(transport, ResourceClass::ReaperWork, amount).unwrap();
    assert_eq!(ledger.snapshot(transport).unwrap().owner_amount, ResourceAmount::default());
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

#[test]
fn a_snapshot_agrees_with_itself_under_concurrent_mutation() {
    // Both process axes are summed from the same class shards, so a reader can
    // compare a total against its class breakdown without observing sampling
    // skew between two different instants.
    use enum_map::enum_map;
    use sonicterm_types::{OwnerKind, OwnerLimits, ProcessKind, ResourceAmount};
    use std::sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Barrier,
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
    let owner_limits = || OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    };
    let window = ledger.create_child(ledger.root, OwnerKind::Window, owner_limits()).unwrap();
    let panes: Vec<_> = (0..3)
        .map(|_| ledger.create_child(window, OwnerKind::AppPane, owner_limits()).unwrap())
        .collect();

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(4));
    let classes =
        [ResourceClass::PtyOutput, ResourceClass::GridVisible, ResourceClass::GlyphRaster];
    let mutators: Vec<_> = (0..3)
        .map(|index| {
            let ledger = ledger.clone();
            let stop = stop.clone();
            let barrier = barrier.clone();
            let pane = panes[index];
            let class = classes[index];
            std::thread::spawn(move || {
                barrier.wait();
                let amount = ResourceAmount { bytes: 4096, items: 1 };
                while !stop.load(AtomicOrdering::Relaxed) {
                    if ledger.reserve(pane, class, amount).is_ok() {
                        let _ = ledger.release(pane, class, amount);
                    }
                }
            })
        })
        .collect();

    barrier.wait();
    for _ in 0..2000 {
        let snapshot = ledger.snapshot(ledger.root).unwrap();
        let class_bytes: usize = snapshot.process_class_bytes.values().sum();
        let class_items: usize = snapshot.process_class_items.values().sum();
        assert_eq!(
            snapshot.process_amount.bytes, class_bytes,
            "process bytes disagreed with its own class breakdown"
        );
        assert_eq!(
            snapshot.process_amount.items, class_items,
            "process items disagreed with its own class breakdown"
        );
        assert_eq!(snapshot.owner_amount, snapshot.process_amount, "root mirrors process totals");
    }
    stop.store(true, AtomicOrdering::Relaxed);
    for mutator in mutators {
        mutator.join().unwrap();
    }

    let settled = ledger.snapshot(ledger.root).unwrap();
    assert_eq!(settled.process_amount, ResourceAmount::default());
    assert_eq!(settled.release_failures, 0);
}
