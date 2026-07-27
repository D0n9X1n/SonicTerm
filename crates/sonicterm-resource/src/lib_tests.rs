use super::*;
use enum_map::enum_map;
use sonicterm_types::{BudgetError, OwnerState, ProcessKind};
use std::sync::{Arc, Barrier};

fn limits(bytes: usize) -> GovernorLimits {
    GovernorLimits {
        process_bytes: bytes,
        class_bytes: enum_map! { _ => bytes },
        class_items: enum_map! { _ => Some(bytes) },
    }
}

fn owner_limits(bytes: usize) -> OwnerLimits {
    OwnerLimits {
        owner_bytes: bytes,
        class_bytes: enum_map! { _ => bytes },
        class_items: enum_map! { _ => Some(bytes) },
    }
}

fn governor(bytes: usize) -> ResourceGovernor {
    ResourceGovernor::new(ProcessKind::Gui, limits(bytes)).unwrap()
}

fn app_pane(governor: &ResourceGovernor, bytes: usize) -> ResourceOwnerId {
    let window = governor
        .create_child(governor.root_owner(), OwnerKind::Window, owner_limits(bytes))
        .unwrap();
    governor.create_child(window, OwnerKind::AppPane, owner_limits(bytes)).unwrap()
}

#[test]
fn invalid_owner_hierarchy_is_rejected() {
    let gui = governor(100);
    assert!(matches!(
        gui.create_child(gui.root_owner(), OwnerKind::AppPane, owner_limits(100)),
        Err(BudgetError::InvalidOwnerHierarchy {
            process: ProcessKind::Gui,
            parent: OwnerKind::Process,
            child: OwnerKind::AppPane,
        })
    ));

    let mux = ResourceGovernor::new(ProcessKind::Mux, limits(100)).unwrap();
    let connection =
        mux.create_child(mux.root_owner(), OwnerKind::MuxConnection, owner_limits(100)).unwrap();
    assert!(matches!(
        mux.create_child(connection, OwnerKind::MuxPane, owner_limits(100)),
        Err(BudgetError::InvalidOwnerHierarchy {
            process: ProcessKind::Mux,
            parent: OwnerKind::MuxConnection,
            child: OwnerKind::MuxPane,
        })
    ));
}

#[test]
fn hierarchy_reservation_drop_and_close_reach_zero() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits(50)).unwrap();
    let reservation = governor
        .try_reserve(pane, ResourceClass::GridVisible, ResourceAmount { bytes: 20, items: 2 })
        .unwrap();
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount.bytes, 20);
    assert_eq!(governor.snapshot(root).unwrap().owner_amount.bytes, 20);
    assert!(matches!(
        governor.finish_close(pane),
        Err(BudgetError::InvalidOwnerState { actual: OwnerState::Open, .. })
    ));
    governor.begin_close(pane).unwrap();
    assert!(matches!(
        governor.try_reserve(pane, ResourceClass::GridHistory, ResourceAmount::default()),
        Err(BudgetError::InvalidOwnerState { actual: OwnerState::Closing, .. })
    ));
    assert!(matches!(governor.finish_close(pane), Err(BudgetError::OwnerHasLiveCharges { .. })));
    drop(reservation);
    governor.finish_close(pane).unwrap();
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn limit_failure_rolls_back_every_level() {
    let governor = governor(10);
    let pane = app_pane(&governor, 8);
    let first = governor
        .try_reserve(pane, ResourceClass::GridVisible, ResourceAmount { bytes: 7, items: 1 })
        .unwrap();
    let before = governor.snapshot(pane).unwrap();
    assert!(matches!(
        governor.try_reserve(
            pane,
            ResourceClass::GridVisible,
            ResourceAmount { bytes: 2, items: 1 }
        ),
        Err(BudgetError::LimitExceeded { .. })
    ));
    let after = governor.snapshot(pane).unwrap();
    assert_eq!(before.owner_amount, after.owner_amount);
    assert_eq!(before.process_amount, after.process_amount);
    drop(first);
}

#[test]
fn commit_split_resize_and_transfer_preserve_exact_accounting() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let reservation = governor
        .try_reserve(source, ResourceClass::GlyphRaster, ResourceAmount { bytes: 40, items: 4 })
        .unwrap();
    let mut committed = reservation.commit(ResourceAmount { bytes: 30, items: 3 }).unwrap();
    let split = committed.split(ResourceAmount { bytes: 10, items: 1 }).unwrap();
    assert_eq!(committed.committed_amount(), ResourceAmount { bytes: 20, items: 2 });
    committed.try_grow(ResourceAmount { bytes: 25, items: 3 }).unwrap();
    committed.shrink(ResourceAmount { bytes: 15, items: 1 }).unwrap();
    let moved = split.transfer(target, ResourceClass::GlyphAtlas).unwrap();
    assert_eq!(governor.snapshot(source).unwrap().owner_amount.bytes, 15);
    assert_eq!(governor.snapshot(target).unwrap().owner_amount.bytes, 10);
    assert_eq!(governor.snapshot(root).unwrap().owner_amount.bytes, 25);
    drop(moved);
    drop(committed);
    assert_eq!(governor.snapshot(root).unwrap().owner_amount, ResourceAmount::default());
}

#[test]
fn failed_commit_and_transfer_return_original_tokens() {
    let governor = governor(20);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(20)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(20)).unwrap();
    let closed = governor.create_child(window, OwnerKind::AppPane, owner_limits(20)).unwrap();
    governor.begin_close(closed).unwrap();
    let reservation = governor
        .try_reserve(source, ResourceClass::PtyInput, ResourceAmount { bytes: 8, items: 1 })
        .unwrap();
    let error = reservation.commit(ResourceAmount { bytes: 9, items: 1 }).unwrap_err();
    assert_eq!(error.reservation.reserved_amount().bytes, 8);
    let error = error.reservation.transfer(closed, ResourceClass::PtyInput).unwrap_err();
    assert_eq!(error.reservation.reserved_amount().bytes, 8);
    assert_eq!(governor.snapshot(source).unwrap().owner_amount.bytes, 8);
    drop(error.reservation);
}

#[test]
fn zero_amount_validates_state_without_mutating_totals() {
    let governor = governor(1);
    let root = governor.root_owner();
    let pane = app_pane(&governor, 1);
    let zero = governor
        .try_reserve(pane, ResourceClass::ProtocolMetadata, ResourceAmount::default())
        .unwrap();
    assert_eq!(governor.snapshot(root).unwrap().process_amount, ResourceAmount::default());
    drop(zero);
    governor.begin_close(pane).unwrap();
    assert!(governor
        .try_reserve(pane, ResourceClass::ProtocolMetadata, ResourceAmount::default())
        .is_err());
}

#[test]
fn owner_id_exhaustion_is_checked() {
    let governor =
        ResourceGovernor::with_next_owner_id(ProcessKind::Gui, limits(10), u64::MAX - 1).unwrap();
    let root = governor.root_owner();
    let id = governor.create_child(root, OwnerKind::Window, owner_limits(10)).unwrap();
    assert_eq!(id.get(), u64::MAX - 1);
    assert_eq!(
        governor.create_child(root, OwnerKind::Window, owner_limits(10)),
        Err(BudgetError::OwnerIdExhausted)
    );
}

#[test]
fn concurrent_cross_class_reservations_never_exceed_process_limit() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let left = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let right = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let first = {
        let governor = governor.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            governor.try_reserve(
                left,
                ResourceClass::GridVisible,
                ResourceAmount { bytes: 60, items: 1 },
            )
        })
    };
    let second = {
        let governor = governor.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            governor.try_reserve(
                right,
                ResourceClass::GlyphAtlas,
                ResourceAmount { bytes: 60, items: 1 },
            )
        })
    };
    barrier.wait();
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.is_ok(), second.is_ok());
    assert!(governor.snapshot(root).unwrap().process_amount.bytes <= 100);
    drop(first);
    drop(second);
    assert_eq!(governor.snapshot(root).unwrap().process_amount, ResourceAmount::default());
}

#[test]
fn concurrent_same_class_siblings_preserve_shared_ancestor_totals() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let left = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let right = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let spawn = |owner| {
        let governor = governor.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            governor.try_reserve(
                owner,
                ResourceClass::GridHistory,
                ResourceAmount { bytes: 40, items: 1 },
            )
        })
    };
    let first = spawn(left);
    let second = spawn(right);
    barrier.wait();
    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert_eq!(governor.snapshot(window).unwrap().owner_amount.bytes, 80);
    assert_eq!(governor.snapshot(root).unwrap().process_amount.bytes, 80);
    drop(first);
    drop(second);
    assert_eq!(governor.snapshot(window).unwrap().owner_amount.bytes, 0);
}

#[test]
fn reserve_and_begin_close_linearize_without_post_close_admission() {
    for _ in 0..128 {
        let governor = governor(10);
        let pane = app_pane(&governor, 10);
        let barrier = Arc::new(Barrier::new(3));
        let reserve = {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                governor.try_reserve(
                    pane,
                    ResourceClass::GridVisible,
                    ResourceAmount { bytes: 1, items: 1 },
                )
            })
        };
        let close = {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                governor.begin_close(pane)
            })
        };
        barrier.wait();
        let reservation = reserve.join().unwrap();
        close.join().unwrap().unwrap();
        assert!(governor
            .try_reserve(pane, ResourceClass::GridVisible, ResourceAmount::default())
            .is_err());
        if let Ok(reservation) = reservation {
            assert!(matches!(
                governor.finish_close(pane),
                Err(BudgetError::OwnerHasLiveCharges { .. })
            ));
            drop(reservation);
        }
        governor.finish_close(pane).unwrap();
    }
}

#[test]
fn zero_and_charged_tokens_commit_on_a_closing_owner() {
    // Closing an owner rejects new reservations; it does not strand tokens that
    // were already admitted, because the close protocol settles live workers
    // before an owner can finish closing.
    let governor = governor(64);
    let pane = app_pane(&governor, 64);
    let zero = governor
        .try_reserve(pane, ResourceClass::ProtocolMetadata, ResourceAmount::default())
        .unwrap();
    let charged = governor
        .try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount { bytes: 32, items: 2 })
        .unwrap();
    governor.begin_close(pane).unwrap();
    let zero = zero.commit(ResourceAmount::default()).expect("zero commit on closing owner");
    let charged = charged
        .commit(ResourceAmount { bytes: 16, items: 1 })
        .unwrap_or_else(|error| panic!("charged commit on closing owner: {:?}", error.error));
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        ResourceAmount { bytes: 16, items: 1 }
    );
    drop(zero);
    drop(charged);
    governor.finish_close(pane).unwrap();
}

#[test]
fn closing_owner_can_transfer_existing_charge_to_open_target() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let reservation = governor
        .try_reserve(source, ResourceClass::PtyOutput, ResourceAmount { bytes: 8, items: 1 })
        .unwrap();
    governor.begin_close(source).unwrap();
    let moved = reservation.transfer(target, ResourceClass::RemoteOutput).unwrap();
    assert_eq!(governor.snapshot(source).unwrap().owner_amount.bytes, 0);
    assert_eq!(governor.snapshot(target).unwrap().owner_amount.bytes, 8);
    governor.finish_close(source).unwrap();
    drop(moved);
}

#[test]
fn transfer_limit_failure_preserves_source_accounting() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(5)).unwrap();
    let reservation = governor
        .try_reserve(source, ResourceClass::PtyOutput, ResourceAmount { bytes: 8, items: 1 })
        .unwrap();
    let error = reservation.transfer(target, ResourceClass::RemoteOutput).unwrap_err();
    assert!(matches!(error.error, BudgetError::LimitExceeded { .. }));
    assert_eq!(governor.snapshot(source).unwrap().owner_amount.bytes, 8);
    assert_eq!(governor.snapshot(target).unwrap().owner_amount.bytes, 0);
    drop(error.reservation);
}

#[test]
fn same_owner_same_class_transfer_is_validation_only() {
    let governor = governor(10);
    let owner = app_pane(&governor, 10);
    let reservation = governor
        .try_reserve(owner, ResourceClass::GridVisible, ResourceAmount { bytes: 3, items: 1 })
        .unwrap();
    let reservation = reservation.transfer(owner, ResourceClass::GridVisible).unwrap();
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 3);
    drop(reservation);
}

#[test]
fn shared_backing_aliases_carry_one_committed_charge() {
    struct Backing {
        _bytes: Arc<[u8]>,
        _charge: CommittedReservation,
    }
    let governor = governor(100);
    let owner = governor.root_owner();
    let charge = governor
        .try_reserve(
            owner,
            ResourceClass::InlineMediaRetained,
            ResourceAmount { bytes: 16, items: 1 },
        )
        .unwrap()
        .commit(ResourceAmount { bytes: 16, items: 1 })
        .unwrap();
    let backing = Arc::new(Backing { _bytes: Arc::from([0u8; 16]), _charge: charge });
    let alias = backing.clone();
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 16);
    drop(backing);
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 16);
    drop(alias);
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 0);
}

#[test]
fn zero_transfer_into_closing_target_is_rejected_without_mutation() {
    let governor = governor(10);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(10)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(10)).unwrap();
    let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(10)).unwrap();
    let zero = governor
        .try_reserve(source, ResourceClass::ProtocolMetadata, ResourceAmount::default())
        .unwrap();
    governor.begin_close(target).unwrap();
    let error = zero.transfer(target, ResourceClass::RegistryMetadata).unwrap_err();
    assert!(matches!(error.error, BudgetError::InvalidOwnerState { .. }));
    assert_eq!(governor.snapshot(root).unwrap().process_amount, ResourceAmount::default());
    drop(error.reservation);
}

#[test]
fn zero_transfer_into_closing_target_ancestor_is_rejected() {
    let governor = governor(10);
    let root = governor.root_owner();
    let source_window = governor.create_child(root, OwnerKind::Window, owner_limits(10)).unwrap();
    let target_window = governor.create_child(root, OwnerKind::Window, owner_limits(10)).unwrap();
    let source =
        governor.create_child(source_window, OwnerKind::AppPane, owner_limits(10)).unwrap();
    let target =
        governor.create_child(target_window, OwnerKind::AppPane, owner_limits(10)).unwrap();
    let zero = governor
        .try_reserve(source, ResourceClass::ProtocolMetadata, ResourceAmount::default())
        .unwrap();
    governor.begin_close(target_window).unwrap();
    let error = zero.transfer(target, ResourceClass::RegistryMetadata).unwrap_err();
    assert!(matches!(error.error, BudgetError::InvalidOwnerState { .. }));
    drop(error.reservation);
}

#[test]
fn zero_and_nonzero_transfer_out_of_closing_source_agree() {
    // The owner close protocol transfers workers out of a `Closing` owner. A
    // borrowed view carries zero bytes and items, so a zero-amount transfer must
    // be admitted on exactly the same terms as a charged one.
    for amount in [ResourceAmount::default(), ResourceAmount { bytes: 8, items: 1 }] {
        let governor = governor(100);
        let root = governor.root_owner();
        let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
        let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
        let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
        let token = governor.try_reserve(source, ResourceClass::PtyOutput, amount).unwrap();
        governor.begin_close(source).unwrap();
        let moved = token
            .transfer(target, ResourceClass::RemoteOutput)
            .unwrap_or_else(|_| panic!("transfer out of closing source rejected for {amount:?}"));
        assert_eq!(governor.snapshot(source).unwrap().owner_amount, ResourceAmount::default());
        assert_eq!(governor.snapshot(target).unwrap().owner_amount, amount);
        governor.finish_close(source).unwrap();
        drop(moved);
        assert_eq!(governor.snapshot(target).unwrap().owner_amount, ResourceAmount::default());
    }
}

#[test]
fn no_item_ceiling_still_counts_items_in_snapshots() {
    let limits = GovernorLimits {
        process_bytes: 10,
        class_bytes: enum_map! { _ => 10 },
        class_items: enum_map! { _ => None },
    };
    let governor = ResourceGovernor::new(ProcessKind::Gui, limits).unwrap();
    let reservation = governor
        .try_reserve(
            governor.root_owner(),
            ResourceClass::CommandEvents,
            ResourceAmount { bytes: 1, items: 9 },
        )
        .unwrap();
    let snapshot = governor.snapshot(governor.root_owner()).unwrap();
    assert_eq!(snapshot.process_amount.items, 9);
    assert_eq!(snapshot.process_class_items[ResourceClass::CommandEvents], 9);
    drop(reservation);
}

#[test]
fn failed_grow_preserves_committed_charge() {
    let governor = governor(10);
    let owner = governor.root_owner();
    let mut committed = governor
        .try_reserve(owner, ResourceClass::Surface, ResourceAmount { bytes: 8, items: 1 })
        .unwrap()
        .commit(ResourceAmount { bytes: 8, items: 1 })
        .unwrap();
    assert!(matches!(
        committed.try_grow(ResourceAmount { bytes: 11, items: 1 }),
        Err(BudgetError::LimitExceeded { .. })
    ));
    assert_eq!(committed.committed_amount(), ResourceAmount { bytes: 8, items: 1 });
    assert_eq!(governor.snapshot(owner).unwrap().owner_amount.bytes, 8);
    drop(committed);
}

#[test]
fn committed_transfer_failure_returns_original_charge() {
    let governor = governor(20);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(20)).unwrap();
    let source = governor.create_child(window, OwnerKind::AppPane, owner_limits(20)).unwrap();
    let target = governor.create_child(window, OwnerKind::AppPane, owner_limits(5)).unwrap();
    let committed = governor
        .try_reserve(source, ResourceClass::SoftwareFrame, ResourceAmount { bytes: 8, items: 1 })
        .unwrap()
        .commit(ResourceAmount { bytes: 8, items: 1 })
        .unwrap();
    let error = committed.transfer(target, ResourceClass::Surface).unwrap_err();
    assert_eq!(error.reservation.committed_amount(), ResourceAmount { bytes: 8, items: 1 });
    assert_eq!(governor.snapshot(source).unwrap().owner_amount.bytes, 8);
    assert_eq!(governor.snapshot(target).unwrap().owner_amount.bytes, 0);
    drop(error.reservation);
}

#[test]
fn concurrent_create_and_close_never_installs_child_after_closing() {
    for _ in 0..64 {
        let governor = governor(10);
        let root = governor.root_owner();
        let parent = governor.create_child(root, OwnerKind::Window, owner_limits(10)).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let create = {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                governor.create_child(parent, OwnerKind::AppPane, owner_limits(10))
            })
        };
        let close = {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                governor.begin_close(parent)
            })
        };
        barrier.wait();
        let child = create.join().unwrap();
        close.join().unwrap().unwrap();
        if let Ok(child) = child {
            assert!(matches!(
                governor.finish_close(parent),
                Err(BudgetError::OwnerHasLiveChildren { .. })
            ));
            governor.begin_close(child).unwrap();
            governor.finish_close(child).unwrap();
        }
        governor.finish_close(parent).unwrap();
    }
}

#[test]
fn token_keeps_ledger_alive_after_governor_drop() {
    let governor = governor(10);
    let reservation = governor
        .try_reserve(
            governor.root_owner(),
            ResourceClass::RegistryMetadata,
            ResourceAmount { bytes: 1, items: 1 },
        )
        .unwrap();
    drop(governor);
    drop(reservation);
}

#[test]
fn consistent_ledger_reports_no_release_failures() {
    let governor = governor(100);
    let pane = app_pane(&governor, 100);
    let reservation = governor
        .try_reserve(pane, ResourceClass::GridHistory, ResourceAmount { bytes: 16, items: 2 })
        .unwrap();
    assert_eq!(governor.snapshot(pane).unwrap().release_failures, 0);
    drop(reservation);
    let settled = governor.snapshot(pane).unwrap();
    assert_eq!(settled.owner_amount, ResourceAmount::default());
    assert_eq!(settled.release_failures, 0);
}

#[test]
fn failed_finish_close_never_leaks_or_double_counts_open_children() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let reservation = governor
        .try_reserve(pane, ResourceClass::GridHistory, ResourceAmount { bytes: 32, items: 1 })
        .unwrap();
    governor.begin_close(pane).unwrap();
    // Each rejection happens after the usage guards are taken but before the parent
    // decrement, so a leak here would strand the parent's open-child count.
    for _ in 0..16 {
        assert!(matches!(
            governor.finish_close(pane),
            Err(BudgetError::OwnerHasLiveCharges { .. })
        ));
    }
    drop(reservation);
    governor.finish_close(pane).unwrap();
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn repeated_finish_close_does_not_double_decrement_parent() {
    let governor = governor(100);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(100)).unwrap();
    let first = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    let second = governor.create_child(window, OwnerKind::AppPane, owner_limits(100)).unwrap();
    governor.begin_close(first).unwrap();
    governor.finish_close(first).unwrap();
    // A closed owner's record is dropped, so closing it again finds nothing
    // rather than finding it in the wrong state. Either way the second call
    // must refuse, which is what stops the parent's child count being
    // decremented twice.
    assert!(
        matches!(governor.finish_close(first), Err(BudgetError::OwnerNotFound(id)) if id == first)
    );
    governor.begin_close(window).unwrap();
    // The second child is still open, so a double decrement would wrongly let the
    // parent close here.
    assert!(matches!(
        governor.finish_close(window),
        Err(BudgetError::OwnerHasLiveChildren { children: 1, .. })
    ));
    governor.begin_close(second).unwrap();
    governor.finish_close(second).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn zero_reserve_succeeds_at_an_exactly_full_limit() {
    let governor = governor(64);
    let pane = app_pane(&governor, 64);
    let full = governor
        .try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount { bytes: 64, items: 1 })
        .unwrap();
    // A zero request mutates nothing, so it must still be admitted while the owner
    // sits exactly at its ceiling.
    let zero =
        governor.try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount::default()).unwrap();
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount.bytes, 64);
    drop(zero);
    drop(full);
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount, ResourceAmount::default());
}

#[test]
fn items_only_reservation_is_accounted_and_released() {
    let governor = governor(64);
    let pane = app_pane(&governor, 64);
    let reservation = governor
        .try_reserve(pane, ResourceClass::ReaperWork, ResourceAmount { bytes: 0, items: 5 })
        .unwrap();
    let held = governor.snapshot(pane).unwrap();
    assert_eq!(held.owner_amount, ResourceAmount { bytes: 0, items: 5 });
    assert_eq!(held.owner_class_items[ResourceClass::ReaperWork], 5);
    drop(reservation);
    assert_eq!(governor.snapshot(pane).unwrap().owner_amount, ResourceAmount::default());
}

#[test]
fn concurrent_swapped_transfers_settle_to_zero() {
    let governor = governor(4096);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(4096)).unwrap();
    let left = governor.create_child(window, OwnerKind::AppPane, owner_limits(4096)).unwrap();
    let right = governor.create_child(window, OwnerKind::AppPane, owner_limits(4096)).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    // Opposing transfer directions acquire the same owner and class guards in
    // opposite logical order; sorted acquisition must keep them from cycling.
    let handles: Vec<_> = [(left, right), (right, left)]
        .into_iter()
        .map(|(source, target)| {
            let governor = governor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..256 {
                    let token = governor.try_reserve(
                        source,
                        ResourceClass::PtyOutput,
                        ResourceAmount { bytes: 8, items: 1 },
                    );
                    if let Ok(token) = token {
                        match token.transfer(target, ResourceClass::RemoteOutput) {
                            Ok(moved) => drop(moved),
                            Err(error) => drop(error.reservation),
                        }
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let settled = governor.snapshot(root).unwrap();
    assert_eq!(settled.process_amount, ResourceAmount::default());
    assert_eq!(settled.release_failures, 0);
    assert_eq!(governor.snapshot(left).unwrap().owner_amount, ResourceAmount::default());
    assert_eq!(governor.snapshot(right).unwrap().owner_amount, ResourceAmount::default());
}

#[test]
fn refused_close_reports_the_true_outstanding_amount() {
    // FM-10: the rejection carries the aggregate the owner actually holds, so a
    // caller can tell whether teardown is progressing rather than only that it
    // is blocked. A payload that reported the last reservation, or a
    // placeholder, would look identical across very different situations.
    let governor = governor(4096);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(4096)).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits(4096)).unwrap();

    let first = governor
        .try_reserve(pane, ResourceClass::GridHistory, ResourceAmount { bytes: 128, items: 2 })
        .unwrap();
    let second = governor
        .try_reserve(pane, ResourceClass::PtyOutput, ResourceAmount { bytes: 64, items: 1 })
        .unwrap();
    governor.begin_close(pane).unwrap();

    // Aggregate across classes, not the most recent reservation.
    let error = governor.finish_close(pane).unwrap_err();
    assert!(
        matches!(error, BudgetError::OwnerHasLiveCharges { owner, amount }
            if owner == pane && amount == ResourceAmount { bytes: 192, items: 3 }),
        "expected the aggregate of both classes, got {error:?}"
    );

    // Releasing part of it moves the reported figure rather than clearing it.
    drop(second);
    let error = governor.finish_close(pane).unwrap_err();
    assert!(
        matches!(error, BudgetError::OwnerHasLiveCharges { amount, .. }
            if amount == ResourceAmount { bytes: 128, items: 2 }),
        "expected the remaining charge after a partial release, got {error:?}"
    );

    drop(first);
    governor.finish_close(pane).unwrap();
    governor.begin_close(window).unwrap();
    governor.finish_close(window).unwrap();
}

#[test]
fn a_parent_close_reports_children_before_charges() {
    // A parent holding both a live child and an inherited charge reports the
    // child first: closing the child is the actionable next step, and the
    // inherited charge disappears with it.
    let governor = governor(4096);
    let root = governor.root_owner();
    let window = governor.create_child(root, OwnerKind::Window, owner_limits(4096)).unwrap();
    let pane = governor.create_child(window, OwnerKind::AppPane, owner_limits(4096)).unwrap();
    let held = governor
        .try_reserve(pane, ResourceClass::GridVisible, ResourceAmount { bytes: 32, items: 1 })
        .unwrap();
    governor.begin_close(window).unwrap();
    assert!(matches!(
        governor.finish_close(window),
        Err(BudgetError::OwnerHasLiveChildren { owner, children: 1 }) if owner == window
    ));
    drop(held);
    governor.begin_close(pane).unwrap();
    governor.finish_close(pane).unwrap();
    governor.finish_close(window).unwrap();
}

/// The call sequence integration has to write, driven end to end.
///
/// Nothing depends on this crate yet — `cargo tree -i -p sonicterm-resource`
/// reports no dependents — so every other test here exercises the governor
/// against values this crate invented. That leaves one thing unverified: that
/// the contract actually fits a caller.
///
/// The subsystems that will call it report a `ResourceAmount` and nothing else
/// (`Grid::retained_amount`, `Parser::retained_amount`,
/// `GlyphAtlas::retained_amount`). This drives exactly that value through the
/// full cycle — reserve, observe, release, re-reserve, grow — so a mismatch
/// between what subsystems report and what the governor accepts fails here
/// rather than during integration.
#[test]
fn a_reported_amount_survives_the_full_reserve_release_cycle() {
    let governor = governor(1_000_000);
    let pane = app_pane(&governor, 1_000_000);

    // Shaped exactly like what a subsystem reports today.
    let reported = ResourceAmount { bytes: 4096, items: 24 };

    let token = governor
        .try_reserve(pane, ResourceClass::GridVisible, reported)
        .expect("a reported amount must be reservable as-is, with no adaptation");
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        reported,
        "the charge must equal what the subsystem reported, not a rounded or derived value"
    );

    // Releasing must return the budget exactly. A subsystem that grows and
    // shrinks repeatedly would otherwise leak its ceiling away over a session,
    // which is the failure mode this architecture exists to prevent.
    drop(token);
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        ResourceAmount { bytes: 0, items: 0 },
        "dropping the token must release the whole charge"
    );

    let reacquired = governor
        .try_reserve(pane, ResourceClass::GridVisible, reported)
        .expect("the same amount must be reservable again after release");

    // Growth is reserved as a delta on top of the live charge, not as a new
    // total — the caller holds both tokens and the ledger sums them.
    let grown = ResourceAmount { bytes: 8192, items: 48 };
    let delta =
        ResourceAmount { bytes: grown.bytes - reported.bytes, items: grown.items - reported.items };
    let growth = governor
        .try_reserve(pane, ResourceClass::GridVisible, delta)
        .expect("a growth delta must be reservable while the original is held");
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        grown,
        "concurrent reservations must sum to the subsystem's new reported total"
    );

    drop(growth);
    drop(reacquired);
    assert_eq!(
        governor.snapshot(pane).unwrap().owner_amount,
        ResourceAmount { bytes: 0, items: 0 },
        "releasing every token must return the owner to zero"
    );
}
