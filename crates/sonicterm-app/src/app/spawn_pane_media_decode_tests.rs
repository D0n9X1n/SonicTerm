//! Pins decode timing through the production worker path: every decode attempt is counted once with
//! its outcome, place and delete count nothing, and a pane without counters records nothing.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use base64::Engine;

use super::super::spawn_pane_tests::{counting_worker_handles, pane_and_worker_handles};
use super::*;
use crate::app::frame_counters::VtFrameStats;
use crate::app::media::MAX_KITTY_PLACEMENTS;

/// A small real PNG, base64-encoded for a kitty payload.
fn png_payload() -> String {
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        3,
        2,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
    .expect("encoding a PNG in memory cannot fail");
    base64::engine::general_purpose::STANDARD.encode(encoded)
}

/// Run `bytes` through the production worker path.
fn run(handles: &PaneVtHandles, bytes: &[u8]) {
    process_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut SyncLatch::default(),
        super::super::super::media::decode_inline_image,
        |_| {},
        Instant::now,
        |_| {},
    );
}

/// The decode counts: attempts, decoded, failed and refused after decode.
fn counts(stats: &VtFrameStats) -> [u64; 4] {
    [
        stats.media.attempts.load(Ordering::Relaxed),
        stats.media.decoded.load(Ordering::Relaxed),
        stats.media.failed.load(Ordering::Relaxed),
        stats.media.refused_after_decode.load(Ordering::Relaxed),
    ]
}

/// A transmit that decodes, a malformed transmit, a query, and a place and delete of the stored
/// image: three attempts (one failed), and place and delete add none.
#[test]
fn every_decode_attempt_is_counted_with_its_outcome() {
    let stats = Arc::new(VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let payload = png_payload();
    run(&handles, format!("\x1b_Ga=t,f=100,i=4,q=2;{payload}\x1b\\").as_bytes());
    assert_eq!(counts(&stats), [1, 1, 0, 0], "a decoded transmit");
    run(&handles, b"\x1b_Ga=t,f=100,i=5,q=2;AAAA\x1b\\");
    assert_eq!(counts(&stats), [2, 1, 1, 0], "a malformed transmit");
    run(&handles, format!("\x1b_Ga=q,f=100,i=6;{payload}\x1b\\").as_bytes());
    assert_eq!(counts(&stats), [3, 2, 1, 0], "a query that decodes");
    run(&handles, b"\x1b_Ga=p,i=4,q=2\x1b\\\x1b_Ga=d,d=I,i=4,q=2\x1b\\");
    assert_eq!(counts(&stats), [3, 2, 1, 0], "place and delete decode nothing");
    assert_eq!(stats.media.duration.snapshot().buckets().counts.iter().sum::<u64>(), 3);
}

/// A transmit the store refuses after decoding counts as refused, not decoded: with the pane's
/// placements at the cap, a displayed transmit of a new image decodes its pixels and is answered
/// `ENOSPC`.
#[test]
fn a_decode_the_store_refuses_counts_as_refused() {
    let stats = Arc::new(VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let payload = png_payload();
    run(&handles, format!("\x1b_Ga=t,f=100,i=4,q=2;{payload}\x1b\\").as_bytes());
    let places = "\x1b_Ga=p,U=1,i=4,q=2\x1b\\".repeat(MAX_KITTY_PLACEMENTS);
    run(&handles, places.as_bytes());
    run(&handles, format!("\x1b_Ga=T,f=100,i=7,q=2;{payload}\x1b\\").as_bytes());
    assert_eq!(counts(&stats), [2, 1, 0, 1], "the second transmit decoded and was refused");
}

/// A pane without counters decodes and stores as before and records nothing anywhere.
#[test]
fn a_pane_without_counters_records_nothing() {
    let (pane, handles) = pane_and_worker_handles();
    run(&handles, format!("\x1b_Ga=t,f=100,i=4,q=2;{}\x1b\\", png_payload()).as_bytes());
    assert_eq!(pane.inline_images.lock().len(), 1, "the image is stored");
    assert!(handles.frame_counters.is_none());
}

/// Every kitty command applied to the store records one `kitty_store_hold` sample, a query (which
/// never takes the store) records none, and a pane without counters records nothing.
#[test]
fn each_kitty_store_command_records_its_hold() {
    let stats = Arc::new(VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let payload = png_payload();
    run(&handles, format!("\x1b_Ga=t,f=100,i=4,q=2;{payload}\x1b\\").as_bytes());
    run(&handles, b"\x1b_Ga=p,i=4,q=2\x1b\\");
    run(&handles, b"\x1b_Ga=d,d=I,i=4,q=2\x1b\\");
    assert_eq!(stats.kitty_store_hold.snapshot().count(), 3, "transmit, place and delete");
    run(&handles, format!("\x1b_Ga=q,f=100,i=9,q=2;{payload}\x1b\\").as_bytes());
    assert_eq!(stats.kitty_store_hold.snapshot().count(), 3, "a query never takes the store");
    let (_quiet, uncounted) = pane_and_worker_handles();
    run(&uncounted, format!("\x1b_Ga=t,f=100,i=4,q=2;{payload}\x1b\\").as_bytes());
    assert_eq!(stats.kitty_store_hold.snapshot().count(), 3);
}
