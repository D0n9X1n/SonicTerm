//! End-to-end coverage for the IME pipeline: state machine + overlay
//! layout positioning + commit-once contract + cancel-on-Esc.
//!
//! These exercise the public API surface that `app::App` glues together
//! at runtime: `ImeState` for the composition state machine, and
//! `overlays::ImePreeditLayout::compute` for the under-cursor popover
//! geometry. The end-to-end claim is that the same code paths used in
//! production produce the expected bytes-to-PTY and the expected
//! on-screen rectangle for any cursor (row, col).

use sonic_shared::ime::ImeState;
use sonic_shared::overlays::ImePreeditLayout;

/// Match the renderer's geometry math so the test pins the actual
/// production formula (cf. `render.rs` near the IME overlay block):
///
///   cursor_x = padding + col * cell_w
///   cursor_y = top_inset + row * cell_h
fn cursor_px(row: u16, col: u16, cell_w: f32, cell_h: f32, pad: f32, top: f32) -> (f32, f32) {
    (pad + f32::from(col) * cell_w, top + f32::from(row) * cell_h)
}

#[test]
fn preedit_layout_sits_immediately_below_cursor_cell() {
    // Fabricated but realistic: 9px-wide, 18px-tall cells, 6px window
    // padding, 28px top inset (tab bar). Cursor at row=4, col=10.
    let (cell_w, cell_h, pad, top) = (9.0_f32, 18.0_f32, 6.0_f32, 28.0_f32);
    let (cx, cy) = cursor_px(4, 10, cell_w, cell_h, pad, top);
    assert_eq!(cx, 6.0 + 10.0 * 9.0);
    assert_eq!(cy, 28.0 + 4.0 * 18.0);

    let mut s = ImeState::new();
    s.handle_preedit("ni", Some((2, 2)));
    let layout = ImePreeditLayout::compute(&s, cx, cy, cell_w, cell_h, 1280.0, 720.0)
        .expect("non-empty preedit must produce a layout");

    // Background's top edge sits one cell-height below the cursor top,
    // i.e. flush against the cursor cell's BOTTOM edge.
    assert!(
        (layout.bg.y - (cy + cell_h)).abs() < f32::EPSILON,
        "expected bg.y={} to equal cy+cell_h={}",
        layout.bg.y,
        cy + cell_h
    );
    // Background's left edge anchors to the cursor's left edge unless
    // it would overflow the right margin (not the case here).
    assert!((layout.bg.x - cx).abs() < f32::EPSILON);
    // Underline sits inside the bg, at the bottom.
    assert!(layout.underline.y > layout.bg.y);
    assert!(layout.underline.y + layout.underline.h <= layout.bg.y + layout.bg.h + f32::EPSILON);
}

#[test]
fn preedit_layout_clamps_to_window_width_at_right_edge() {
    let (cell_w, cell_h) = (9.0_f32, 18.0_f32);
    let window_w = 200.0_f32;
    // Cursor near the right edge with a long preedit -> bg must reflow
    // leftward to stay on screen.
    let cx = 190.0;
    let cy = 50.0;
    let mut s = ImeState::new();
    s.handle_preedit("nihaoshijie", Some((11, 11)));
    let layout = ImePreeditLayout::compute(&s, cx, cy, cell_w, cell_h, window_w, 720.0)
        .expect("non-empty preedit must produce a layout");
    assert!(layout.bg.x + layout.bg.w <= window_w + f32::EPSILON);
}

#[test]
fn commit_yields_bytes_once_and_releases_composing_guard() {
    // The app.rs path: on Ime::Commit it calls handle_commit then
    // immediately take_commits and forwards the bytes to the PTY. A
    // second drain must be empty (no double-type) and is_composing()
    // must be false so the KeyboardInput arm stops swallowing keys.
    let mut s = ImeState::new();
    s.handle_enabled();
    s.handle_preedit("ni", Some((2, 2)));
    assert!(s.is_composing());
    s.handle_commit("你好");

    assert!(!s.is_composing(), "composing guard must release after commit");
    let bytes = s.take_commits();
    assert_eq!(bytes, "你好");
    assert_eq!(bytes.as_bytes(), &[0xe4, 0xbd, 0xa0, 0xe5, 0xa5, 0xbd]);
    assert_eq!(s.take_commits(), "", "commit bytes must drain exactly once");
}

#[test]
fn korean_hangul_preedit_cycle_yields_expected_final_bytes() {
    // Hangul composition for 한 (han): preedits ㅎ -> 하 -> 한, then
    // committed as one syllable. We don't simulate the IME's internal
    // jamo transitions — the OS hands us preedit snapshots — but we
    // verify the final committed bytes are exactly UTF-8 for "한국".
    let mut s = ImeState::new();
    s.handle_enabled();
    s.handle_preedit("\u{314E}", Some((1, 1))); // ㅎ
    s.handle_preedit("\u{D558}", Some((1, 1))); // 하
    s.handle_preedit("\u{D55C}", Some((1, 1))); // 한
    assert!(s.is_composing());
    s.handle_commit("한국");
    let out = s.take_commits();
    assert_eq!(out.as_bytes(), "한국".as_bytes());
    // 한 = U+D55C -> ED 95 9C, 국 = U+AD6D -> EA B5 AD
    assert_eq!(out.as_bytes(), &[0xED, 0x95, 0x9C, 0xEA, 0xB5, 0xAD]);
}

#[test]
fn esc_cancels_preedit_without_writing_bytes_to_pty() {
    // The KeyboardInput arm in app.rs: when is_composing() is true and
    // Esc is pressed, it calls ime.cancel() and RETURNS without writing
    // anything to the PTY. Mirror that contract here.
    let mut s = ImeState::new();
    s.handle_enabled();
    s.handle_preedit("nih", Some((3, 3)));
    assert!(s.is_composing());

    s.cancel();
    assert!(!s.is_composing(), "cancel must end composition");
    assert!(s.preedit().is_empty(), "cancel must drop preedit");
    assert_eq!(s.cursor(), None);
    assert_eq!(s.take_commits(), "", "cancel must NOT promote preedit to commit buffer");
}

#[test]
fn cancel_preserves_undrained_commit_buffer() {
    // Defensive: if a commit arrived but the host hasn't drained yet, a
    // later cancel (e.g. focus loss before the next redraw tick) must
    // not eat those bytes.
    let mut s = ImeState::new();
    s.handle_commit("hi");
    s.cancel();
    assert_eq!(s.take_commits(), "hi");
}

#[test]
fn preedit_layout_at_origin_cursor_anchors_at_top_left_inset() {
    // Cursor in cell (0,0) — preedit should sit directly under the
    // tab-bar + padding, not floating at window origin.
    let (cell_w, cell_h, pad, top) = (10.0_f32, 20.0_f32, 4.0_f32, 30.0_f32);
    let (cx, cy) = cursor_px(0, 0, cell_w, cell_h, pad, top);
    let mut s = ImeState::new();
    s.handle_preedit("a", Some((1, 1)));
    let layout = ImePreeditLayout::compute(&s, cx, cy, cell_w, cell_h, 800.0, 600.0).unwrap();
    assert!((layout.bg.x - pad).abs() < f32::EPSILON);
    assert!((layout.bg.y - (top + cell_h)).abs() < f32::EPSILON);
}

#[test]
fn empty_preedit_yields_no_layout() {
    // No in-flight composition means no overlay quad / text area should
    // be emitted — caller chains with `if let Some(...)`.
    let s = ImeState::new();
    assert!(ImePreeditLayout::compute(&s, 50.0, 50.0, 10.0, 20.0, 800.0, 600.0).is_none());
}
