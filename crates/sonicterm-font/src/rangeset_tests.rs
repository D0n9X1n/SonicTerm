use super::*;

fn ranges(set: &RangeSet<u32>) -> Vec<Range<u32>> {
    set.iter().cloned().collect()
}

#[test]
fn free_range_helpers_cover_empty_overlap_subtract_and_union() {
    assert!(range_is_empty(&(4u32..4)));
    assert!(!range_is_empty(&(4u32..5)));
    assert!(intersects_range(&(0..5), &(4..8)));
    assert!(!intersects_range(&(0..5), &(5..8)));
    assert_eq!(range_intersection(&(0..8), &(3..10)), Some(3..8));
    assert_eq!(range_intersection(&(0..3), &(3..5)), None);
    assert_eq!(range_subtract(&(0..10), &(3..7)), (Some(0..3), Some(7..10)));
    assert_eq!(range_subtract(&(0..10), &(20..30)), (Some(0..10), None));
    assert_eq!(range_union(0..0, 2..5), 2..5);
    assert_eq!(range_union(2..6, 5..10), 2..10);
}

#[test]
fn add_ranges_merge_overlap_and_adjacency() {
    let mut set = RangeSet::new();
    set.add_range(10..20);
    set.add_range(0..5);
    set.add_range(5..10);
    set.add_range(18..30);
    assert_eq!(ranges(&set), vec![0..30]);
    assert_eq!(set.len(), 30);
    assert!(set.contains(0));
    assert!(set.contains(29));
    assert!(!set.contains(30));
}

#[test]
fn remove_value_and_range_split_existing_ranges() {
    let mut set = RangeSet::new();
    set.add_range(0..10);
    set.remove(5);
    assert_eq!(ranges(&set), vec![0..5, 6..10]);
    set.remove_range(2..8);
    assert_eq!(ranges(&set), vec![0..2, 8..10]);
}

#[test]
fn set_difference_and_intersections_preserve_expected_members() {
    let mut a = RangeSet::new();
    a.add_range(0..10);
    a.add_range(20..30);
    let mut b = RangeSet::new();
    b.add_range(5..25);

    assert_eq!(ranges(&a.difference(&b)), vec![0..5, 25..30]);
    assert_eq!(ranges(&a.intersection(&b)), vec![5..10, 20..25]);
    assert_eq!(ranges(&a.intersection_with_range(8..22)), vec![8..10, 20..22]);
}

#[test]
fn add_and_remove_sets_apply_every_range() {
    let mut a = RangeSet::new();
    a.add_range(0..5);
    let mut b = RangeSet::new();
    b.add_range(5..10);
    b.add_range(20..25);
    a.add_set(&b);
    assert_eq!(ranges(&a), vec![0..10, 20..25]);
    a.remove_set(&b);
    assert_eq!(ranges(&a), vec![0..5]);
}

#[test]
fn unchecked_ranges_sort_before_use_and_iter_values_expand_members() {
    let mut set = RangeSet::new();
    set.add_range_unchecked(10..12);
    set.add_range_unchecked(2..5);
    set.sort_if_needed();
    assert_eq!(ranges(&set), vec![2..5, 10..12]);
    assert_eq!(set.iter_values().collect::<Vec<_>>(), vec![2, 3, 4, 10, 11]);
}

#[test]
fn empty_ranges_are_noops() {
    let mut set = RangeSet::<u32>::new();
    set.add_range(2..2);
    assert!(set.is_empty());
    set.remove_range(1..1);
    assert!(set.is_empty());
}
