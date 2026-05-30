//! Issue #296 regression test: the tab thumbnail rendered for an
//! OS-level drag must be a non-empty, well-formed PNG.
//!
//! Pre-#296, `try_os_drag_handoff` passed `Vec::new()` to
//! `OsTabDragBackend::begin_session`, so NSDraggingSession / OLE
//! `DoDragDrop` had no real preview to render. This test pins the
//! invariant that the in-process renderer
//! ([`sonic_app::tab_thumbnail::render_tab_thumbnail_png`]) always
//! returns a buffer that
//!
//! 1. is large enough to plausibly be a PNG (>100 bytes — the minimum
//!    for an 8-byte signature + IHDR + at least one IDAT + IEND);
//! 2. starts with the standard PNG magic-number signature; and
//! 3. survives scale-factor variation (1.0 / Retina 2.0 / oddball
//!    1.5) — the original empty-Vec bug was masked precisely because
//!    no test ever inspected the bytes.

use sonic_app::tab_thumbnail::{
    is_png, render_tab_thumbnail_png, tab_thumbnail_inputs_from_payload, PNG_SIGNATURE,
};

#[test]
fn drag_image_png_nonempty_at_1x() {
    let inputs = tab_thumbnail_inputs_from_payload("zsh — ~/code", 1.0);
    let png = render_tab_thumbnail_png(&inputs);
    assert!(png.len() > 100, "tab thumbnail PNG must be > 100 bytes (was {} bytes)", png.len());
    assert!(is_png(&png), "tab thumbnail bytes lack PNG signature");
    assert_eq!(&png[..PNG_SIGNATURE.len()], &PNG_SIGNATURE);
}

#[test]
fn drag_image_png_nonempty_at_retina() {
    let inputs = tab_thumbnail_inputs_from_payload("vim main.rs", 2.0);
    let png = render_tab_thumbnail_png(&inputs);
    assert!(
        png.len() > 100,
        "Retina (2x) tab thumbnail PNG must be > 100 bytes (was {} bytes)",
        png.len()
    );
    assert!(is_png(&png), "Retina tab thumbnail bytes lack PNG signature");
}

#[test]
fn drag_image_png_nonempty_at_oddball_dpi() {
    // 1.5x is the most common Windows DPI scale; covers the
    // round-to-pixel branch in render_tab_thumbnail_png.
    let inputs = tab_thumbnail_inputs_from_payload("pwsh", 1.5);
    let png = render_tab_thumbnail_png(&inputs);
    assert!(png.len() > 100, "1.5x PNG too small: {} bytes", png.len());
    assert!(is_png(&png));
}

#[test]
fn drag_image_png_empty_title_still_renders() {
    // An empty title must NOT short-circuit to an empty Vec — the OS
    // still needs a chip to drag.
    let inputs = tab_thumbnail_inputs_from_payload("", 1.0);
    let png = render_tab_thumbnail_png(&inputs);
    assert!(png.len() > 100, "empty-title PNG too small: {} bytes", png.len());
    assert!(is_png(&png));
}

#[test]
fn drag_image_png_dimensions_scale_with_dpi() {
    // The PNG IHDR width is at bytes [16..20] big-endian. A 2x render
    // must be wider than a 1x render — pins the DPI multiplier.
    fn width_of(png: &[u8]) -> u32 {
        u32::from_be_bytes([png[16], png[17], png[18], png[19]])
    }
    let one_x = render_tab_thumbnail_png(&tab_thumbnail_inputs_from_payload("t", 1.0));
    let two_x = render_tab_thumbnail_png(&tab_thumbnail_inputs_from_payload("t", 2.0));
    assert!(is_png(&one_x) && is_png(&two_x));
    assert!(
        width_of(&two_x) > width_of(&one_x),
        "2x width {} must exceed 1x width {}",
        width_of(&two_x),
        width_of(&one_x)
    );
}
