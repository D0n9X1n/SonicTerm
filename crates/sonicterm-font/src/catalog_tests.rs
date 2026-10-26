use super::*;
use std::path::PathBuf;

/// Every face under `dirs`, parsed the way the font-directory database parses them.
fn parsed_faces(dirs: &[&str]) -> Vec<ParsedFont> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut settings = config::Config::default();
    settings.font_dirs = dirs.iter().map(|dir| root.join(dir)).collect();
    crate::db::FontDatabase::with_font_dirs(&settings).unwrap().list_available()
}

/// Only fixed-pitch faces that cover ASCII text count: the four Rec Mono faces collapse to one
/// family, proportional Roboto is left out, and the fixed-pitch icon-only symbols font is too.
#[test]
fn monospace_families_keep_fixed_pitch_text_faces_only() {
    let fonts = parsed_faces(&[
        "crates/sonicterm-font/test-fonts/rec-mono",
        "crates/sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c",
        "assets/fonts",
    ]);
    let families: Vec<&str> = fonts.iter().map(|font| font.names().family.as_str()).collect();
    for expected in ["Rec Mono St.Helens", "Roboto", "Symbols Nerd Font Mono"] {
        assert!(families.contains(&expected), "fixture {expected} parsed: {families:?}");
    }
    assert_eq!(monospace_families(&fonts), ["Rec Mono St.Helens"]);
}

/// Probe, not a gate: how long this host's enumeration takes and what it finds.
#[test]
#[ignore = "enumerates the host's installed fonts; run by hand to measure"]
fn installed_monospace_families_on_this_host() {
    let started = std::time::Instant::now();
    let families = installed_monospace_families().unwrap();
    eprintln!("{} families in {:?}: {families:?}", families.len(), started.elapsed());
}
