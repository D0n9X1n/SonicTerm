//! Renderer inspection types contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

/// A readback row is padded to wgpu's copy alignment, and unpadding keeps each row's pixels and
/// drops the padding, so two renderers' frames compare byte for byte whatever their padding.
#[test]
fn readback_rows_pad_to_the_copy_alignment_and_unpad_to_tight_rows() {
    assert_eq!(padded_readback_row_bytes(1), 256, "one pixel pads to one alignment unit");
    assert_eq!(padded_readback_row_bytes(64), 256, "exactly one unit needs no padding");
    assert_eq!(padded_readback_row_bytes(65), 512);
    // Two rows of one pixel each, padded to 256 bytes; the padding holds a marker byte.
    let mut mapped = vec![0xEE; 2 * 256];
    mapped[..4].copy_from_slice(&[1, 2, 3, 4]);
    mapped[256..260].copy_from_slice(&[5, 6, 7, 8]);
    assert_eq!(unpad_readback_rows(&mapped, 1, 2, 256), vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

/// Production never asks for a copyable retained frame: the constructor and every rebuild pass the
/// test-only readback flag, which only `__enable_retained_frame_readback` sets, and only that flag
/// adds `COPY_SRC`.
#[test]
fn only_the_test_readback_flag_makes_the_retained_frame_copyable() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let create = source.split_once("\npub(super) fn create_frame_texture(").unwrap().1;
    let create = create.split_once("\n}\n").unwrap().0;
    assert!(create.contains("if copy_source"), "COPY_SRC is conditional");
    let compact: String = source.split_whitespace().collect();
    let constructor = compact.split_once("asyncfnnew_async(").unwrap().1;
    let build =
        "build_frame_texture(&device,software_presenter,config.width,config.height,format,false,)";
    assert!(constructor.contains(build), "the constructor builds without COPY_SRC");
    assert!(source.contains("            retained_frame_readback: false,\n"));
    let enable = source.split_once("    pub fn __enable_retained_frame_readback(").unwrap().1;
    assert!(enable
        .split_once("\n    }\n")
        .unwrap()
        .0
        .contains("self.retained_frame_readback = true;"));
    assert_eq!(source.matches("self.retained_frame_readback = true;").count(), 1);
}

/// Every presented frame that records its damage share also records its waste, from the same
/// final damage and the parts it unions.
#[test]
fn presented_frames_record_damage_waste_beside_damage() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    let finish: String = function_body(&source, "    pub(super) fn finish_successful_frame(")
        .split_whitespace()
        .collect();
    let damage = finish.find("crate::frame_stats::note_damage(||").expect("damage recorded");
    let waste = finish.find("crate::frame_stats::note_damage_waste(||").expect("waste recorded");
    assert!(damage < waste);
    assert!(finish.contains(
        "crate::frame_stats::damage_waste_permille(&plan.damage,&plan.damage_parts,surface_width,surface_height,)"
    ));
}

/// A presented frame's damage is narrow only when it is not a first frame and covers less than
/// the surface, so a whole-surface repaint can never pass a narrow-damage assertion: a first
/// frame, a surface-sized rectangle and a larger-than-surface rectangle are all not narrow.
#[test]
fn presented_damage_is_narrow_only_below_the_surface_and_after_the_first_frame() {
    let surface = PixelRect { x: 0, y: 0, w: 200, h: 100 };
    let row = PixelRect { x: 0, y: 40, w: 200, h: 24 };
    let narrow = PresentedDamage { first_frame: false, damage: row, surface };
    assert!(narrow.is_narrow());
    assert!(!PresentedDamage { first_frame: true, ..narrow }.is_narrow());
    assert!(!PresentedDamage { damage: surface, ..narrow }.is_narrow());
    let oversized = PixelRect { x: -10, y: -10, w: 400, h: 400 };
    assert!(!PresentedDamage { damage: oversized, ..narrow }.is_narrow());
}

/// A presented-damage recorder that was never enabled, as in production, keeps no snapshot and
/// never builds one: its builder is not called, so a presented frame pays only the branch. Once
/// enabled it keeps the last snapshot until a read takes it.
#[test]
fn a_recorder_never_enabled_keeps_no_presented_damage_snapshot() {
    let mut recorder = PresentedDamageRecorder::default();
    recorder.record(|| panic!("a disabled recorder builds no snapshot"));
    assert_eq!(recorder.take(), None);
    let surface = PixelRect { x: 0, y: 0, w: 200, h: 100 };
    let snapshot = PresentedDamage { first_frame: false, damage: surface, surface };
    recorder.enable();
    recorder.record(|| snapshot);
    assert_eq!(recorder.take(), Some(snapshot));
    assert_eq!(recorder.take(), None, "a read takes the snapshot");
}

/// The test hooks are opt-in: the renderer starts with a disabled recorder, only
/// `__enable_presented_damage` enables it, and assembly tests the injected glyph's `Option`
/// before calling the seam. Line endings are normalized before scanning.
#[test]
fn presented_damage_recording_and_glyph_injection_are_opt_in() {
    let source = crate::core::CORE_SOURCE.replace("\r\n", "\n");
    assert!(source.contains("            presented_damage: PresentedDamageRecorder::default(),\n"));
    assert_eq!(source.matches("self.presented_damage.enable();").count(), 1);
    let enable = source.split_once("    pub fn __enable_presented_damage(").expect("hook").1;
    let enable = enable.split_once("\n    }\n").expect("hook body").0;
    assert!(enable.contains("self.presented_damage.enable();"));
    let assembly =
        source.split_once("    pub(super) fn assemble_frame(").expect("assemble_frame exists").1;
    assert!(assembly.contains("self.assemble_backgrounds("));
    let background = include_str!("core_assemble_backgrounds.rs").replace("\r\n", "\n");
    let guard = background.find("if self.injected_test_glyph.is_some() {").expect("Option guard");
    let call = background.find("self.push_injected_test_glyph(").expect("seam call");
    assert!(guard < call, "the seam is reached only when a glyph is injected");
}
