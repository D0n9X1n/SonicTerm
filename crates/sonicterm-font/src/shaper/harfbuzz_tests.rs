use super::*;
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use std::path::PathBuf;

// A real fallback error keeps stage identity but never persists the affected run or raw shape dumps.
#[test]
fn production_shaping_logs_exclude_payloads_from_crash_history() {
    const CHILD: &str = "SONICTERM_FONT_LOG_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let mut failures = Vec::new();
        for filter in ["sonicterm=warn", "sonicterm=trace"] {
            let output = crate::lib_tests::child_output(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "shaper::harfbuzz::harfbuzz_tests::production_shaping_logs_exclude_payloads_from_crash_history", "--nocapture"])
                .env(CHILD, "1")
                .env("RUST_LOG", filter));
            if !output.status.success() {
                failures.push(format!(
                    "{filter}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        return;
    }
    let directory = std::env::temp_dir().join(format!("sonicterm-font-log-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    let base = ParsedFont::from_locator(&bundled_font_handle()).unwrap();
    let mut absent = base.clone();
    absent.handle.source = FontDataSource::OnDisk(directory.join("missing.ttf"));
    let config = config::ConfigHandle::new(config::Config::default());
    let shaper = HarfbuzzShaper::new(&config, &[base, absent], 2).unwrap();
    let text = "synthetic-run-\u{1f600}\u{fe0e}";
    let mut missing = Vec::new();
    let glyphs = shaper
        .shape(text, 12.0, 96, &mut missing, None, Direction::LeftToRight, None, None)
        .unwrap();
    assert!(!glyphs.is_empty());
    tracing::warn!(target: "sonicterm_font::control", "font-warn-positive-control");
    let dump =
        sonicterm_logging::crash::__test_write_dump(&directory.join("dump"), "synthetic").unwrap();
    let history = std::fs::read_to_string(dump).unwrap();
    drop(guard);
    let mut sink = String::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("sonicterm.log") {
            sink.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    let trace_requested = std::env::var("RUST_LOG").unwrap() == "sonicterm=trace";
    assert_eq!(sink.contains("synthetic-run-"), trace_requested);
    assert_eq!(sink.contains("sonicterm_font::payload"), trace_requested);
    assert!(history.contains("font-warn-positive-control"));
    assert!(history.contains("font fallback shaping failed"), "{history}");
    assert!(!history.contains("synthetic-run-") && !history.contains('\u{1f600}'));
    assert!(!history.contains("info_clusters:") && !history.contains("cluster_resolver:"));
    std::fs::remove_dir_all(directory).unwrap();
}

fn bundled_font_handle() -> FontDataHandle {
    FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: None,
    }
}

#[test]
fn missing_fallback_replaces_a_nonzero_multibyte_cluster_without_panicking() {
    // A stale fallback used to apply the source range 1..8 to a three-byte replacement string.
    let base = ParsedFont::from_locator(&bundled_font_handle()).unwrap();
    let mut missing_fallback = base.clone();
    missing_fallback.handle.source = FontDataSource::OnDisk(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/shaper/sonicterm-missing-fallback-font.ttf"),
    );
    let config = config::ConfigHandle::new(config::Config::default());
    let shaper = HarfbuzzShaper::new(&config, &[base, missing_fallback], 2).unwrap();
    let text = "a\u{1f600}\u{fe0e}";
    let mut no_glyphs = Vec::new();

    let glyphs = shaper
        .shape(text, 12.0, 96, &mut no_glyphs, None, Direction::LeftToRight, None, None)
        .unwrap();

    let replacements: Vec<_> = glyphs.iter().filter(|glyph| glyph.cluster == 1).collect();
    assert!(!replacements.is_empty(), "missing fallback should preserve the source cluster");
    assert!(replacements
        .iter()
        .any(|glyph| { matches!(glyph.only_char, Some(std::char::REPLACEMENT_CHARACTER | '?')) }));
    assert_eq!(
        replacements.iter().map(|glyph| u16::from(glyph.num_cells)).sum::<u16>(),
        UnicodeWidthStr::width("\u{1f600}\u{fe0e}") as u16
    );
}

/// The on-disk face at `path`, face 0.
fn face_at(path: PathBuf) -> FontDataHandle {
    FontDataHandle { source: FontDataSource::OnDisk(path), ..bundled_font_handle() }
}

/// A shaper over `handles` whose first `configured` are the configured chain.
fn shaper_with(handles: &[FontDataHandle], configured: usize) -> HarfbuzzShaper {
    let parsed: Vec<ParsedFont> =
        handles.iter().map(|handle| ParsedFont::from_locator(handle).unwrap()).collect();
    let config = config::ConfigHandle::new(config::Config::default());
    HarfbuzzShaper::new(&config, &parsed, configured).unwrap()
}

/// Each glyph `shaper` gives `text`, as (glyph id, face index).
fn glyphs(shaper: &HarfbuzzShaper, text: &str) -> Vec<(u32, usize)> {
    let mut missing = Vec::new();
    let shaped = shaper
        .shape(text, 12.0, 96, &mut missing, None, Direction::LeftToRight, None, None)
        .unwrap();
    shaped.iter().map(|glyph| (glyph.glyph_pos, glyph.font_idx)).collect()
}

/// A configured fallback face that drew nothing stays resident: shaping a missing emoji six times
/// opens each configured face once. A face fallback discovery appended keeps WezTerm's rule, so it
/// is unloaded after each run it draws nothing for and reopens every time. Output never changes.
#[test]
fn configured_faces_stay_resident_and_discovered_faces_unload() {
    let italic = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test-fonts/rec-mono/RecMonoSt.Helens-Italic.ttf");
    let opens = |configured: usize| {
        let shaper = shaper_with(&[bundled_font_handle(), face_at(italic.clone())], configured);
        FACES_OPENED.with(|count| count.set(0));
        let first = glyphs(&shaper, "a\u{1f600}");
        for _ in 0..5 {
            assert_eq!(glyphs(&shaper, "a\u{1f600}"), first, "a resident face shapes alike");
        }
        FACES_OPENED.with(std::cell::Cell::get)
    };
    assert_eq!(opens(2), 2, "both configured faces open once");
    assert_eq!(opens(1), 7, "the discovered face reopens for each of the six runs");
}

/// Shaping history never changes a run's glyphs, whether the second face is configured or
/// discovered: 一 after 一 + U+035C with [Symbols Nerd Font Mono, Rec Mono], and E + U+0302 after E
/// and U+0302 alone with [Symbols, CJKSymbolsFallback face 0], whose components map to nothing on
/// their own but whose composition maps. Each matches a fresh shaper.
#[test]
fn shaping_history_never_changes_a_runs_glyphs() {
    let symbols = face_at(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf"),
    );
    let cjk_symbols = PathBuf::from("/System/Library/Fonts/CJKSymbolsFallback.ttc");
    for configured in [1, 2] {
        let handles = [symbols.clone(), bundled_font_handle()];
        let used = shaper_with(&handles, configured);
        glyphs(&used, "\u{4e00}\u{35c}");
        let fresh = glyphs(&shaper_with(&handles, configured), "\u{4e00}");
        assert_eq!(glyphs(&used, "\u{4e00}"), fresh, "一 with {configured} configured");
        if !cjk_symbols.exists() {
            // When: the host has no CJKSymbolsFallback.ttc, the composition case cannot run.
            println!("skipped E + U+0302: {} is absent", cjk_symbols.display());
            continue;
        }
        let handles = [symbols.clone(), face_at(cjk_symbols.clone())];
        let used = shaper_with(&handles, configured);
        glyphs(&used, "E");
        glyphs(&used, "\u{302}");
        let fresh = glyphs(&shaper_with(&handles, configured), "E\u{302}");
        assert_eq!(glyphs(&used, "E\u{302}"), fresh, "E + U+0302 with {configured} configured");
    }
}
