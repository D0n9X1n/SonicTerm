use super::*;
use crate::status_line::StatusLineConfig;
use StatusSegmentId as Seg;

/// The resolved file for a `[status_line]` table written as TOML.
fn file(toml_text: &str) -> ResolvedStatusLine {
    toml::from_str::<StatusLineConfig>(toml_text).unwrap().resolve().unwrap()
}

/// The ids of `resolved`'s `side` list, in order.
fn list(resolved: &ResolvedStatusLine, side: StatusSide) -> Vec<StatusSegmentId> {
    resolved
        .segments
        .iter()
        .filter(|segment| segment.side == side)
        .map(|segment| segment.id)
        .collect()
}

#[test]
fn enabling_appends_to_the_preset_list_or_the_home_list() {
    // A segment the active preset lists goes back to that list; any other goes to its home list.
    let base = file("left = [\"mode\"]\ncenter = []\nright = [\"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Scroll);
    overrides.toggle_segment(&base, Seg::Cpu);
    let shown = overrides.effective(&base);
    assert_eq!(
        list(&shown, StatusSide::Left),
        [Seg::Mode, Seg::Scroll],
        "developer lists scroll left"
    );
    assert_eq!(list(&shown, StatusSide::Right), [Seg::Health, Seg::Cpu], "cpu's home is right");
}

#[test]
fn disabling_removes_a_segment_from_whichever_list_holds_it() {
    // A file segment gets an `absent` override; a session-enabled one simply loses its override.
    let base = file("left = [\"mode\", \"scroll\"]\ncenter = [\"link\"]\nright = []\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Link);
    overrides.toggle_segment(&base, Seg::Cpu);
    overrides.toggle_segment(&base, Seg::Cpu);
    let shown = overrides.effective(&base);
    assert!(shown.segment(Seg::Link).is_none() && shown.segment(Seg::Cpu).is_none());
    assert_eq!(overrides.segment(Seg::Cpu), None, "disabling a session segment leaves no override");
    assert_eq!(overrides.segment(Seg::Link), Some(SegmentOverride::Absent));
}

#[test]
fn a_moved_segment_goes_to_the_end_of_its_new_side() {
    // Right moves right → center → left; the segment follows the file's segments there.
    let base = file("left = [\"mode\"]\ncenter = [\"link\"]\nright = [\"health\", \"mem\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Left);
    let shown = overrides.effective(&base);
    assert_eq!(list(&shown, StatusSide::Center), [Seg::Link, Seg::Mem]);
    assert_eq!(list(&shown, StatusSide::Right), [Seg::Health]);
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Left);
    assert_eq!(list(&overrides.effective(&base), StatusSide::Left), [Seg::Mode, Seg::Mem]);
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Left);
    assert_eq!(
        list(&overrides.effective(&base), StatusSide::Left),
        [Seg::Mode, Seg::Mem],
        "left is the end"
    );
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Right);
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Right);
    // Back on the right it lands last, which is where the file already shows it, so the override
    // changes nothing and is dropped; a move that lands elsewhere in the list keeps one (below).
    assert_eq!(overrides.segment(Seg::Mem), None);
    assert_eq!(overrides.effective(&base), base);
}

#[test]
fn a_segment_moved_back_to_its_file_side_goes_to_the_end_of_that_list() {
    // With `right = ["mem", "health"]`, moving `mem` to the center and back appends it after
    // `health`, and the override keeps a side and an order number across an unrelated reload.
    let base = file("left = []\ncenter = []\nright = [\"mem\", \"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Left);
    overrides.move_segment(&base, Seg::Mem, MoveDirection::Right);
    assert_eq!(list(&overrides.effective(&base), StatusSide::Right), [Seg::Health, Seg::Mem]);
    assert_eq!(
        overrides.segment(Seg::Mem),
        Some(SegmentOverride::Placed { side: StatusSide::Right, order: 1 })
    );
    let unrelated =
        file("left = []\ncenter = []\nright = [\"mem\", \"health\"]\nupdate_interval_ms = 5000\n");
    overrides.reconcile(&base, &unrelated);
    assert_eq!(list(&overrides.effective(&unrelated), StatusSide::Right), [Seg::Health, Seg::Mem]);
}

#[test]
fn a_side_only_file_change_keeps_a_disabled_segment_hidden() {
    // Only a change of its presence may drop an `absent` choice: the file moving a disabled
    // `mode` to the center keeps it hidden and keeps the override.
    let base = file("left = [\"mode\"]\ncenter = []\nright = [\"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Mode);
    let moved = file("left = []\ncenter = [\"mode\"]\nright = [\"health\"]\n");
    overrides.reconcile(&base, &moved);
    assert_eq!(overrides.segment(Seg::Mode), Some(SegmentOverride::Absent));
    assert!(overrides.effective(&moved).segment(Seg::Mode).is_none());
    // A file that stops listing it changed its presence, which drops the choice.
    let dropped = file("left = []\ncenter = []\nright = [\"health\"]\n");
    overrides.reconcile(&moved, &dropped);
    assert_eq!(overrides.segment(Seg::Mode), None);
}

#[test]
fn re_enabling_a_file_segment_returns_it_to_its_file_position() {
    // Recorded decision: enabling a segment the file lists, after the user disabled it, cancels
    // the disable and returns it to its file position; appending applies only to unlisted ones.
    let base = file("left = [\"mode\", \"scroll\"]\ncenter = []\nright = [\"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Mode);
    overrides.toggle_segment(&base, Seg::Mode);
    assert_eq!(overrides.segment(Seg::Mode), None);
    assert_eq!(list(&overrides.effective(&base), StatusSide::Left), [Seg::Mode, Seg::Scroll]);
}

#[test]
fn placed_segments_keep_their_enable_order_across_a_reordering_reload() {
    // Enabling c, a, b shows them in that order after the file's segments, and a reload that
    // only reorders the file keeps every override and their order.
    let base = file("left = [\"mode\", \"scroll\"]\ncenter = []\nright = [\"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    for id in [Seg::Out, Seg::Cpu, Seg::Frame] {
        overrides.toggle_segment(&base, id);
    }
    assert_eq!(
        list(&overrides.effective(&base), StatusSide::Right),
        [Seg::Health, Seg::Out, Seg::Cpu, Seg::Frame]
    );
    let reordered = file("left = [\"scroll\", \"mode\"]\ncenter = []\nright = [\"health\"]\n");
    overrides.reconcile(&base, &reordered);
    let shown = overrides.effective(&reordered);
    assert_eq!(
        list(&shown, StatusSide::Left),
        [Seg::Scroll, Seg::Mode],
        "file order for untouched"
    );
    assert_eq!(list(&shown, StatusSide::Right), [Seg::Health, Seg::Out, Seg::Cpu, Seg::Frame]);
}

#[test]
fn order_numbers_stay_dense_and_bounded_by_the_catalog() {
    // Disabling the middle segment renumbers the rest densely; endless cycles never grow a number.
    let base = file("left = []\ncenter = []\nright = []\n");
    let mut overrides = StatusLineOverrides::default();
    for id in [Seg::Out, Seg::Cpu, Seg::Frame] {
        overrides.toggle_segment(&base, id);
    }
    overrides.toggle_segment(&base, Seg::Cpu);
    assert_eq!(
        overrides.segment(Seg::Out),
        Some(SegmentOverride::Placed { side: StatusSide::Right, order: 1 })
    );
    assert_eq!(
        overrides.segment(Seg::Frame),
        Some(SegmentOverride::Placed { side: StatusSide::Right, order: 2 })
    );
    for _ in 0..500 {
        overrides.toggle_segment(&base, Seg::Cpu);
        overrides.move_segment(&base, Seg::Out, MoveDirection::Left);
        overrides.move_segment(&base, Seg::Out, MoveDirection::Right);
    }
    let catalog = StatusSegmentId::ALL.len() as u16;
    assert!(overrides.orders().all(|order| (1..=catalog).contains(&order)), "{overrides:?}");
}

#[test]
fn reload_drops_only_overrides_whose_own_segment_changed() {
    // Removing a segment the user moved drops only that override; a file that now lists a
    // session-enabled segment satisfies it, which releases its number.
    let base = file("left = [\"mode\"]\ncenter = [\"link\"]\nright = [\"health\"]\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.move_segment(&base, Seg::Link, MoveDirection::Right);
    overrides.toggle_segment(&base, Seg::Cpu);
    overrides.toggle_segment(&base, Seg::Mode);
    let new = file("left = [\"mode\"]\ncenter = []\nright = [\"health\", \"cpu\"]\n");
    overrides.reconcile(&base, &new);
    assert_eq!(overrides.segment(Seg::Link), None, "the file removed the moved segment");
    assert_eq!(overrides.segment(Seg::Cpu), None, "the file now lists it where the user put it");
    assert_eq!(
        overrides.segment(Seg::Mode),
        Some(SegmentOverride::Absent),
        "untouched by the file"
    );
    // A file that moves a segment drops only that segment's side override.
    let mut overrides = StatusLineOverrides::default();
    overrides.move_segment(&base, Seg::Health, MoveDirection::Left);
    overrides.move_segment(&base, Seg::Link, MoveDirection::Left);
    let moved = file("left = [\"mode\", \"health\"]\ncenter = [\"link\"]\nright = []\n");
    overrides.reconcile(&base, &moved);
    assert_eq!(overrides.segment(Seg::Health), None);
    assert!(matches!(
        overrides.segment(Seg::Link),
        Some(SegmentOverride::Placed { side: StatusSide::Left, .. })
    ));
}

#[test]
fn a_preset_change_drops_the_overrides_of_the_segments_it_moved() {
    // Minimal drops `mode` from the left; a `mode` override no longer matches the file's state.
    let developer = file("");
    let minimal = file("preset = \"minimal\"\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&developer, Seg::Mode);
    overrides.move_segment(&developer, Seg::Health, MoveDirection::Left);
    overrides.reconcile(&developer, &minimal);
    assert_eq!(overrides.segment(Seg::Mode), None, "the preset changed mode's presence");
    assert!(overrides.segment(Seg::Health).is_some(), "health stays on the right in both presets");
}

#[test]
fn enabled_and_hide_sensitive_drop_only_on_their_own_change() {
    // A session toggle survives a reload unless the file's own value changed.
    let base = file("");
    let mut overrides = StatusLineOverrides::default();
    overrides.set_enabled(&base, true);
    overrides.set_hide_sensitive(&base, true);
    let other = file("update_interval_ms = 5000\n");
    overrides.reconcile(&base, &other);
    assert!(overrides.effective(&other).enabled && overrides.effective(&other).hide_sensitive);
    let enabled_changed = file("enabled = true\n");
    overrides.reconcile(&other, &enabled_changed);
    assert_eq!(overrides.enabled(), None);
    assert_eq!(overrides.hide_sensitive(), Some(true));
    overrides.set_enabled(&enabled_changed, true);
    assert_eq!(overrides.enabled(), None, "an override equal to the file is not kept");
}

#[test]
fn reset_drops_every_override() {
    // Reset returns to the file's resolved lists exactly.
    let base = file("");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Cpu);
    overrides.set_enabled(&base, true);
    overrides.reset();
    assert_eq!(overrides, StatusLineOverrides::default());
    assert_eq!(overrides.effective(&base), base);
}

#[test]
fn a_session_enabled_segment_keeps_its_configured_keys() {
    // Per-segment keys in the file apply to a segment the file does not list once it is enabled.
    let base = file("right = [\"health\"]\n[segment.cpu]\npriority = 7\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.toggle_segment(&base, Seg::Cpu);
    assert_eq!(
        overrides.effective(&base).segment(Seg::Cpu).map(|segment| segment.priority),
        Some(7)
    );
}

/// The session can move the strip and resize its text; an override equal to the file is not kept,
/// and a reload that changes the file's own value drops it.
#[test]
fn position_and_font_scale_overrides_apply_and_reconcile() {
    use crate::status_line::{StatusFontScale, StatusPosition};
    let base = file("enabled = true\n");
    let mut overrides = StatusLineOverrides::default();
    overrides.set_position(&base, StatusPosition::Bottom);
    overrides.set_font_scale(&base, StatusFontScale::DEFAULT.stepped(1));
    let shown = overrides.effective(&base);
    assert_eq!(shown.position, StatusPosition::Bottom);
    assert_eq!(shown.font_scale, StatusFontScale::DEFAULT.stepped(1));
    assert!(!overrides.is_empty());
    overrides.set_position(&base, StatusPosition::Top);
    overrides.set_font_scale(&base, StatusFontScale::DEFAULT);
    assert!(overrides.is_empty(), "choices equal to the file are not kept");
    overrides.set_font_scale(&base, StatusFontScale::MIN);
    let edited = file("enabled = true\nfont_scale = 0.9\n");
    overrides.reconcile(&base, &edited);
    assert_eq!(overrides.effective(&edited).font_scale, StatusFontScale::from_f64(0.9).unwrap());
    assert!(overrides.is_empty(), "the file's own change wins");
}
