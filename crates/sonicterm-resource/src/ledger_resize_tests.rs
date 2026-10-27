use crate::governor_test_support::*;
use crate::*;
use enum_map::enum_map;
use std::sync::Barrier;

#[test]
fn mixed_resize_uses_final_limits_and_settles_every_accounting_level() {
    // Opposing byte/item changes use final totals, not a transient component-wise maximum or a release gap.
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let class = ResourceClass::InlineMediaRetained;
    let initial = ResourceAmount { bytes: 100, items: 1 };
    let mut held = governor.try_reserve(pane, class, initial).unwrap().commit(initial).unwrap();
    for actual in [
        ResourceAmount { bytes: 60, items: 100 },
        ResourceAmount { bytes: 60, items: 100 },
        initial,
        initial,
        ResourceAmount { bytes: 80, items: 50 },
    ] {
        held.try_resize(actual).unwrap();
        assert_eq!(held.committed_amount(), actual);
        for owner in [pane, window, root] {
            let snapshot = governor.snapshot(owner).unwrap();
            assert_eq!(snapshot.owner_amount, actual);
            assert_eq!(snapshot.owner_class_bytes[class], actual.bytes);
            assert_eq!(snapshot.owner_class_items[class], actual.items);
            assert_eq!(snapshot.process_amount, actual);
            assert_eq!(snapshot.release_failures, 0);
        }
    }
    drop(held);
    assert_eq!(governor.snapshot(root).unwrap().process_amount, ResourceAmount::default());
}

#[test]
fn mixed_resize_refusals_leave_tokens_balances_and_epochs_unchanged() {
    // Both budget axes and Closing admission reject without partially shrinking the other axis.
    let governor = governor(100);
    let pane = app_pane(&governor, 100);
    let class = ResourceClass::InlineMediaRetained;
    let initial = ResourceAmount { bytes: 80, items: 10 };
    let mut held = governor.try_reserve(pane, class, initial).unwrap().commit(initial).unwrap();
    for actual in [ResourceAmount { bytes: 101, items: 1 }, ResourceAmount { bytes: 1, items: 101 }]
    {
        let before = governor.snapshot(pane).unwrap();
        assert!(held.try_resize(actual).is_err());
        let after = governor.snapshot(pane).unwrap();
        assert_eq!(held.committed_amount(), initial);
        assert_eq!(after.owner_amount, before.owner_amount);
        assert_eq!(after.process_amount, before.process_amount);
        assert_eq!(after.owner_class_bytes, before.owner_class_bytes);
        assert_eq!(after.owner_class_items, before.owner_class_items);
        assert_eq!(after.class_epochs, before.class_epochs);
        assert_eq!(after.owner_epoch, before.owner_epoch);
    }
    governor.begin_close(pane).unwrap();
    assert!(held.try_resize(ResourceAmount { bytes: 40, items: 11 }).is_err());
    assert_eq!(held.committed_amount(), initial);
    held.try_resize(ResourceAmount { bytes: 40, items: 5 }).unwrap();
    let before = governor.snapshot(pane).unwrap();
    held.try_resize(held.committed_amount()).unwrap();
    assert_eq!(governor.snapshot(pane).unwrap().owner_epoch, before.owner_epoch);
    drop(held);
    governor.finish_close(pane).unwrap();
}

#[test]
fn mixed_resize_process_limit_and_overflow_leave_old_accounting_intact() {
    // Last-step process admission and checked item overflow cannot commit an earlier byte shrink.
    let mut global = limits(1000);
    global.process_bytes = 100;
    let governor = ResourceGovernor::new(ProcessKind::Gui, global).unwrap();
    let pane = app_pane(&governor, 1000);
    let class = ResourceClass::InlineMediaRetained;
    let old = ResourceAmount { bytes: 60, items: 10 };
    let mut held = governor.try_reserve(pane, class, old).unwrap().commit(old).unwrap();
    let peer = governor
        .try_reserve(pane, ResourceClass::GridVisible, ResourceAmount { bytes: 40, items: 1 })
        .unwrap();
    let before = governor.snapshot(pane).unwrap();
    assert!(matches!(
        held.try_resize(ResourceAmount { bytes: 61, items: 1 }),
        Err(BudgetError::LimitExceeded { scope: sonicterm_types::BudgetScope::Process, .. })
    ));
    assert_eq!(held.committed_amount(), old);
    let after = governor.snapshot(pane).unwrap();
    assert_eq!(after.owner_amount, before.owner_amount);
    assert_eq!(after.owner_epoch, before.owner_epoch);
    assert_eq!(after.class_epochs, before.class_epochs);
    drop(peer);
    held.try_resize(ResourceAmount { bytes: 100, items: 1 }).unwrap();
    drop(held);
    assert_eq!(governor.snapshot(pane).unwrap().process_amount, ResourceAmount::default());

    let governor = ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap();
    let pane = governor
        .create_child(
            governor.root_owner(),
            OwnerKind::Window,
            OwnerLimits {
                owner_bytes: usize::MAX,
                class_bytes: enum_map! { _ => usize::MAX },
                class_items: enum_map! { _ => None },
            },
        )
        .unwrap();
    let old = ResourceAmount { bytes: 10, items: usize::MAX - 1 };
    let mut held = governor.try_reserve(pane, class, old).unwrap().commit(old).unwrap();
    let peer = governor.try_reserve(pane, class, ResourceAmount { bytes: 1, items: 1 }).unwrap();
    let before = governor.snapshot(pane).unwrap();
    assert_eq!(
        held.try_resize(ResourceAmount { bytes: 1, items: usize::MAX }),
        Err(BudgetError::Overflow)
    );
    let after = governor.snapshot(pane).unwrap();
    assert_eq!(held.committed_amount(), old);
    assert_eq!(after.owner_amount, before.owner_amount);
    assert_eq!(after.owner_epoch, before.owner_epoch);
    assert_eq!(after.class_epochs, before.class_epochs);
    drop(peer);
    drop(held);
    assert_eq!(governor.snapshot(pane).unwrap().process_amount, ResourceAmount::default());
}

#[test]
fn mixed_resize_concurrent_reserve_transfer_and_drop_settle_to_zero() {
    // Opposite owner paths and concurrent class activity keep the canonical lock order and exact final release.
    let governor = governor(100_000);
    let first = app_pane(&governor, 100_000);
    let second = app_pane(&governor, 100_000);
    let barrier = Arc::new(Barrier::new(5));
    let workers: Vec<_> = (0..4)
        .map(|index| {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let class = if index % 2 == 0 {
                    ResourceClass::InlineMediaRetained
                } else {
                    ResourceClass::PtyOutput
                };
                let mut owner = if index < 2 { first } else { second };
                let initial = ResourceAmount { bytes: 64, items: 1 };
                let mut held =
                    governor.try_reserve(owner, class, initial).unwrap().commit(initial).unwrap();
                barrier.wait();
                for _ in 0..200 {
                    held.try_resize(ResourceAmount { bytes: 32, items: 3 }).unwrap();
                    let transient = governor
                        .try_reserve(owner, class, ResourceAmount { bytes: 8, items: 1 })
                        .unwrap();
                    let next = if owner == first { second } else { first };
                    held = held.transfer(next, class).unwrap();
                    owner = next;
                    held.try_resize(initial).unwrap();
                    drop(transient);
                }
                drop(held);
            })
        })
        .collect();
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    for owner in [first, second, governor.root_owner()] {
        let snapshot = governor.snapshot(owner).unwrap();
        assert_eq!(snapshot.owner_amount, ResourceAmount::default());
        assert_eq!(snapshot.process_amount, ResourceAmount::default());
        assert_eq!(snapshot.release_failures, 0);
    }
}
