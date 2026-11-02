//! Pins the host side of kitty graphics: raw and PNG decoding with their refusals, and the store
//! operations — transmit, place, delete — with kitty's lifecycle rules and per-buffer retention.

use std::sync::Arc;

use base64::Engine;
use sonicterm_vt::vt::MediaProtocol;

use super::*;

/// A kitty event carrying `metadata` and base64 `data`.
fn event(metadata: &str, data: &[u8]) -> MediaEvent {
    MediaEvent {
        scroll_mark: 0,
        protocol: MediaProtocol::Kitty,
        row: 2,
        col: 3,
        metadata: metadata.to_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(data).into_bytes(),
    }
}

/// A decoded image entry with pixel buffer `id` and `bytes` of pixels.
fn pixels(id: u64, bytes: usize) -> InlineImage {
    InlineImage {
        scroll_mark: 0,
        id,
        row: 0,
        col: 0,
        width: 1,
        height: 1,
        bgra: Arc::from(vec![0; bytes]),
        kitty: None,
    }
}

/// The command `metadata` parses to.
fn command(metadata: &str) -> KittyCommand {
    KittyCommand::parse(metadata.as_bytes()).expect("valid command")
}

/// The kitty tags of `images`, in order.
fn tags(images: &[InlineImage]) -> Vec<KittyTag> {
    images.iter().filter_map(|image| image.kitty).collect()
}

#[test]
fn raw_rgb_and_rgba_decode_to_premultiplied_bgra() {
    // f=24 is RGB, f=32 straight-alpha RGBA; both come out as premultiplied BGRA.
    let (width, height, bgra) =
        decode_kitty_pixels(&event("f=24,s=1,v=1", &[10, 20, 30])).expect("rgb");
    assert_eq!((width, height, bgra), (1, 1, vec![30, 20, 10, 255]));
    let (_, _, bgra) =
        decode_kitty_pixels(&event("f=32,s=1,v=1", &[200, 100, 0, 0])).expect("rgba");
    assert_eq!(bgra, vec![0, 0, 0, 0], "zero alpha premultiplies to zero");
}

#[test]
fn raw_data_must_match_its_declared_size() {
    // A short or long buffer, or a side past the decode limit, is refused before allocation.
    assert!(decode_kitty_pixels(&event("f=24,s=2,v=1", &[1, 2, 3])).is_none());
    assert!(decode_kitty_pixels(&event("f=32,s=1,v=1", &[1, 2, 3, 4, 5])).is_none());
    assert!(decode_kitty_pixels(&event("f=24,s=8193,v=1", &[0; 3])).is_none());
}

#[test]
fn a_tall_raw_image_inside_the_pixel_budget_decodes() {
    // A 1x4096 raw strip is far under the pixel budget, so its height alone does not refuse it.
    let data = vec![255u8; 4096 * 4];
    let (width, height, _) =
        decode_kitty_pixels(&event("f=32,s=1,v=4096", &data)).expect("decodes");
    assert_eq!((width, height), (1, 1024));
}

#[test]
fn raw_images_are_downscaled_to_the_rendered_cap() {
    // A 2,048 px raw image is reduced to the 1,024 px side every retained image is held to.
    let side = 2048u32;
    let data = vec![255u8; (side * 4) as usize];
    let (width, height, _) =
        decode_kitty_pixels(&event(&format!("f=32,s={side},v=1"), &data)).expect("decodes");
    assert_eq!((width, height), (1024, 1));
}

#[test]
fn f100_refuses_a_payload_that_is_not_png() {
    // f=100 names PNG; a GIF or truncated PNG does not decode as something else.
    let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
    assert!(decode_kitty_pixels(&event("f=100", gif)).is_none());
    assert!(decode_kitty_pixels(&event("f=100", b"\x89PNG\r\n\x1a\n")).is_none());
}

#[test]
fn an_upload_is_stored_but_not_drawn() {
    // a=t keeps the pixels for a later placement without drawing them anywhere.
    let mut images = Vec::new();
    let (reply, _) =
        apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=5"), pixels(1, 4), None));
    assert_eq!(reply, Some("OK"));
    assert_eq!(tags(&images)[0].show, KittyShow::Upload);
    assert!(!images[0].draws_at_anchor());
}

#[test]
fn retransmitting_an_id_replaces_every_entry_of_it() {
    // A new transmit under the same id drops the old pixels and their placements.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=5"), pixels(1, 4), None));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5,p=2"), 0, 0, 0));
    let (_, removed) =
        apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=5"), pixels(2, 4), None));
    assert_eq!(removed.len(), 2);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].id, 2, "the new pixels have a fresh buffer id");
}

#[test]
fn placing_an_unknown_id_replies_enoent() {
    // a=p for an image never transmitted places nothing.
    let mut images = Vec::new();
    let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,i=9"), 0, 0, 0));
    assert_eq!(reply, Some("ENOENT:no such image"));
    assert!(images.is_empty());
}

#[test]
fn anonymous_placements_accumulate_and_named_ones_replace() {
    // p=0 adds another placement each time; a repeated nonzero p replaces its own placement.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=5"), pixels(1, 4), None));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    assert_eq!(images.len(), 2, "the upload became the first placement; the second was added");
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5,p=7,c=3"), 0, 0, 0));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5,p=7,c=4"), 0, 0, 0));
    let named: Vec<_> = tags(&images).into_iter().filter(|tag| tag.placement_id == 7).collect();
    assert_eq!(
        named,
        vec![KittyTag {
            image_id: 5,
            placement_id: 7,
            show: KittyShow::Virtual { cols: 4, rows: 0 },
            source_px: (1, 1),
        }]
    );
}

#[test]
fn lowercase_delete_keeps_the_data_and_uppercase_frees_it() {
    // d=i removes placements and leaves an upload that can be placed again; d=I removes everything.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=5"), pixels(1, 4), None));
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=i,i=5")));
    assert_eq!(
        tags(&images),
        vec![KittyTag { image_id: 5, placement_id: 0, show: KittyShow::Upload, source_px: (1, 1) }]
    );
    let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    assert_eq!(reply, Some("OK"), "the kept data can be placed again");
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=I,i=5")));
    assert!(images.is_empty());
}

#[test]
fn deleting_one_placement_keeps_the_others_and_their_pixels() {
    // d=I,p=N removes only that placement; the pixels stay while another placement uses them.
    let mut images = Vec::new();
    let _ =
        apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=5,p=1"), pixels(1, 4), None));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5,p=2"), 0, 0, 0));
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=I,i=5,p=1")));
    assert_eq!(tags(&images).iter().map(|tag| tag.placement_id).collect::<Vec<_>>(), vec![2]);
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=i,i=5,p=2")));
    assert_eq!(
        tags(&images)[0].show,
        KittyShow::Upload,
        "d=i on the last placement keeps the data"
    );
}

#[test]
fn unsupported_delete_selectors_change_nothing() {
    // d=a/A depend on what is visible, which this subset does not decide; they are ignored.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=T,i=5"), pixels(1, 4), None));
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=A")));
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d")));
    assert_eq!(images.len(), 1);
}

#[test]
fn untagged_images_are_never_named_by_kitty_commands() {
    // Sixel and iTerm2 images carry no kitty id, so a delete of id 0 or any id leaves them alone.
    let mut images = vec![pixels(1, 4)];
    let _ = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=I,i=0")));
    let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,i=0"), 0, 0, 0));
    assert_eq!(reply, Some("ENOENT:no such image"));
    assert_eq!(images.len(), 1);
}

#[test]
fn placements_sharing_pixels_are_retained_once() {
    // Two placements of one image hold one buffer; retention and eviction count it once.
    let mut images = Vec::new();
    let _ =
        apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=5"), pixels(1, 1000), None));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    images.push(pixels(2, 500));
    let retained = super::super::retained_inline_media(&images);
    assert_eq!((retained.bytes, retained.items), (1500, 2));
    let evicted = super::super::take_trimmed_inline_images(&mut images, 600);
    assert_eq!(evicted.len(), 2, "both placements of the oldest buffer go together");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].id, 2);
}

#[test]
fn an_upload_with_u1_stays_an_upload() {
    // a=t never displays, even with U=1; only a later a=p (or a=T) shows it.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,U=1,i=5"), pixels(1, 4), None));
    assert_eq!(tags(&images)[0].show, KittyShow::Upload);
}

#[test]
fn uppercase_delete_frees_an_unplaced_upload() {
    // d=I on an image that was only uploaded frees its pixels; an upload is not a placement.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=5"), pixels(1, 4), None));
    let (_, removed) = apply_kitty(&mut images, KittyOp::Delete(command("a=d,d=I,i=5")));
    assert!(images.is_empty());
    assert_eq!(removed.len(), 1);
}

#[test]
fn placements_are_capped_per_pane() {
    // Repeated anonymous placements stop at the per-pane cap with ENOSPC instead of growing without bound.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=5"), pixels(1, 4), None));
    for _ in 0..MAX_KITTY_PLACEMENTS {
        let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
        assert_eq!(reply, Some("OK"));
    }
    let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    assert_eq!(reply, Some("ENOSPC:too many placements"));
    assert_eq!(images.len(), MAX_KITTY_PLACEMENTS);
}

#[test]
fn eviction_follows_pixel_age_not_entry_order() {
    // Re-placing an older image moves its entry to the end of the vector, but it is still the
    // older buffer, so trimming evicts it before the newer one.
    let mut images = Vec::new();
    let _ = apply_kitty(
        &mut images,
        KittyOp::Transmit(command("a=T,U=1,i=5,p=1"), pixels(1, 1000), None),
    );
    images.push(pixels(2, 1000));
    let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5,p=1"), 0, 0, 0));
    assert_eq!(
        images.last().map(|image| image.id),
        Some(1),
        "the re-placed entry moved to the end"
    );
    let evicted = super::super::take_trimmed_inline_images(&mut images, 1500);
    assert!(evicted.iter().all(|image| image.id == 1), "the older buffer is evicted");
    assert_eq!(images.iter().map(|image| image.id).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn the_source_size_is_kept_for_natural_sizing() {
    // A transmit records the image's pixel size before downscaling, for placeholder boxes without c/r.
    let mut images = Vec::new();
    let mut image = pixels(1, 4);
    image.width = 1024;
    image.height = 512;
    let command = command("a=T,U=1,i=5");
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command, image, None));
    assert_eq!(tags(&images)[0].source_px, (1024, 512), "defaults to the decoded size");
}

#[test]
fn the_placement_cap_holds_for_transmits_and_placed_uploads() {
    // At the cap, a new displayed transmit and the first placement of an upload are refused too.
    let mut images = Vec::new();
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=5"), pixels(1, 4), None));
    for _ in 0..MAX_KITTY_PLACEMENTS {
        let _ = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=5"), 0, 0, 0));
    }
    let (reply, _) =
        apply_kitty(&mut images, KittyOp::Transmit(command("a=T,U=1,i=6"), pixels(2, 4), None));
    assert_eq!(reply, Some("ENOSPC:too many placements"));
    let _ = apply_kitty(&mut images, KittyOp::Transmit(command("a=t,i=7"), pixels(3, 4), None));
    let (reply, _) = apply_kitty(&mut images, KittyOp::Place(command("a=p,U=1,i=7"), 0, 0, 0));
    assert_eq!(reply, Some("ENOSPC:too many placements"));
    assert!(placement_count(&images) <= MAX_KITTY_PLACEMENTS);
}
