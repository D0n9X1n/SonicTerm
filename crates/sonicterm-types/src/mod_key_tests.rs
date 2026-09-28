use super::*;

#[test]
fn empty_holds_no_modifiers() {
    let modifiers = ModKey::empty();
    assert!(!modifiers.contains(ModKey::SHIFT));
    assert!(!modifiers.contains(ModKey::CTRL));
    assert!(!modifiers.contains(ModKey::ALT));
    assert!(!modifiers.contains(ModKey::SUPER));
    assert!(modifiers.is_empty());
}

#[test]
fn chord_contains_each_of_its_members_but_not_others() {
    // Ctrl+Shift chord: contains both members, excludes the ones not held.
    let chord = ModKey::CTRL | ModKey::SHIFT;
    assert!(chord.contains(ModKey::CTRL));
    assert!(chord.contains(ModKey::SHIFT));
    assert!(!chord.contains(ModKey::ALT));
    assert!(!chord.contains(ModKey::SUPER));
}

#[test]
fn contains_requires_all_bits_of_a_multi_key_query() {
    // `contains` is subset semantics: a lone Ctrl does not satisfy a
    // Ctrl+Alt query, but the full Ctrl+Alt+Shift superset does.
    let ctrl_alt = ModKey::CTRL | ModKey::ALT;
    assert!(!ModKey::CTRL.contains(ctrl_alt));
    assert!((ModKey::CTRL | ModKey::ALT | ModKey::SHIFT).contains(ctrl_alt));
}

#[test]
fn intersects_is_any_overlap_not_full_subset() {
    let ctrl_shift = ModKey::CTRL | ModKey::SHIFT;
    let shift_alt = ModKey::SHIFT | ModKey::ALT;
    // Overlap on SHIFT ⇒ intersects, even though neither contains the other.
    assert!(ctrl_shift.intersects(shift_alt));
    assert!(!ctrl_shift.contains(shift_alt));
    // No shared bit ⇒ no intersection.
    assert!(!ModKey::CTRL.intersects(ModKey::ALT));
}

#[test]
fn union_merges_chords_and_is_idempotent() {
    let left = ModKey::CTRL | ModKey::SHIFT;
    let right = ModKey::SHIFT | ModKey::SUPER;
    assert_eq!(left | right, ModKey::CTRL | ModKey::SHIFT | ModKey::SUPER);
    // Unioning a chord with itself changes nothing.
    assert_eq!(left | left, left);
}

#[test]
fn intersection_keeps_only_shared_modifiers() {
    let left = ModKey::CTRL | ModKey::SHIFT | ModKey::ALT;
    let right = ModKey::SHIFT | ModKey::ALT | ModKey::SUPER;
    assert_eq!(left & right, ModKey::SHIFT | ModKey::ALT);
}

#[test]
fn difference_removes_the_right_hand_modifiers() {
    let all = ModKey::SHIFT | ModKey::CTRL | ModKey::ALT | ModKey::SUPER;
    // Releasing Ctrl+Alt from an all-held state leaves Shift+Super.
    assert_eq!(all - (ModKey::CTRL | ModKey::ALT), ModKey::SHIFT | ModKey::SUPER);
}

#[test]
fn symmetric_difference_toggles_membership() {
    let held = ModKey::CTRL | ModKey::SHIFT;
    // XOR toggles: SHIFT (shared) drops, ALT (new) is added, CTRL stays.
    let toggled = held ^ (ModKey::SHIFT | ModKey::ALT);
    assert_eq!(toggled, ModKey::CTRL | ModKey::ALT);
}

#[test]
fn complement_flips_all_four_known_modifiers() {
    // The bitflags complement is masked to defined bits, so !empty is the
    // full four-key set and round-trips back under a second complement.
    let none = ModKey::empty();
    let all = ModKey::SHIFT | ModKey::CTRL | ModKey::ALT | ModKey::SUPER;
    assert_eq!(!none, all);
    assert_eq!(!all, none);
}

#[test]
fn insert_and_remove_mutate_the_held_set() {
    let mut modifiers = ModKey::empty();
    modifiers.insert(ModKey::CTRL);
    modifiers.insert(ModKey::SHIFT);
    assert_eq!(modifiers, ModKey::CTRL | ModKey::SHIFT);
    modifiers.remove(ModKey::CTRL);
    assert_eq!(modifiers, ModKey::SHIFT);
    // Removing a modifier that is not held is a no-op.
    modifiers.remove(ModKey::SUPER);
    assert_eq!(modifiers, ModKey::SHIFT);
}
