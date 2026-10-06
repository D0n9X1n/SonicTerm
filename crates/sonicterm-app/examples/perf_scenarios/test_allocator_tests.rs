use super::*;

/// The counter sees an allocation made inside the counted work and nothing made outside it, so a
/// zero count means the work allocated nothing.
#[test]
fn only_allocations_inside_the_counted_work_are_counted() {
    // Built outside the counted work, so its allocation is never counted.
    let mut outside = Vec::with_capacity(64);
    outside.push(1_u8);
    let (inside, count) = allocations_during(|| vec![2_u8; 64]);
    assert_eq!(count, 1);
    let (_, none) = allocations_during(|| outside.len() + inside.len());
    assert_eq!(none, 0);
}

/// A nested call is refused before it touches the state, and the outer call's counting is intact
/// afterwards: the outer call still counts its own allocation and nothing else.
#[test]
fn a_nested_call_is_refused_and_leaves_the_outer_count_intact() {
    let ((nested, inside), count) = allocations_during(|| {
        let nested = std::panic::catch_unwind(|| allocations_during(|| ())).is_err();
        // Counting is still on here, so this allocation is the outer call's.
        (nested, vec![3_u8; 8])
    });
    assert!(nested, "the nested call panics");
    assert!(count >= 1, "the outer call still counted its own allocation");
    assert_eq!(inside.len(), 8);
    assert!(!COUNTING.with(Cell::get), "counting is off once the outer call returns");
}

/// A panic inside the counted work restores the thread's counting state as it was, so later
/// allocations on the thread are not counted and a later counted call starts from zero.
#[test]
fn a_caught_panic_restores_the_previous_counting_state() {
    COUNT.with(|count| count.set(1_000));
    let caught = std::panic::catch_unwind(|| allocations_during(|| panic!("counted work failed")));
    assert!(caught.is_err());
    assert!(!COUNTING.with(Cell::get), "counting is off after the unwind");
    assert_eq!(COUNT.with(Cell::get), 1_000, "the count is the one from before the call");
    let (_, count) = allocations_during(|| ());
    assert_eq!(count, 0);
}

/// The dispatch timeline's rebase, which this test binary alone can count: the allocation-counting
/// example installs its own global allocator, so the counted test lives beside this one.
#[cfg(perf_dispatch_timeline_api)]
mod rebase_tests {
    use std::time::{Duration, Instant};

    use crate::dispatch_timeline::recorders::{
        Buffers, Frozen, InvocationV1, OuterCallbackV1, INVOCATION_CAPACITY, OUTER_CAPACITY,
    };
    use crate::dispatch_timeline::{InvocationKind, OuterKind};

    use super::allocations_during;

    /// A frozen record on `origin` filling both buffers to capacity, each record entering and
    /// returning at its own index in ns.
    fn full_frozen(origin: Instant) -> Frozen {
        let mut buffers = Buffers::new();
        for index in 0..OUTER_CAPACITY as u64 {
            buffers.outer.push(OuterCallbackV1 {
                enter_ns: index,
                return_ns: index,
                id: index as u32,
                kind: OuterKind::UserEvent,
                open: false,
                clipped_at_arm: false,
            });
        }
        for index in 0..INVOCATION_CAPACITY as u64 {
            buffers.invocations.push(InvocationV1 {
                enter_ns: index,
                return_ns: index,
                window: 0,
                id: index as u32,
                parent: 0,
                kind: InvocationKind::UserEvent,
                synthetic: false,
            });
        }
        Frozen { buffers, origin, incomplete: false, overflow: false }
    }

    /// The rebase allocates nothing, at full capacity and with a zero gap alike, and keeps both buffers'
    /// allocations and capacities: the storage stays two vectors totaling 13,312 bytes of payload.
    #[test]
    fn a_rebase_allocates_nothing() {
        let app = Instant::now();
        for gap_ms in [0_u64, 4] {
            let mut frozen = full_frozen(app + Duration::from_millis(gap_ms));
            let pointers = (frozen.buffers.outer.as_ptr(), frozen.buffers.invocations.as_ptr());
            let payload = frozen.buffers.payload_bytes();
            let ((), allocations) = allocations_during(|| frozen.rebase_onto(app));
            assert_eq!(allocations, 0, "gap {gap_ms} ms");
            assert!(!frozen.incomplete);
            assert_eq!(
                (frozen.buffers.outer.as_ptr(), frozen.buffers.invocations.as_ptr()),
                pointers
            );
            assert_eq!(frozen.buffers.payload_bytes(), payload);
            assert_eq!(frozen.buffers.invocations[255].enter_ns, 255 + gap_ms * 1_000_000);
        }
    }
}
