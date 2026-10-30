//! Pins the VT worker's kitty path end to end: Claude Code's chunked virtual PNG is stored for
//! placeholders, its delete removes it, a failed decode is reported, and a query's reply precedes
//! the replies of sequences after it.

use std::sync::Arc;

use base64::Engine;
use sonicterm_render_model::KittyShow;

use super::super::spawn_pane_tests::pane_and_worker_handles;
use super::*;

/// A small real PNG.
fn png() -> Vec<u8> {
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        4,
        2,
        image::Rgba([9, 8, 7, 255]),
    ))
    .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
    .expect("encoding a PNG in memory cannot fail");
    encoded
}

/// Run `bytes` through the production worker path, returning every reply it sent, in order.
fn run(handles: &PaneVtHandles, bytes: &[u8]) -> Vec<u8> {
    let mut sent = Vec::new();
    process_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut SyncLatch::default(),
        super::super::super::media::decode_inline_image,
        |_| {},
        Instant::now,
        |reply| sent.extend(reply),
    );
    sent
}

/// Claude Code's transmit: the PNG chunked at `chunk` base64 characters, quiet, virtual.
fn claude_code_transmit(image_id: u32, chunk: usize) -> Vec<u8> {
    let payload = base64::engine::general_purpose::STANDARD.encode(png());
    let pieces: Vec<&str> =
        payload.as_bytes().chunks(chunk).map(|piece| std::str::from_utf8(piece).unwrap()).collect();
    let mut stream = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        let more = u8::from(index + 1 < pieces.len());
        if index == 0 {
            // When: index is 0, the first chunk carries the whole control section.
            stream.push_str(&format!(
                "\x1b_Ga=T,t=d,f=100,q=2,U=1,i={image_id},c=4,r=2,m={more};{piece}\x1b\\"
            ));
        } else {
            stream.push_str(&format!("\x1b_Gm={more};{piece}\x1b\\"));
        }
    }
    stream.into_bytes()
}

#[test]
fn a_chunked_virtual_png_is_stored_for_placeholders_and_deleted() {
    // The image is kept, tagged virtual with its box, not drawn at the cursor, and a=d,d=I frees it.
    let (pane, handles) = pane_and_worker_handles();
    let replies = run(&handles, &claude_code_transmit(77, 16));
    assert!(replies.is_empty(), "q=2 sends nothing");
    {
        let images = pane.inline_images.lock();
        assert_eq!(images.len(), 1);
        let tag = images[0].kitty.expect("tagged");
        assert_eq!((tag.image_id, tag.show), (77, KittyShow::Virtual { cols: 4, rows: 2 }));
        assert!(!images[0].draws_at_anchor());
        assert_eq!((images[0].width, images[0].height), (4, 2));
    }
    run(&handles, b"\x1b_Ga=d,d=I,i=77,q=2\x1b\\");
    assert!(pane.inline_images.lock().is_empty());
}

#[test]
fn a_query_is_answered_before_a_later_device_attributes_reply() {
    // Claude Code's probe followed by DA: the kitty OK must arrive first.
    let (_pane, handles) = pane_and_worker_handles();
    let replies = run(&handles, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
    let text = String::from_utf8_lossy(&replies);
    let ok = text.find("\x1b_Gi=31;OK\x1b\\").expect("the probe is answered OK");
    let device_attributes = text.find("\x1b[?").expect("DA is answered");
    assert!(ok < device_attributes, "replies out of order: {text:?}");
}

#[test]
fn a_transmit_that_does_not_decode_is_reported() {
    // A loud transmit of bytes that are not PNG stores nothing and replies with an error.
    let (pane, handles) = pane_and_worker_handles();
    let replies = run(&handles, b"\x1b_Ga=T,f=100,i=5;AAAA\x1b\\");
    assert!(String::from_utf8_lossy(&replies).contains("i=5;EBADPNG"));
    assert!(pane.inline_images.lock().is_empty());
}

#[test]
fn a_loud_transmit_is_acknowledged_after_storing() {
    // Without q, a successful transmit replies OK once the image is stored.
    let (pane, handles) = pane_and_worker_handles();
    let payload = base64::engine::general_purpose::STANDARD.encode(png());
    let replies = run(&handles, format!("\x1b_Ga=t,f=100,i=6;{payload}\x1b\\").as_bytes());
    assert_eq!(replies, b"\x1b_Gi=6;OK\x1b\\".to_vec());
    assert_eq!(pane.inline_images.lock().len(), 1);
}

#[test]
fn the_pane_charge_counts_kitty_pixels_once() {
    // Two placements of one image charge the pane for one pixel buffer.
    let (pane, handles) = pane_and_worker_handles();
    run(&handles, &claude_code_transmit(8, 4096));
    run(&handles, b"\x1b_Ga=p,U=1,i=8,p=2,q=2\x1b\\");
    let images = pane.inline_images.lock();
    assert_eq!(images.len(), 2);
    assert!(Arc::ptr_eq(&images[0].bgra, &images[1].bgra), "placements share the buffer");
    let retained = super::super::super::media::retained_inline_media(&images);
    assert_eq!(retained.bytes, 4 * 2 * 4);
}

/// A kitty transmit with a second placement, then an anchored image, on one pane: the pool charge
/// and `measure_pane` both report the pane's two pixel buffers once each, never the shared kitty
/// buffer twice. The idle-pane pass trims the same store and leaves the charge equal to what
/// remains.
#[test]
fn kitty_and_anchored_images_share_one_charge_measure_and_trim() {
    let (pane, handles) = pane_and_worker_handles();
    run(&handles, &claude_code_transmit(8, 4096));
    run(&handles, b"\x1b_Ga=p,U=1,i=8,p=2,q=2\x1b\\");
    let payload = base64::engine::general_purpose::STANDARD.encode(png());
    run(&handles, format!("\x1b_Ga=T,f=100,i=9,q=2;{payload}\x1b\\").as_bytes());
    let pool = Arc::clone(handles.inline_media_charge.lock().pool());
    let expected = 2 * (4 * 2 * 4);
    assert_eq!(pane.inline_images.lock().len(), 3, "two kitty placements and one anchored image");
    assert_eq!(pool.bytes(), expected, "the charge counts each pixel buffer once");
    let measured = super::super::super::retention::measure_pane(&pane).expect("uncontended");
    assert_eq!(measured.inline_media.bytes, expected);
    assert_eq!(measured.inline_media.items, 2);
    let reclaimed = super::super::super::retention::trim_panes_over_pool_ceiling(&pool, [&pane]);
    assert_eq!(reclaimed, 0, "under the ceiling, the idle pass leaves both buffers");
    assert_eq!(
        pool.bytes(),
        super::super::super::media::retained_inline_media(&pane.inline_images.lock()).bytes
    );
}

/// Over the process ceiling, the idle-pane pass trims a pane holding kitty placements and an
/// anchored image as one store: whole pixel buffers go, oldest first, every placement of an
/// evicted kitty image with it, and the charge still equals what remains.
#[test]
fn the_idle_pass_trims_mixed_kitty_and_anchored_media_over_the_ceiling() {
    let pool = crate::app::media::InlineMediaPool::new();
    let pane_on = |pool: &Arc<crate::app::media::InlineMediaPool>| {
        let parser = sonicterm_vt::vt::Parser::new_with_staging_pool(
            sonicterm_grid::grid::Grid::new(80, 24),
            None,
            sonicterm_vt::vt::CaptureStagingPool::new(),
        );
        let pane = PaneState::new_with_media_pool(Arc::new(Mutex::new(parser)), None, pool);
        let handles = PaneVtHandles::from_pane_state(&pane);
        (pane, handles)
    };
    let (pane, handles) = pane_on(&pool);
    run(&handles, &claude_code_transmit(8, 4096));
    run(&handles, b"\x1b_Ga=p,U=1,i=8,p=2,q=2\x1b\\");
    let payload = base64::engine::general_purpose::STANDARD.encode(png());
    run(&handles, format!("\x1b_Ga=T,f=100,i=9,q=2;{payload}\x1b\\").as_bytes());
    // Siblings created early keep the larger share they were admitted under, so filling them and
    // then adding more panes pushes the pool past its ceiling, as the retention tests do.
    let fill = |pane: &PaneState, first_id: u64, count: u64| {
        for id in first_id..first_id + count {
            // Each image is merged and trimmed under the sibling's charge, as its worker would.
            let evicted = {
                let mut images = pane.inline_images.lock();
                images.push(InlineImage {
                    id,
                    row: 0,
                    col: 0,
                    width: 1,
                    height: 1,
                    bgra: Arc::from(vec![0u8; 4 * 1024 * 1024]),
                    kitty: None,
                });
                super::super::super::media::trim_inline_images_charged(
                    &mut images,
                    &pane.inline_media_charge,
                )
            };
            drop(evicted);
        }
    };
    let early: Vec<_> = (0..3).map(|_| pane_on(&pool)).collect();
    for (index, (sibling, _)) in early.iter().enumerate() {
        fill(sibling, 1_000_000 + 1_000 * index as u64, 32);
    }
    let mut crowd = Vec::new();
    while pool.bytes() <= super::super::super::media::MAX_PROCESS_INLINE_MEDIA_BYTES
        && crowd.len() < 64
    {
        let (late, late_handles) = pane_on(&pool);
        fill(&late, 2_000_000 + 1_000 * crowd.len() as u64, 4);
        crowd.push((late, late_handles));
    }
    assert!(pool.bytes() > super::super::super::media::MAX_PROCESS_INLINE_MEDIA_BYTES);
    let before = super::super::super::media::retained_inline_media(&pane.inline_images.lock());
    super::super::super::retention::trim_panes_over_pool_ceiling(&pool, [&pane]);
    let images = pane.inline_images.lock();
    let after = super::super::super::media::retained_inline_media(&images);
    assert!(after.items <= before.items);
    assert_eq!(
        images.last().map(|image| image.kitty.map(|tag| tag.image_id)),
        Some(Some(9)),
        "the newest buffer stays"
    );
    let kitty_eight =
        images.iter().filter(|image| image.kitty.is_some_and(|tag| tag.image_id == 8)).count();
    assert!(kitty_eight == 0 || kitty_eight == 2, "a kitty image's placements stay or go together");
    drop(images);
    assert_eq!(
        handles.inline_media_charge.lock().pool().bytes(),
        pool.bytes(),
        "the pane's charge is part of the pool total it reports"
    );
}
