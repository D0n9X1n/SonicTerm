//! Pins the shared spill budget: reservations never pass capacity, even under racing threads, and
//! a charge refunds what its file no longer holds.

use std::sync::Arc;

use super::{SpillCharge, SpillPool};

/// A reservation that fits is charged; one that would pass capacity is refused and charges
/// nothing; refunds return the pool to empty.
#[test]
fn reservations_stop_at_capacity_and_refunds_return_them() {
    let pool = SpillPool::new(100);
    assert!(pool.try_reserve(60));
    assert!(!pool.try_reserve(41), "61 + 41 would pass 100");
    assert_eq!(pool.charged(), 60);
    assert!(pool.try_reserve(40));
    assert_eq!(pool.charged(), 100);
    pool.refund(100);
    assert_eq!(pool.charged(), 0);
    assert_eq!(pool.peak(), 100);
}

/// A charge grows by what it reserves, shrinks to a file's new length, and refunds the rest when
/// dropped.
#[test]
fn a_charge_shrinks_with_its_file_and_refunds_on_drop() {
    let pool = SpillPool::new(1_000);
    let mut charge = SpillCharge::new(Arc::clone(&pool));
    assert!(charge.grow(300));
    assert!(charge.grow(200));
    assert_eq!(pool.charged(), 500);
    charge.shrink_to(120);
    assert_eq!(pool.charged(), 120, "compaction to 120 B refunds 380 B");
    drop(charge);
    assert_eq!(pool.charged(), 0, "a dropped file refunds everything");
}

/// Eight threads reserving and refunding against one small pool never see it pass capacity or
/// underflow: the peak stays within capacity and the pool ends empty.
#[test]
fn racing_reservations_never_pass_capacity() {
    let pool = SpillPool::new(4_096);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    if pool.try_reserve(512) {
                        pool.refund(512);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(pool.peak() <= pool.capacity(), "peak {} over capacity", pool.peak());
    assert_eq!(pool.charged(), 0);
}
