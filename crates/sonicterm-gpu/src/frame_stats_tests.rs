//! Pins the renderer's frame statistics: the counting gate, draining, shaping counts, damage
//! and the shaping call sites.

use std::path::Path;

use sonicterm_render_model::geometry::PixelRect;

use super::*;

#[test]
fn notes_outside_a_counting_renderer_record_nothing() {
    // With the renderer's counting flag off, no statistic is written.
    note_shape_request();
    note_buffer_writes(64, 32);
    note_row_cache(true);
    {
        let _not_counting = CollectGuard::enter(false);
        note_frame(true);
    }
    assert_eq!(drain(), FrameStats::ZERO);
}

#[test]
fn a_counting_renderer_collects_until_drained_and_restores_the_enclosing_gate() {
    // Each note lands once while counting; a non-counting renderer inside it records nothing.
    {
        let _counting = CollectGuard::enter(true);
        note_buffer_writes(64, 32);
        note_damage(250);
        note_frame(false);
        note_frame(true);
        note_row_cache(true);
        note_row_cache(false);
        note_row_cache(false);
        {
            let _not_counting = CollectGuard::enter(false);
            note_shape_request();
        }
        note_shape_request();
    }
    note_shape_request();
    let expected = FrameStats {
        vertex_bytes: 64,
        index_bytes: 32,
        damage_permille_sum: 250,
        damaged_frames: 1,
        software_frames: 1,
        gpu_frames: 1,
        row_cache_hits: 1,
        row_cache_misses: 2,
        shape_requests: 1,
    };
    assert_eq!(drain(), expected);
    assert_eq!(drain(), FrameStats::ZERO, "draining empties the collector");
}

#[test]
fn each_shaping_request_counts_once_failures_included() {
    // A request that fails was still made, so it counts.
    let _counting = CollectGuard::enter(true);
    let width: Result<f32, &str> = shape_request(|| Ok(12.5));
    let failed: Result<f32, &str> = shape_request(|| Err("no face"));
    assert_eq!((width, failed), (Ok(12.5), Err("no face")));
    assert_eq!(drain().shape_requests, 2);
}

#[test]
fn damage_is_the_damaged_share_of_the_surface_in_permille() {
    // A share of an empty surface is 0, and damage never counts above the whole surface.
    let full = PixelRect { x: 0, y: 0, w: 800, h: 600 };
    let quarter = PixelRect { x: 0, y: 0, w: 400, h: 300 };
    assert_eq!(damage_permille(&full, 800, 600), 1_000);
    assert_eq!(damage_permille(&quarter, 800, 600), 250);
    assert_eq!(damage_permille(&full, 0, 600), 0);
    assert_eq!(damage_permille(&full, 400, 300), 1_000);
}

/// Every non-test Rust source under `dir`, recursively, as `(path, text)`.
fn crate_sources(dir: &Path, found: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("crate sources") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            crate_sources(&path, found);
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            found.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
        }
    }
}

#[test]
fn every_font_stack_shaping_call_goes_through_shape_request() {
    // A call outside the wrapper goes uncounted; the wrapper counts each one exactly once.
    let mut sources = Vec::new();
    crate_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
    let mut wrapped = 0;
    let mut bare = Vec::new();
    for (path, text) in &sources {
        if path.ends_with("frame_stats.rs") {
            continue;
        }
        for method in [".shape_text_with_style(", ".shape_text(", ".measure_text_width("] {
            for (offset, _) in text.match_indices(method) {
                let mut start = offset.saturating_sub(80);
                while !text.is_char_boundary(start) {
                    start -= 1;
                }
                if text[start..offset].contains("shape_request(||") {
                    wrapped += 1;
                } else {
                    bare.push(format!("{path}: byte {offset} {method}"));
                }
            }
        }
    }
    assert!(bare.is_empty(), "uncounted FontStack shaping calls: {bare:#?}");
    assert_eq!(wrapped, 8, "the shaping sites changed; review the count");
}

#[test]
fn renderer_entry_points_collect_only_under_their_counting_flag() {
    // The frame and the two layout entry points the App calls directly collect for this renderer.
    let core = include_str!("core.rs");
    assert_eq!(core.matches("frame_stats::CollectGuard::enter(self.counting)").count(), 3);
}
