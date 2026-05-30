//! Regression for #366: the custom min/max/close trio must be absent on
//! every platform. The OS-native window controls (macOS traffic lights,
//! Windows native caption buttons) handle min/max/close — Sonic never
//! renders its own trio in the client area.
//!
//! This is asserted by source-grep on the renderer call site
//! (`crates/sonic-shared/src/render/core.rs`), the GPU helper module
//! (`crates/sonic-gpu/src/quad.rs`), and the UI tab-bar view
//! (`crates/sonic-ui/src/tabbar_view.rs`). If any of them re-introduces
//! the trio, the test fails on every platform — there is no `cfg(...)`
//! gate.

#[test]
fn render_core_does_not_paint_caption_buttons() {
    let source = include_str!("../src/render/core.rs");
    assert!(
        !source.contains("paint_caption_buttons"),
        "render/core.rs must not call paint_caption_buttons (#366): \
         the custom min/max/close trio was removed on all platforms"
    );
    assert!(
        !source.contains("caption_button_rects"),
        "render/core.rs must not reference caption_button_rects (#366)"
    );
}

#[test]
fn gpu_quad_does_not_define_paint_caption_buttons() {
    let source = include_str!("../../sonic-gpu/src/quad.rs");
    assert!(
        !source.contains("pub fn paint_caption_buttons"),
        "sonic-gpu/src/quad.rs must not define paint_caption_buttons (#366): \
         hit-detection + rendering of the custom trio is gone on all platforms"
    );
}

#[test]
fn ui_tabbar_view_does_not_export_caption_button_helpers() {
    let source = include_str!("../../sonic-ui/src/tabbar_view.rs");
    assert!(
        !source.contains("caption_button_rects"),
        "sonic-ui::tabbar_view must not export caption_button_rects (#366)"
    );
    assert!(
        !source.contains("CAPTION_BUTTON_WIDTH"),
        "sonic-ui::tabbar_view must not export CAPTION_BUTTON_WIDTH (#366)"
    );
    assert!(
        !source.contains("caption_strip_reserved_width"),
        "sonic-ui::tabbar_view must not export caption_strip_reserved_width (#366)"
    );
}
