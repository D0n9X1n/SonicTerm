use super::*;
use std::path::PathBuf;

/// A stack whose configured family is missing names the face that stood in for regular text, and
/// one whose family is installed names that family.
#[test]
fn the_resolved_family_names_the_face_regular_text_uses() {
    let fonts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sonicterm-font/test-fonts/rec-mono");
    let stack = |families: &[(&str, bool)]| {
        FontStack::try_new_with_font_dirs_for_test(families, vec![fonts.clone()], 14.0, 72, 1.0)
            .unwrap()
    };
    let missing = stack(&[("SonicTerm No Such Family", false), ("Rec Mono St.Helens", true)]);
    assert_eq!(missing.resolved_family().as_deref(), Some("Rec Mono St.Helens"));
    let installed = stack(&[("Rec Mono St.Helens", false)]);
    assert_eq!(installed.resolved_family().as_deref(), Some("Rec Mono St.Helens"));
}
