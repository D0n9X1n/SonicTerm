use super::*;

/// A presented frame of `scene` at atlas `stamp`, 512x512, with these missing characters.
fn frame<'frame>(
    full: bool,
    scene: &'static str,
    stamp: u64,
    terminal: &'frame [char],
    chrome: &'frame [char],
) -> Presented<'frame, &'static str, u64> {
    Presented {
        full,
        scene,
        stamp,
        atlas_dims: (512, 512),
        missing_terminal: terminal,
        missing_chrome: chrome,
    }
}

/// The reading against a retained `scene` and atlas `stamp` at 512x512.
fn read(
    certificate: &Option<Certificate<&'static str, u64>>,
    scene: Option<&'static str>,
    stamp: u64,
) -> CompletenessCheckpoint {
    read_checkpoint(certificate.as_ref(), scene.as_ref(), &stamp, (512, 512))
}

/// Certified counts with a 512x512 atlas.
fn counts(missing_terminal: usize, missing_chrome: usize) -> CompletenessCheckpoint {
    CompletenessCheckpoint::Certified(CompletenessCounts {
        missing_terminal,
        missing_chrome,
        atlas_dims: (512, 512),
    })
}

/// An incomplete `Full` frame stays certified through an unrelated partial presentation of the same
/// scene and atlas, and that partial frame's own missing characters are added, never dropped: its
/// lists replace the renderer's latest ones but cannot erase the `Full` frame's census.
#[test]
fn a_partial_presentation_of_the_same_scene_keeps_the_full_census() {
    let mut certificate = None;
    record_presented(&mut certificate, frame(true, "scene", 1, &['字', '字', ' '], &['T']));
    assert_eq!(read(&certificate, Some("scene"), 1), counts(1, 1));
    record_presented(&mut certificate, frame(false, "scene", 1, &[], &[]));
    assert_eq!(read(&certificate, Some("scene"), 1), counts(1, 1), "the census survives");
    record_presented(&mut certificate, frame(false, "scene", 1, &['😀'], &[]));
    assert_eq!(read(&certificate, Some("scene"), 1), counts(2, 1), "a new tofu is added");
}

/// Units: distinct characters, whitespace excluded, terminal and chrome counted separately even when
/// the same character is missing in both.
#[test]
fn counts_are_distinct_characters_per_surface() {
    let mut certificate = None;
    record_presented(
        &mut certificate,
        frame(true, "scene", 1, &['a', 'a', '\t', 'b'], &['a', ' ']),
    );
    assert_eq!(read(&certificate, Some("scene"), 1), counts(2, 1));
}

/// A viewport, overlay or title-width change is a different scene: the reading is unavailable as
/// `scene changed`, whether the renderer retains the new scene, retains none after a failed attempt,
/// or presented it as a partial frame; the old certificate is not revived when the scene returns.
#[test]
fn a_changed_scene_makes_the_checkpoint_unavailable() {
    let scene_changed = CompletenessCheckpoint::Unavailable(CompletenessUnavailable::SceneChanged);
    let mut certificate = None;
    record_presented(&mut certificate, frame(true, "scene", 1, &[], &[]));
    for retained in [Some("viewport moved"), Some("overlay shown"), Some("title widths"), None] {
        assert_eq!(read(&certificate, retained, 1), scene_changed, "{retained:?}");
    }
    record_presented(&mut certificate, frame(false, "cursor hidden", 1, &['x'], &[]));
    assert_eq!(read(&certificate, Some("cursor hidden"), 1), scene_changed);
    assert_eq!(
        read(&certificate, Some("scene"), 1),
        scene_changed,
        "a frame of another scene drew characters the certificate never saw"
    );
    record_presented(&mut certificate, frame(true, "scene", 1, &[], &[]));
    assert_eq!(
        read(&certificate, Some("scene"), 1),
        counts(0, 0),
        "a new Full frame certifies again"
    );
}

/// After a successful `Full` frame, an attempt that changed the atlas (a failed or retried assembly
/// resets, evicts or grows it) makes the reading `atlas changed`, checked before the scene, and a
/// dimension change alone does too.
#[test]
fn an_atlas_change_makes_the_checkpoint_unavailable() {
    let atlas_changed = CompletenessCheckpoint::Unavailable(CompletenessUnavailable::AtlasChanged);
    let mut certificate = None;
    record_presented(&mut certificate, frame(true, "scene", 1, &[], &[]));
    assert_eq!(read(&certificate, Some("scene"), 2), atlas_changed);
    assert_eq!(read(&certificate, None, 2), atlas_changed, "the atlas is read before the scene");
    assert_eq!(
        read_checkpoint(certificate.as_ref(), Some(&"scene"), &1, (1024, 512)),
        atlas_changed
    );
    record_presented(&mut certificate, frame(false, "scene", 2, &[], &[]));
    assert_eq!(
        read(&certificate, Some("scene"), 1),
        atlas_changed,
        "a partial frame at another atlas"
    );
}

/// With no `Full` frame presented, or only partial frames, there is nothing to certify.
#[test]
fn no_full_frame_means_no_certificate() {
    let none = CompletenessCheckpoint::Unavailable(CompletenessUnavailable::NoCertificate);
    let mut certificate = None;
    assert_eq!(read(&certificate, Some("scene"), 1), none);
    record_presented(&mut certificate, frame(false, "scene", 1, &[], &[]));
    assert_eq!(read(&certificate, Some("scene"), 1), none);
}

/// The reasons read as the spec names them, so the comparison and the docs agree.
#[test]
fn reasons_read_as_named() {
    assert_eq!(CompletenessUnavailable::NoCertificate.reason(), "no certificate");
    assert_eq!(CompletenessUnavailable::SceneChanged.reason(), "scene changed");
    assert_eq!(CompletenessUnavailable::AtlasChanged.reason(), "atlas changed");
}
