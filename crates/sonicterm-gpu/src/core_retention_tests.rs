//! Renderer retention contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// Every part must reach a class, and no part may be charged twice.
///
/// The failure this guards is a part added to `RendererRetention` without a
/// matching row in `seam_classes` — it would be counted by `total()` and
/// classified as nothing, so the struct would report a byte it could not name.
///
/// A structural check on the struct, not on a charge path. Nothing charges
/// these classes; see `RendererRetention::seam_classes`.
#[test]
fn every_reported_part_is_classified_exactly_once() {
    let retention = RendererRetention {
        glyph_atlas: amount(16 * 1024 * 1024, 512),
        image_atlas: amount(8 * 1024 * 1024, 12),
        row_glyph_cache: amount(2 * 1024 * 1024, 120),
        row_quad_cache: amount(1024 * 1024, 80),
        software_frame: amount(4 * 1024 * 1024, 1),
        vertex_scratch: amount(3 * 1024 * 1024, 1),
        row_ink: amount(64 * 1024, 40),
        frame_scratch: amount(2 * 1024 * 1024, 6),
        chrome_cache: amount(96 * 1024, 7),
    };

    let classes = retention.seam_classes();
    let classified: usize = classes.iter().map(|(_, part)| part.bytes).sum();

    assert_eq!(
        classified,
        retention.total().bytes,
        "the classified parts must account for every byte the struct reports"
    );

    let distinct: std::collections::HashSet<_> = classes.iter().map(|(class, _)| *class).collect();
    assert_eq!(
        distinct.len(),
        classes.len(),
        "no class may appear twice, or bytes are counted twice"
    );
}

/// The per-row ink records are one reported part, classified once under `RowInk` and inside
/// `total()`, so the bytes a partial frame's records hold are visible and never double-counted.
#[test]
fn row_ink_is_reported_once_under_its_own_class() {
    let retention = RendererRetention { row_ink: amount(4096, 40), ..RendererRetention::default() };
    let classes = retention.seam_classes();
    let row_ink: Vec<_> =
        classes.iter().filter(|(class, _)| *class == ResourceClass::RowInk).collect();
    assert_eq!(row_ink.len(), 1);
    assert_eq!(row_ink[0].1, amount(4096, 40));
    assert_eq!(retention.total(), amount(4096, 40));
    assert!(matches!(ResourceClass::RowInk.coverage(), ClassCoverage::UnchargedRetention { .. }));
    assert_eq!(ResourceClass::RowInk.pane_seam_term(), PaneSeamTerm::NotChargedInProduction);
}

/// Pane cache eviction is one glyph-then-quad renderer operation.
#[test]
fn pane_cache_eviction_order_is_explicit() {
    const SOURCE: &str = crate::core::CORE_SOURCE;
    let start = SOURCE.find("pub fn invalidate_pane_caches").expect("pane cache eviction seam");
    let body = &SOURCE[start..];
    let glyph = body.find("self.row_glyph_cache.invalidate_pane(pane_id)").expect("glyph eviction");
    let quad = body.find("self.line_quad_cache.invalidate_pane(pane_id)").expect("quad eviction");
    assert!(glyph < quad, "glyph cache must evict before quad cache");
}

/// Both row-cache parts remain separately classified and uncharged.
#[test]
fn row_cache_parts_have_distinct_uncharged_classes() {
    let classes = RendererRetention::default().seam_classes();
    for class in [ResourceClass::RowGlyphCache, ResourceClass::RowQuadCache] {
        assert!(
            classes.iter().any(|(reported, _)| *reported == class),
            "{class:?} must remain a named renderer-retention part"
        );
        assert!(
            matches!(class.coverage(), ClassCoverage::UnchargedRetention { .. }),
            "{class:?} is measured and reported but reaches no governor owner"
        );
        assert_eq!(class.pane_seam_term(), PaneSeamTerm::NotChargedInProduction);
    }
}

/// The row-cache coverage envelopes bound what each cache can retain. The glyph cache enforces
/// its envelope: the production payload and tracking budgets sum to exactly the recorded
/// figure, so reported storage cannot pass it however many panes are drawn. The quad envelope
/// is pinned to four maximum viewport working sets of its replay record.
#[test]
fn row_cache_coverage_envelopes_cover_maximum_visible_payloads() {
    use sonicterm_text::row_glyph_cache::{
        DEFAULT_PAYLOAD_BUDGET_BYTES, DEFAULT_TRACKING_BUDGET_BYTES,
    };
    let ClassCoverage::UnchargedRetention { per_owner_bytes: glyph_bound } =
        ResourceClass::RowGlyphCache.coverage()
    else {
        panic!("RowGlyphCache must record its uncharged high-water envelope");
    };
    assert_eq!(DEFAULT_PAYLOAD_BUDGET_BYTES + DEFAULT_TRACKING_BUDGET_BYTES, glyph_bound);
    assert_eq!(glyph_bound, 512 * 1024 * 1024);
    let cache = sonicterm_text::row_glyph_cache::RowGlyphCache::new();
    assert_eq!(cache.payload_budget() + cache.tracking_budget(), glyph_bound);

    let visible_cells =
        sonicterm_render_model::boundary::grid::grid::MAX_VISIBLE_GRID_CELLS as usize;
    let headroom = 4usize;
    let table_entry = std::mem::size_of::<((u64, u64, u64), (u64, Vec<u8>))>() + 2;
    let quad_required = visible_cells
        .saturating_mul(headroom)
        .saturating_mul(std::mem::size_of::<crate::quad::QuadInstance>())
        .saturating_add(usize::from(u16::MAX).saturating_mul(table_entry));
    let ClassCoverage::UnchargedRetention { per_owner_bytes: quad_bound } =
        ResourceClass::RowQuadCache.coverage()
    else {
        panic!("RowQuadCache must record its uncharged high-water envelope");
    };
    assert!(quad_bound >= quad_required, "quad bound {quad_bound} < {quad_required}");
}

/// The software frame is reported on every platform, zero where absent.
///
/// A caller reading renderer classes should not need a `#[cfg(windows)]`
/// branch — an absent part and an empty part read the same.
#[test]
fn the_software_frame_part_is_present_on_every_platform() {
    let retention = RendererRetention::default();
    let classes = retention.seam_classes();

    assert!(
        classes.iter().any(|(class, _)| *class == ResourceClass::SoftwareFrame),
        "SoftwareFrame must be classified on every platform, zero where there is no software path"
    );
    assert_eq!(retention.total(), ResourceAmount::default(), "a default renderer holds nothing");
}

/// An empty renderer reports nothing.
#[test]
fn an_empty_renderer_reports_nothing() {
    let classes = RendererRetention::default().seam_classes();
    assert!(classes.iter().all(|(_, part)| part.bytes == 0 && part.items == 0));
}

/// The resource table's `SoftwareFrame` bound is this crate's surface clamp.
///
/// `ClassCoverage::UnchargedRetention { per_owner_bytes }` asks how much can
/// hide in a class nothing charges. The software frame is
/// `width * height * 4`, so no single figure describes it and the honest
/// answer is the most one surface may hold — [`MAX_SURFACE_BYTES`], which
/// `validated_surface_size` enforces on every construction and resize.
///
/// The table previously carried one 4K frame, 33,177,600 bytes. That is one
/// common window, not a bound: a 5K window holds 178% of it and the clamp
/// admits 5.06x. Nothing noticed it drifting, because the figure was a literal
/// checked against nothing.
///
/// Asserted against the constant, not a copy of its value, so moving the clamp
/// without moving the table fails here.
#[test]
fn the_tabled_software_frame_bound_is_the_surface_clamp() {
    use sonicterm_types::{ClassCoverage, ResourceClass};

    let ClassCoverage::UnchargedRetention { per_owner_bytes } =
        ResourceClass::SoftwareFrame.coverage()
    else {
        panic!(
            "SoftwareFrame must be UnchargedRetention: this crate computes it and no \
             governor charges it"
        );
    };

    assert_eq!(
        u64::try_from(per_owner_bytes).expect("the bound fits u64"),
        MAX_SURFACE_BYTES,
        "the resource table's SoftwareFrame bound and this crate's surface clamp \
         disagree; the table would misstate what one frame can hold"
    );

    // And the clamp is reachable in principle, or the bound is fiction. The
    // per-axis cap is the binding constraint, not the byte cap.
    let max_pixels = u64::from(MAX_SURFACE_DIMENSION) * u64::from(MAX_SURFACE_DIMENSION);
    assert!(
        max_pixels * 4 >= MAX_SURFACE_BYTES,
        "the dimension cap makes the byte clamp unreachable, so the bound describes \
         a surface that cannot exist"
    );
}

/// The vertex scratch is renderer-held CPU storage: `retained_amounts` reports the
/// presentation pipeline's figure together with both atlas uploads' rect lists, `total()` counts
/// it, and it is tagged as upload staging.
#[test]
fn vertex_scratch_is_part_of_the_retained_report() {
    let retention =
        RendererRetention { vertex_scratch: amount(68 * 4096, 1), ..RendererRetention::default() };
    assert_eq!(retention.total(), amount(68 * 4096, 1));
    assert!(retention
        .seam_classes()
        .contains(&(ResourceClass::UploadStaging, amount(68 * 4096, 1))));

    let core: String = crate::core::CORE_SOURCE.split_whitespace().collect();
    let report = core.find("pubfnretained_amounts(&self)").expect("retained_amounts");
    let body = &core[report..report + 600];
    assert!(body.contains("vertex_scratch:self.upload_staging_retained(),"));
    let staging = core.split_once("fnupload_staging_retained(&self)").expect("helper").1;
    assert!(staging[..300].contains("self.present_pipeline.vertex_scratch_retained()"));
}

/// The `UploadStaging` row of `seam_classes` is exactly the vertex scratch, reported live.
///
/// That class's recorded coverage figure is the atlas staging ceiling only, and the scratch
/// has no fixed ceiling, so the row is not compared against it: a scratch larger than the
/// atlas figure is still reported as measured, once.
#[test]
fn upload_staging_row_is_the_live_vertex_scratch_not_the_atlas_figure() {
    let ClassCoverage::UnchargedRetention { per_owner_bytes: atlas_figure } =
        ResourceClass::UploadStaging.coverage()
    else {
        panic!("UploadStaging records the atlas staging ceiling");
    };
    let scratch = amount(atlas_figure + 4096, 1);
    let retention = RendererRetention { vertex_scratch: scratch, ..RendererRetention::default() };

    let rows: Vec<ResourceAmount> = retention
        .seam_classes()
        .iter()
        .filter(|(class, _)| *class == ResourceClass::UploadStaging)
        .map(|(_, part)| *part)
        .collect();

    assert_eq!(rows, vec![scratch], "one UploadStaging row, equal to the vertex scratch");
}

#[test]
fn frame_scratch_and_chrome_cache_are_classified_parts_with_shared_envelopes() {
    // The two parts this renderer reports beside its caches are classified once each, sum into
    // `total()`, and their class envelopes are the same figures the renderer computes from its
    // caps and table sizes, so neither side can drift alone.
    let retention = RendererRetention {
        frame_scratch: amount(3 * 1024, 2),
        chrome_cache: amount(5 * 1024, 4),
        ..RendererRetention::default()
    };
    let classes = retention.seam_classes();
    assert_eq!(classes.len(), 9, "nine reported parts");
    for (class, part) in [
        (ResourceClass::FrameScratch, retention.frame_scratch),
        (ResourceClass::ChromeCache, retention.chrome_cache),
    ] {
        let rows: Vec<_> = classes.iter().filter(|(row_class, _)| *row_class == class).collect();
        assert_eq!(rows.len(), 1, "{class:?} is classified once");
        assert_eq!(rows[0].1, part);
        assert_eq!(class.pane_seam_term(), PaneSeamTerm::NotChargedInProduction);
    }
    assert_eq!(retention.total(), amount(8 * 1024, 6), "total() includes both parts");
    assert_eq!(
        ResourceClass::FrameScratch.coverage(),
        ClassCoverage::UnchargedRetention { per_owner_bytes: frame_scratch::FRAME_SCRATCH_CAP }
    );
    assert_eq!(
        ResourceClass::ChromeCache.coverage(),
        ClassCoverage::UnchargedRetention {
            per_owner_bytes: crate::chrome_cache::CHROME_CACHE_ENVELOPE_BYTES
        }
    );
}
