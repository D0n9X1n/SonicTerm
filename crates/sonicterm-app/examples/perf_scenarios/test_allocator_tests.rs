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
