//! Frame fallback keeps shaping nonblocking and preserves raster custody.

use super::*;

#[test]
fn a_mid_frame_merge_is_corrected_by_the_frame_that_applies_its_generation() {
    // Frame N measures é's title with notdef's advance, then shapes é again after the worker
    // published it: the real glyph draws while the stored width lags. The completion's one
    // wake finds the generation unapplied, and frame N+1 applies it and remeasures.
    let fixture = gated_stack("mid-frame");
    let mut frames = Frames::new(&fixture.stack);
    assert!(frames.begin(), "frame N applies generation 0");
    let notdef_width = frames.title_width();
    assert_eq!(shaped_e(&fixture.stack).0, 0, "é is notdef before publication");
    open(&fixture.gate);
    wait_for_generation(&fixture.stack, 1);
    let (glyph, advance) = shaped_e(&fixture.stack);
    assert_ne!(glyph, 0, "frame N merges the published handles and shapes the real glyph");
    assert_eq!(frames.title_width(), notdef_width, "the stored width lags within frame N");
    wait_for_wakes(&frames, 1);
    assert_eq!(frames.wakes(), 1, "the completion posts exactly one wake");
    assert!(frames.wake_due(), "the handler sees generation 1 unapplied");
    assert!(frames.begin(), "frame N+1 applies generation 1");
    assert_eq!(frames.applies, 2);
    let real_width = frames.title_width();
    assert_ne!(real_width, notdef_width, "frame N+1 remeasures with the real face");
    assert!((f64::from(real_width) - advance).abs() < 0.5, "widths and glyphs agree");
    assert_eq!(shaped_e(&fixture.stack).0, glyph);
    assert!(!frames.wake_due(), "an applied generation needs no further frame");
}

#[test]
fn a_frame_shaping_while_the_worker_holds_the_lock_keeps_notdef_until_the_next_frame() {
    // The worker pauses while holding `pending_fallback` mid-append, so frame N's shape skips
    // the merge and returns notdef at once; after release the next frame applies and resolves.
    let fixture = gated_stack("lock-busy");
    let (entered, release): (Gate, Gate) = (Arc::default(), Arc::default());
    fixture.stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
        during_append: Some(pause_hook(&entered, &release)),
        before_completion: None,
        ..Default::default()
    });
    let mut frames = Frames::new(&fixture.stack);
    assert!(frames.begin());
    let notdef_width = frames.title_width();
    open(&fixture.gate);
    wait(&entered);
    let started = Instant::now();
    assert_eq!(shaped_e(&fixture.stack).0, 0, "a busy lock skips the merge");
    assert!(started.elapsed() < Duration::from_secs(5), "frame shaping waited on the worker");
    assert_eq!(frames.wakes(), 0, "nothing completes while the worker holds the lock");
    open(&release);
    wait_for_generation(&fixture.stack, 1);
    assert!(frames.wake_due());
    assert!(frames.begin(), "the next frame applies generation 1");
    assert_ne!(frames.title_width(), notdef_width);
    assert_ne!(shaped_e(&fixture.stack).0, 0);
}

#[test]
fn frames_between_the_unlock_and_the_completion_lag_until_the_published_generation() {
    // The worker unlocks, then pauses before completing: frames prepared in that window draw
    // the real glyph with the old width and post no wake. Releasing the completion posts one
    // wake, and the frame that applies its generation remeasures.
    let fixture = gated_stack("paused-completion");
    let (entered, release): (Gate, Gate) = (Arc::default(), Arc::default());
    fixture.stack.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
        during_append: None,
        before_completion: Some(pause_hook(&entered, &release)),
        ..Default::default()
    });
    let mut frames = Frames::new(&fixture.stack);
    assert!(frames.begin(), "frame N applies generation 0");
    let notdef_width = frames.title_width();
    open(&fixture.gate);
    wait(&entered);
    let real_glyph = shaped_e(&fixture.stack).0;
    assert_ne!(real_glyph, 0, "frame N merges handles published before the completion");
    for _ in 0..2 {
        assert!(!frames.begin(), "generation 0 is still current, so nothing is applied");
        assert_eq!(frames.title_width(), notdef_width, "the width lags");
        assert_eq!(shaped_e(&fixture.stack).0, real_glyph, "the real glyph draws");
    }
    assert_eq!(frames.wakes(), 0, "no wake before the completion");
    open(&release);
    wait_for_generation(&fixture.stack, 1);
    wait_for_wakes(&frames, 1);
    assert_eq!(frames.wakes(), 1, "the completion posts exactly one wake");
    assert!(frames.wake_due());
    assert!(frames.begin(), "the next frame applies generation 1");
    assert_ne!(frames.title_width(), notdef_width, "and remeasures");
}

#[test]
fn frame_shaping_and_measuring_return_at_once_while_fallback_is_blocked() {
    // The renderer's frame entry points never wait for discovery: with the locator blocked, é
    // shapes as notdef and measures at once; after publication a later frame shapes the real glyph.
    let fixture = gated_stack("frame");
    let started = Instant::now();
    let shaped = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
    assert_eq!(shaped[0].glyph_pos, 0, "notdef while the locator is blocked");
    assert!(fixture.stack.measure_text_width_for_frame("é").unwrap() > 0.0);
    assert!(started.elapsed() < Duration::from_secs(5), "frame shaping waited");
    open(&fixture.gate);
    wait_for_generation(&fixture.stack, 1);
    let resolved = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
    assert_ne!(resolved[0].glyph_pos, 0, "a later frame shapes the published face");
}

#[test]
fn rasterizing_an_unresolved_glyph_returns_none_without_waiting() {
    // A glyph-0 atlas miss shapes for the frame: while é is unresolved it rasterizes to `None` at
    // once (the atlas caches that as missing), and after publication it rasterizes the real glyph.
    let mut fixture = gated_stack("raster");
    let started = Instant::now();
    assert!(fixture.stack.rasterize(GlyphKey::new('é', false, false)).is_none());
    assert!(started.elapsed() < Duration::from_secs(5), "rasterize waited for fallback");
    open(&fixture.gate);
    wait_for_generation(&fixture.stack, 1);
    let tile = fixture.stack.rasterize(GlyphKey::new('é', false, false)).expect("resolved");
    assert!(tile.width > 0 && tile.height > 0);
}

#[test]
fn a_space_is_an_empty_tile_and_a_malformed_buffer_is_none() {
    // A space is a valid empty glyph, so it rasterizes to an empty tile with no coverage, never
    // `None`; a buffer whose length does not match its size stays `None`.
    let mut fixture = gated_stack("space");
    let space =
        fixture.stack.rasterize(GlyphKey::new(' ', false, false)).expect("a space is valid");
    assert_eq!(space.width * space.height, 0);
    assert!(space.coverage.is_empty());
    let raster = |data: Vec<u8>, width: usize, height: usize| sonicterm_font::RasterizedGlyph {
        data,
        width,
        height,
        bearing_x: sonicterm_font::units::PixelLength::new(1.0),
        bearing_y: sonicterm_font::units::PixelLength::new(2.0),
        has_color: false,
        is_scaled: true,
    };
    let empty = fixture.stack.rasterized_glyph_to_tile(raster(Vec::new(), 0, 0)).unwrap();
    assert_eq!((empty.offset_x, empty.offset_y, empty.advance), (1, -2, 0.0));
    assert!(fixture.stack.rasterized_glyph_to_tile(raster(vec![0; 5], 3, 2)).is_none());
    assert!(fixture.stack.rasterized_glyph_to_tile(raster(vec![0; 4], 0, 0)).is_none());
}

#[test]
fn memoized_face_shapes_a_merged_fallback() {
    // A fallback merge mutates the loaded face in place without moving the face epoch, so a
    // memoized face must be that shared `Rc`, not a copy: after the merge the warm memo
    // shapes the new character with the merged handles.
    let fixture = gated_stack("memo-merge");
    let warm = fixture.stack.font_for_style(false, false).unwrap();
    let before = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
    assert_eq!(before[0].glyph_pos, 0, "notdef while the fallback gate is closed");
    open(&fixture.gate);
    wait_for_generation(&fixture.stack, 1);
    let after = fixture.stack.shape_text_for_frame("é", false, false).unwrap();
    assert_ne!(after[0].glyph_pos, 0, "the memoized face shapes the merged fallback");
    let memoized = fixture.stack.font_for_style(false, false).unwrap();
    assert!(std::rc::Rc::ptr_eq(&warm, &memoized), "the merge kept the same face");
}
