//! Renderer layout contracts, sharing one root fixture owner.

/// Live LCD policy changes invalidate presentation without rebuilding font or atlas state.
#[test]
fn subpixel_aa_setter_does_not_rebuild_fonts_or_atlases() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let start = source.find("pub fn set_subpixel_aa_mode").expect("LCD setter");
    let body = &source[start..source[start..].find("\n    /// Requested LCD").unwrap() + start];

    assert!(body.contains("self.last_frame_key = None"));
    assert!(body.contains("self.request_window_redraw()"));
    for forbidden in ["set_font(", "reset_glyph_atlas", "rebuild_glyph_upload"] {
        assert!(!body.contains(forbidden), "LCD setter unexpectedly calls {forbidden}");
    }
}
