//! Regression contracts for the native operations owned by this sibling.

use super::*;
use crate::lib_tests::{
    fallback_configuration, fallback_fixture, frame_glyph, locator, pause, wait_for_generation,
    Latch,
};
use std::sync::atomic::AtomicUsize;

#[test]
fn frame_shaping_returns_notdef_at_once_while_the_locator_is_blocked() {
    // A frame never waits for fallback discovery: with the locator blocked, the character shapes as
    // notdef at once; after the face is published, a later frame merges it and gets the real glyph.
    let gate = Arc::new(Latch::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("blocked", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0, "notdef while the locator is blocked");
    gate.open();
    wait_for_generation(&fixture.configuration, 1);
    assert_ne!(frame_glyph(&font, 'é'), 0, "a later frame merges the published face");
}

#[test]
fn frame_shaping_skips_the_merge_while_the_worker_holds_the_pending_lock() {
    // Paused mid-append the worker holds `pending_fallback`, and a frame shapes notdef at once.
    // Paused after the unlock and before the completion, a frame already merges the real glyph
    // while the generation is still 0, so the completion always follows the unlock.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("contended", locator(&gate, &calls));
    let (in_append, leave_append) = (Arc::new(Latch::default()), Arc::new(Latch::default()));
    let (before_completion, complete) = (Arc::new(Latch::default()), Arc::new(Latch::default()));
    fixture.configuration.set_fallback_worker_hooks_for_test(crate::FallbackWorkerHooks {
        during_append: Some(pause(&in_append, &leave_append)),
        before_completion: Some(pause(&before_completion, &complete)),
        ..Default::default()
    });
    let font = fixture.configuration.default_font().unwrap();
    assert_eq!(frame_glyph(&font, 'é'), 0);
    in_append.wait();
    assert_eq!(frame_glyph(&font, 'é'), 0, "the lock is busy, so the frame skips the merge");
    leave_append.open();
    before_completion.wait();
    assert_ne!(frame_glyph(&font, 'é'), 0, "after the unlock a frame merges");
    assert_eq!(fixture.configuration.fallback_notice().generation(), 0, "no completion yet");
    complete.open();
    wait_for_generation(&fixture.configuration, 1);
}

#[test]
fn blocking_shape_still_retries_after_clear_shape_cache() {
    // The explicit blocking path keeps its meaning: it waits for the fallback, merges it through a
    // `ClearShapeCache` retry and returns the real glyph.
    let gate = Arc::new(Latch::default());
    gate.open();
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("blocking", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    let shaped = font
        .blocking_shape(
            "é",
            Some(crate::Presentation::Text),
            crate::Direction::LeftToRight,
            None,
            None,
        )
        .unwrap();
    assert_ne!(shaped[0].glyph_pos, 0);
}

/// The configured boundary survives a fallback merge: after `insert_fallback_handles` appends a
/// discovered face and rebuilds the shaper, the shaper's configured count is still the configured
/// chain's, not the enlarged handle count, so the appended face keeps WezTerm's unload rule.
#[test]
fn the_configured_boundary_survives_a_fallback_merge() {
    let gate = Arc::new(Latch::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let fixture = fallback_configuration("boundary", locator(&gate, &calls));
    let font = fixture.configuration.default_font().unwrap();
    let configured = font.configured_handle_count;
    assert_eq!(font.shaper.borrow().configured_handle_count(), configured);
    let handles = font.handles.borrow().len();
    let merged = font.insert_fallback_handles(vec![fallback_fixture(&['é'], false)]).unwrap();
    assert!(merged, "the merge appends a discovered face");
    assert_eq!(font.handles.borrow().len(), handles + 1);
    assert_eq!(
        font.shaper.borrow().configured_handle_count(),
        configured,
        "the rebuilt shaper keeps the configured boundary"
    );
    gate.open();
}
