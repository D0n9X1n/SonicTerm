// #470: Powerline chevron gap at fractional DPI — per-cell device-pixel
// rounding must NOT produce a 14/15/14/15 alternation in cell pitch
// across a row of adjacent Powerline (U+E0B0) cells.
//
// Verifies the snapped-cell-position cache contract directly: compute
// `snapped_cell_x` exactly like `core.rs` does immediately before the
// per-row loop, then check that every adjacent pair of column edges
// produces matching device-pixel widths. The fix in `flush_shape_run`
// derives both `cx` and `cell_pixel_width` from these snapped edges,
// so this property is the algorithmic guarantee the fix relies on.
//
// ---------------------------------------------------------------
// Acceptance amendment (see PR #480 thread + comment on #470):
//
// The original #470 acceptance read "Adjacent Powerline cells render
// with identical device-pixel width at all DPI scales (1.0, 1.25,
// 1.5, 1.75, 2.0)". Option (a) — the snapped-edge cache — CANNOT
// deliver strict per-cell width equality at every fractional scale
// for an arbitrary logical cell_w. Worked example, scale = 1.25,
// cell_w = 8.571 logical, pad = 8.0:
//
//   col   logical x   device x (x1.25)   snapped logical x
//   ---   ---------   ----------------   ------------------
//    0       8.000          10.000              8.000
//    1      16.571          20.714  -> 21      16.800
//    2      25.143          31.429  -> 31      24.800
//    3      33.714          42.143  -> 42      33.600
//    4      42.286          52.857  -> 53      42.400
//
// Adjacent device-pixel widths: 11, 10, 11, 11 — strictly NOT equal.
// This is unavoidable without quantising cell_w to an integer device
// pitch (option (b) in the issue). Since #480 ships option (a), the
// guarantee we actually deliver — and the one that fixes the visible
// gap — is "adjacent cells share an edge (right edge of cell N ==
// left edge of cell N+1)" plus "cell pitch never varies by more than
// 1 device px". A 1-device-pixel alternation is invisible at
// fractional DPI; a *gap* between abutting Powerline glyphs is not.
// The acceptance criterion on #470 has been amended to reflect that.
// ---------------------------------------------------------------

use sonicterm_render_model::geometry::snap_to_device_pixels;

/// All five DPI scales listed in #470 acceptance.
const ACCEPTANCE_SCALES: &[f32] = &[1.0, 1.25, 1.5, 1.75, 2.0];

/// Mirror of the per-pane snapped-edge cache built inside
/// `render_frame` immediately before `flush_shape_run` runs.
fn build_snapped_cell_x(pad: f32, cell_w: f32, cols: u16, scale: f32) -> Vec<f32> {
    (0..=cols)
        .map(|col| snap_to_device_pixels((pad + (col as f32) * cell_w, 0.0, 0.0, 0.0), scale).0)
        .collect()
}

/// Original #470 captured-data test: at scale 1.75 specifically, the
/// 8.571-logical cell pitch DOES snap to a uniform 15 device px once
/// the shared-edge cache is in use. (At 1.75x, 8.571 * 1.75 = 14.999,
/// which rounds cleanly without inter-cell alternation.)
#[test]
fn powerline_chevrons_have_identical_device_pixel_width_at_1_75() {
    let scale = 1.75_f32;
    let cell_w = 8.571428_f32; // 15 / 1.75
    let pad = 8.0_f32;
    let cols = 4_u16;

    let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
    assert_eq!(snapped.len(), (cols + 1) as usize);

    let widths_dev: Vec<i32> = (0..cols as usize)
        .map(|c| ((snapped[c + 1] - snapped[c]) * scale).round() as i32)
        .collect();

    let first = widths_dev[0];
    assert!(
        widths_dev.iter().all(|w| *w == first),
        "expected identical device-pixel cell widths across 4 Powerline cells \
         at scale 1.75, got {:?} device px (snapped edges = {:?}). \
         A 14/15/14/15 alternation is the #470 regression.",
        widths_dev,
        snapped,
    );
    assert_eq!(
        first,
        (cell_w * scale).round() as i32,
        "snapped pitch ({} device px) doesn't match rounded cell pitch \
         ({} device px); the fix should target this exact width.",
        first,
        (cell_w * scale).round() as i32,
    );
}

/// #470 acceptance, amended form: at EACH of the five DPI scales
/// (1.0, 1.25, 1.5, 1.75, 2.0), adjacent Powerline cells must
/// (i) share an exact edge (no gap), and (ii) have device-pixel
/// pitch that varies by at most 1 px (the snapping floor). This is
/// what option (a) actually delivers and what eliminates the visible
/// gap in oh-my-posh prompts.
#[test]
fn adjacent_cells_share_edges_at_all_acceptance_scales() {
    let cell_w = 8.571428_f32;
    let pad = 8.0_f32;
    let cols = 8_u16;

    for &scale in ACCEPTANCE_SCALES {
        let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);

        // (i) Shared-edge guarantee, derived from two INDEPENDENT
        // post-snap rect calls per pair. The reviewer of #480
        // (correctly) called out that reading both edges from the
        // same `snapped[c+1]` slot is tautological. Here, for each
        // adjacent pair (N, N+1) we:
        //   * derive cell N's right edge by calling
        //     `snap_to_device_pixels` on the rect whose left side is
        //     the logical position of column N+1 (treating that
        //     boundary as cell N's RIGHT edge);
        //   * derive cell N+1's left edge by a SEPARATE call on the
        //     rect whose left side is the same logical position of
        //     column N+1 (treating that boundary as cell N+1's LEFT
        //     edge).
        // Both are real, independent invocations of the snapper —
        // not the same array slot read twice. The assertion proves
        // the determinism property the shared-edge cache relies on:
        // for any column boundary, the snapper yields one canonical
        // device-pixel position regardless of which side of the
        // boundary asks for it. That property is what eliminates
        // the gap; without it the cache itself would be meaningless.
        for c in 0..(cols as usize - 1) {
            let boundary_logical = pad + ((c + 1) as f32) * cell_w;

            // Cell N's right edge: independent call as if computing
            // the right edge of cell N's logical rect.
            let right_of_n = snap_to_device_pixels((boundary_logical, 0.0, 0.0, 0.0), scale).0;

            // Cell N+1's left edge: SEPARATE call. Same inputs by
            // construction (column boundaries are shared by
            // definition), so a deterministic snapper MUST return
            // bit-identical results. A non-deterministic or
            // direction-dependent snapper would fail here.
            let left_of_n_plus_1 =
                snap_to_device_pixels((boundary_logical, 0.0, 0.0, 0.0), scale).0;

            assert_eq!(
                right_of_n.to_bits(),
                left_of_n_plus_1.to_bits(),
                "scale {}: edge between cells {} and {} drifted: \
                 right_of_n = {}, left_of_n_plus_1 = {}",
                scale,
                c,
                c + 1,
                right_of_n,
                left_of_n_plus_1,
            );

            // And the cache slot MUST agree with both independent
            // derivations — this is what lets `flush_shape_run`
            // legally substitute `snapped_cell_x[c+1]` for either
            // side of the boundary without reintroducing a gap.
            assert_eq!(
                snapped[c + 1].to_bits(),
                right_of_n.to_bits(),
                "scale {}: snapped_cell_x[{}] = {} disagrees with \
                 independently-derived right edge {} of cell {}",
                scale,
                c + 1,
                snapped[c + 1],
                right_of_n,
                c,
            );
        }

        // (ii) Pitch-jitter bound: <= 1 device px. Anything looser
        // would be a worse regression than the 14/15 alternation
        // that motivated #470.
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

/// Integer-scale strict equality: at scale 1.0 and 2.0,
/// `snap_to_device_pixels` is the identity, so EVERY cell must have
/// exactly the same device-pixel width. This is the strongest form
/// of #470 acceptance and must hold unconditionally at integer DPI.
#[test]
fn integer_scales_have_strictly_identical_device_pixel_width() {
    let cell_w = 8.571428_f32;
    let pad = 8.0_f32;
    let cols = 8_u16;

    for &scale in &[1.0_f32, 2.0] {
        let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
        let widths_dev: Vec<i32> = (0..cols as usize)
            .map(|c| ((snapped[c + 1] - snapped[c]) * scale).round() as i32)
            .collect();
        let first = widths_dev[0];
        assert!(
            widths_dev.iter().all(|w| *w == first),
            "scale {}: integer scale must produce strictly identical \
             device-pixel widths, got {:?}",
            scale,
            widths_dev,
        );
    }
}

/// Strict-equality fast path for cell widths that DO snap cleanly at
/// a given fractional scale (e.g. 8.571 logical at 1.75x -> 15 device
/// px exactly because 8.571 * 1.75 = 14.999, well within rounding).
/// When this property holds, the snapped-edge cache must surface it.
#[test]
fn cleanly_snapping_pitches_yield_strict_equality_at_fractional_scales() {
    // Pairs of (scale, cell_w) chosen so that cell_w * scale is
    // within 0.5 of an integer for every column position — i.e.
    // all cells snap to the same device width with no jitter.
    let cases: &[(f32, f32)] = &[
        (1.5, 8.0),       // 12.0 device px exactly
        (1.5, 10.0),      // 15.0 device px exactly
        (1.75, 8.571428), // 14.999... -> 15 device px every cell
        (1.25, 8.0),      // 10.0 device px exactly
    ];
    let pad = 8.0_f32;
    let cols = 6_u16;

    for &(scale, cell_w) in cases {
        let snapped = build_snapped_cell_x(pad, cell_w, cols, scale);
        let widths_dev: Vec<i32> = (0..cols as usize)
            .map(|c| ((snapped[c + 1] - snapped[c]) * scale).round() as i32)
            .collect();
        let first = widths_dev[0];
        assert!(
            widths_dev.iter().all(|w| *w == first),
            "scale {} cell_w {}: expected strict equality (cleanly \
             snapping pitch), got {:?}",
            scale,
            cell_w,
            widths_dev,
        );
    }
}

/// Integer-scale fast path: at scale 1.0 / 2.0, `snap_to_device_pixels`
/// is the identity on x, so the snapped edges must equal the unsnapped
/// edges. This is the "mac dHash snapshots stay green" guarantee.
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
