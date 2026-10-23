use super::*;

/// A record for `chars` with key 7 and fonts `(1, 1)`.
fn record(chars: &[char]) -> SlotMissing {
    SlotMissing { key: 7, fonts: Some((1, 1)), chars: chars.to_vec(), truncated: false }
}

/// A snapshot holding `codepoints`, sorted as the notice keeps them.
fn unresolved(codepoints: &[char], overflow: bool) -> sonicterm_engine::UnresolvedSnapshot {
    let mut codepoints = codepoints.to_vec();
    codepoints.sort_unstable();
    sonicterm_engine::UnresolvedSnapshot { revision: 1, codepoints, overflow }
}

#[test]
fn only_unresolved_codepoints_are_counted_once_each() {
    // Tofu from a pending fallback, a raster failure or an oversize tile is not in the
    // unresolved set and is not counted; a codepoint repeated across rows counts once.
    let rows = [record(&['\u{378}', 'é']), record(&['\u{378}', '\u{379}'])];
    let counted = count_missing(rows.iter(), &unresolved(&['\u{378}', '\u{379}'], false), false);
    assert_eq!(counted.value, MissingGlyphValue::Exact(2));
    assert_eq!(counted.sample, vec!['\u{378}', '\u{379}']);
    let none = count_missing(rows.iter(), &unresolved(&[], false), false);
    assert_eq!(none.value, MissingGlyphValue::Exact(0));
}

#[test]
fn private_use_is_skipped_on_request() {
    // A Nerd Font icon is counted unless private use is ignored.
    let rows = [record(&['\u{E0B0}'])];
    let set = unresolved(&['\u{E0B0}'], false);
    assert_eq!(count_missing(rows.iter(), &set, false).value, MissingGlyphValue::Exact(1));
    assert_eq!(count_missing(rows.iter(), &set, true).value, MissingGlyphValue::Exact(0));
}

#[test]
fn caps_report_a_proven_lower_bound_or_capped() {
    // A truncated slot makes the count a minimum; an overflowed unresolved set cannot prove
    // membership, so any drawn tofu reads capped; with no tofu drawn it is still exactly 0.
    let mut truncated = record(&['\u{378}']);
    truncated.truncated = true;
    let set = unresolved(&['\u{378}'], false);
    assert_eq!(count_missing([truncated].iter(), &set, false).value, MissingGlyphValue::AtLeast(1));
    let overflowed = unresolved(&['\u{378}'], true);
    assert_eq!(
        count_missing([record(&['\u{378}'])].iter(), &overflowed, false).value,
        MissingGlyphValue::Capped
    );
    assert_eq!(
        count_missing(std::iter::empty(), &overflowed, false).value,
        MissingGlyphValue::Exact(0)
    );
    let wide: Vec<char> =
        (0..WINDOW_CAP as u32 + 5).filter_map(|offset| char::from_u32(0x4E00 + offset)).collect();
    let rows: Vec<SlotMissing> = wide.chunks(SLOT_CAP).map(record).collect();
    let counted = count_missing(rows.iter(), &unresolved(&wide, false), false);
    assert_eq!(counted.value, MissingGlyphValue::AtLeast(WINDOW_CAP as u64));
    assert_eq!(counted.sample.len(), SAMPLE_LEN);
}

#[test]
fn a_slot_keeps_at_most_its_cap_of_distinct_characters() {
    // Repeats collapse; past SLOT_CAP distinct characters the row is marked truncated.
    let drawn: Vec<char> =
        (0..SLOT_CAP as u32 + 1).filter_map(|offset| char::from_u32(0x378 + offset)).collect();
    let (chars, truncated) = distinct_capped(&[drawn.as_slice(), drawn.as_slice()].concat());
    assert_eq!((chars.len(), truncated), (SLOT_CAP, true));
    assert_eq!(distinct_capped(&['a', 'a', 'b']), (vec!['a', 'b'], false));
}

#[test]
fn a_partial_frame_keeps_undrawn_slots_and_pruning_follows_the_committed_rows() {
    // A presented frame replaces only the slots it emitted; an empty staged row clears its slot.
    // A slot whose committed key changed (scrolled, cleared, other screen), a released pane and
    // a font apply each drop their records.
    let mut presented = HashMap::new();
    let stage =
        |pane_id, slot, chars: &[char]| StagedMissing { pane_id, slot, record: record(chars) };
    let mut staged =
        vec![stage(1, 0, &['\u{378}']), stage(1, 1, &['\u{379}']), stage(2, 0, &['\u{37A}'])];
    commit_missing(&mut presented, &mut staged);
    assert!(staged.is_empty());
    let mut partial = vec![stage(1, 1, &[])];
    commit_missing(&mut presented, &mut partial);
    assert_eq!(presented.len(), 2, "slot 1 cleared, slot 0 kept");
    prune_missing(&mut presented, |pane_id, _| (pane_id == 1).then_some(7), Some((1, 1)));
    assert_eq!(presented.keys().copied().collect::<Vec<_>>(), vec![(1, 0)], "pane 2 released");
    prune_missing(&mut presented, |_, _| Some(8), Some((1, 1)));
    assert!(presented.is_empty(), "the slot shows another row");
    commit_missing(&mut presented, &mut vec![stage(1, 0, &['\u{378}'])]);
    prune_missing(&mut presented, |_, _| Some(7), Some((1, 2)));
    assert!(presented.is_empty(), "a font apply drops the record");
}
