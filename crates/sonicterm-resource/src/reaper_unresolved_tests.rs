use super::*;
use crate::reaper::test_support::*;
use crate::reaper::*;
use crate::{CancelSource, SystemClock, TestClock};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Terminal unresolved storage owns its payload, charge and handle permits without running native destructors at process teardown.
#[test]
fn unresolved_sink_preserves_payload_accounting_and_admission() {
    use crate::ResourceGovernor;
    use sonicterm_types::{
        GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceAmount, ResourceClass,
    };
    struct AccountedPayload {
        dropped: Arc<AtomicUsize>,
        _charge: crate::CommittedReservation,
        _handles: Vec<ReapHandlePermit>,
        _governor: ResourceGovernor,
    }
    // Lifecycle: AccountedPayload records native destruction before its charge and permits release.
    impl Drop for AccountedPayload {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    let governor = ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map::enum_map! { _ => usize::MAX },
            class_items: enum_map::enum_map! { _ => None },
        },
    )
    .unwrap();
    let transport = governor
        .create_child(
            governor.root_owner(),
            OwnerKind::PtyTransport,
            OwnerLimits {
                owner_bytes: usize::MAX,
                class_bytes: enum_map::enum_map! { _ => usize::MAX },
                class_items: enum_map::enum_map! { _ => None },
            },
        )
        .unwrap();
    let amount = ResourceAmount { bytes: 0, items: 1 };
    let charge = governor
        .try_reserve(transport, ResourceClass::ReaperWork, amount)
        .unwrap()
        .commit(amount)
        .unwrap();
    let (slot, handles) = supervisor
        .try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 })
        .unwrap()
        .into_parts();
    drop(slot);
    let dropped = Arc::new(AtomicUsize::new(0));
    sink.retain(
        transport,
        Box::new(AccountedPayload {
            dropped: dropped.clone(),
            _charge: charge,
            _handles: handles,
            _governor: governor.clone(),
        }),
    );
    assert_eq!((sink.entries(), supervisor.live_tasks(), supervisor.live_handles()), (1, 1, 1));
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    assert_eq!(
        supervisor.try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 }).err(),
        Some(ReapAdmission::QueueFull)
    );
    assert_eq!(
        governor.snapshot(transport).unwrap().owner_class_items[ResourceClass::ReaperWork],
        1
    );
    let report = supervisor.shutdown(Instant::now(), &CancelSource::new().token());
    assert!(!report.is_clean());
    assert_eq!((report.live_tasks, report.unresolved_entries, report.live_handles), (1, 1, 1));
    assert!(report.unresolved_owners.contains(&transport));
    drop(sink);
    drop(supervisor);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        0,
        "terminal sink drop must not destroy unresolved native payloads"
    );
    assert_eq!(
        governor.snapshot(transport).unwrap().owner_class_items[ResourceClass::ReaperWork],
        1
    );
}

/// Preservation: terminal task release hands off unresolved custody before returning its task permit, with no admission gap.
#[test]
fn release_retained_transfers_native_payload_to_unresolved_sink() {
    struct RetainOnDropTask {
        sink: UnresolvedSink,
        handles: Option<Vec<ReapHandlePermit>>,
    }
    impl ReapTask for RetainOnDropTask {
        fn owner(&self) -> ResourceOwnerId {
            owner(1)
        }
        fn next_action(&mut self, _now: Instant) -> ReapAction {
            ReapAction::Complete(ReapResult::TimedOut)
        }
        fn on_completion(&mut self, _result: ReapResult) {}
        fn force_cancel(&mut self) -> CancelOutcome {
            CancelOutcome::TimedOut
        }
    }
    // Lifecycle: RetainOnDropTask moves handles into the sink instead of releasing unresolved native custody.
    impl Drop for RetainOnDropTask {
        fn drop(&mut self) {
            self.sink.retain(owner(1), Box::new(self.handles.take().unwrap()));
        }
    }
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(TestClock::new()));
    let sink = supervisor.unresolved_sink();
    let (slot, handles) = supervisor
        .try_reserve_unit(ReapUnitDemand { helpers: 1, handles: 1 })
        .unwrap()
        .into_parts();
    slot.enqueue(Box::new(RetainOnDropTask { sink: sink.clone(), handles: Some(handles) }));
    let cancel = CancelSource::new();
    supervisor.run_until(Instant::now() + Duration::from_secs(1), &cancel.token());
    assert_eq!(supervisor.release_retained(), 1);
    assert_eq!(
        (
            sink.entries(),
            supervisor.live_tasks(),
            supervisor.live_handles(),
            supervisor.retained_tasks()
        ),
        (1, 1, 1, 0)
    );
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    let report = supervisor.shutdown(Instant::now(), &cancel.token());
    assert_eq!(
        report.unresolved_owners,
        vec![owner(1)],
        "task and sink naming the same transport stays deduplicated"
    );
    assert_eq!((report.live_tasks, report.unresolved_entries), (1, 1));
}

/// Slotless cancellation duplicates have independent sink counts until their actual native owner closes them.
#[test]
fn fallback_duplicate_token_keeps_shutdown_unclean_until_closed() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    let duplicate = sink.track_fallback_duplicate();
    let report = supervisor.shutdown(Instant::now(), &CancelSource::new().token());
    assert_eq!(
        (report.live_tasks, report.live_handles, report.open_fallback_duplicates),
        (0, 0, 1)
    );
    assert!(!report.is_clean());
    drop(duplicate);
    assert_eq!(sink.open_fallback_duplicates(), 0);
    assert!(supervisor.shutdown(Instant::now(), &CancelSource::new().token()).is_clean());
}

/// Preservation: sink counters stay observable while its payload lock is held, so admission never nests that lock.
#[test]
fn unresolved_sink_admission_uses_atomic_entry_count() {
    let supervisor =
        ReaperSupervisor::new(ReaperLimits::new(1, 1, 1).unwrap(), Arc::new(SystemClock));
    let sink = supervisor.unresolved_sink();
    sink.retain(owner(1), Box::new(()));
    let payloads = sink.state.payloads.lock();
    assert_eq!(supervisor.live_tasks(), 1);
    assert_eq!(supervisor.try_reserve_slot().err(), Some(ReapAdmission::QueueFull));
    drop(payloads);
}
