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
    let shaper = HarfbuzzShaper::new(&config, &[base, absent]).unwrap();
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
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
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
    let shaper = HarfbuzzShaper::new(&config, &[base, missing_fallback]).unwrap();
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
