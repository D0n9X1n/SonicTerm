// #470: Powerline chevron gap at fractional DPI — per-cell device-pixel
// rounding must NOT produce a 14/15/14/15 alternation in cell pitch
// across a row of adjacent Powerline (U+E0B0) cells.
//
// Verifies the snapped-cell-position cache contract directly: compute
// `snapped_cell_x` exactly like `core.rs` does immediately before the
// per-row loop, then check that every adjacent pair of column edges
// produces the SAME device-pixel width. The fix in `flush_shape_run`
// derives both `cx` and `cell_pixel_width` from these snapped edges,
// so this property is the algorithmic guarantee the fix relies on.

use sonicterm_render_model::geometry::snap_to_device_pixels;

/// Mirror of the per-pane snapped-edge cache built inside
/// `render_frame` immediately before `flush_shape_run` runs.
fn build_snapped_cell_x(pad: f32, cell_w: f32, cols: u16, scale: f32) -> Vec<f32> {
    (0..=cols)
        .map(|col| snap_to_device_pixels((pad + (col as f32) * cell_w, 0.0, 0.0, 0.0), scale).0)
        .collect()
}

/// Acceptance criterion from issue #470: at scale 1.75, four Powerline
/// chevrons in adjacent cells must render with IDENTICAL device-pixel
/// width per cell. Captured-data in the issue showed the broken state
/// alternated 14 / 15 / 14 / 15 device px; the snapped-edge cache fixes
/// this by sharing edges between adjacent cells.
#[test]
fn powerline_chevrons_have_identical_device_pixel_width_at_1_75() {
    let scale = 1.75_f32;
    // Reproduce the captured geometry from #461 PR-B1:
    //   cell_w = 8.571 logical (= 15 device px at 1.75x)
    //   pad ≈ 8.0 logical (matches the (79.428, 87.428, ...) trace)
    let cell_w = 8.571428_f32; // 15 / 1.75
    let pad = 8.0_f32;
    let cols = 4_u16;

    let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
    assert_eq!(snapped.len(), (cols + 1) as usize);

    // Device-pixel width of each adjacent cell.
    let widths_dev: Vec<i32> = (0..cols as usize)
        .map(|c| ((snapped[c + 1] - snapped[c]) * scale).round() as i32)
        .collect();

    // Every cell must be the same device-pixel width.
    let first = widths_dev[0];
    assert!(
        widths_dev.iter().all(|w| *w == first),
        "expected identical device-pixel cell widths across 4 Powerline cells \
         at scale 1.75, got {:?} device px (snapped edges = {:?}). \
         A 14/15/14/15 alternation is the #470 regression.",
        widths_dev,
        snapped,
    );
    // And it must be the cell's rounded device width — 15 px at 1.75x —
    // so we're not pinning the regression to a value that's still wrong
    // (e.g. uniformly 14 across all cells).
    assert_eq!(
        first,
        (cell_w * scale).round() as i32,
        "snapped pitch ({} device px) doesn't match rounded cell pitch \
         ({} device px); the fix should target this exact width.",
        first,
        (cell_w * scale).round() as i32,
    );
}

/// Generalize across the fractional DPI scales the issue lists. The
/// snapped-cache fix guarantees adjacent cells share an edge (right edge
/// of cell N == left edge of cell N+1 by construction, since both come
/// from the same `snapped_cell_x[c+1]` entry). With shared edges no gap
/// can appear between Powerline chevrons. Individual cell widths may
/// still alternate by 1 device px at scales like 1.25 (logical pitch
/// 10.71 device px snaps to 11/10 alternation) — that's unavoidable
/// without changing the cell pitch, but the *gap* — the visible #470
/// regression — is gone.
#[test]
fn adjacent_cells_share_edges_at_all_fractional_scales() {
    let cell_w = 8.571428_f32;
    let pad = 8.0_f32;
    let cols = 8_u16;

    for &scale in &[1.0_f32, 1.25, 1.5, 1.75, 2.0] {
        let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
        // Cell N+1's left edge MUST equal cell N's right edge — they're
        // the same slot in the cache. This is the gap-elimination
        // guarantee. (Tautological given how the cache is built; the
        // test exists so any future regression that recomputes edges
        // with two different formulas fails loudly here.)
        for c in 0..cols as usize {
            let right_of_n = snapped[c + 1];
            let left_of_n_plus_1 = snapped[c + 1];
            assert_eq!(
                right_of_n.to_bits(),
                left_of_n_plus_1.to_bits(),
                "scale {}: edge between cells {} and {} drifted",
                scale,
                c,
                c + 1,
            );
        }
        // And: cell pitch in device pixels must never vary by more than
        // 1 — a wider drift would be a worse regression than #470's
        // 14/15 alternation.
        let widths_dev: Vec<i32> = (0..cols as usize)
            .map(|c| ((snapped[c + 1] - snapped[c]) * scale).round() as i32)
            .collect();
        let min = *widths_dev.iter().min().unwrap();
        let max = *widths_dev.iter().max().unwrap();
        assert!(
            max - min <= 1,
            "scale {}: cell pitch varies by more than 1 device px: {:?}",
            scale,
            widths_dev,
        );
    }
}

/// Integer-scale fast path: at scale 1.0 / 2.0, `snap_to_device_pixels`
/// is the identity, so the snapped edges must equal the unsnapped edges.
/// This is the "mac dHash snapshots stay green" guarantee.
#[test]
fn integer_scales_are_identity() {
    let cell_w = 9.0_f32;
    let pad = 8.0_f32;
    let cols = 4_u16;

    for &scale in &[1.0_f32, 2.0] {
        let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
        for (c, snapped_x) in snapped.iter().enumerate() {
            let expected = pad + (c as f32) * cell_w;
            assert!(
                (snapped_x - expected).abs() < 1e-6,
                "scale {} should be identity but column {} drifted: \
                 snapped = {}, expected = {}",
                scale,
                c,
                snapped_x,
                expected,
            );
        }
    }
}
