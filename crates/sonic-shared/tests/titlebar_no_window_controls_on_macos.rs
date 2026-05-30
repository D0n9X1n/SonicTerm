//! Issue #366: the integrated client-area titlebar must NOT paint a
//! min/max/close trio on macOS — the native AppKit traffic lights in the
//! top-left already cover those actions. Windows uses native Win11 chrome
//! and also does not paint a duplicate trio.
//!
//! Static source-level guard: if a future refactor reintroduces a call to
//! `paint_caption_buttons` from the renderer hot path, this test fails and
//! whoever made the change must justify the regression (and update the
//! per-platform GUI smoke baseline).

#[test]
fn renderer_does_not_paint_caption_button_trio() {
    let source = include_str!("../src/render/core.rs");
    assert!(
        !source.contains("paint_caption_buttons"),
        "render/core.rs must not call paint_caption_buttons — macOS uses \
         native traffic lights and Windows uses native Win11 chrome (issue #366)",
    );
}
