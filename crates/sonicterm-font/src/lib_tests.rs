//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{linear_u8_to_srgb8, SrgbaPixel};
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use crate::select_fallback_fonts;
use std::path::PathBuf;

pub(super) fn child_output(command: &mut std::process::Command) -> std::process::Output {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let read_pipe = |mut pipe: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read_pipe(Box::new(child.stdout.take().unwrap()));
    let stderr = read_pipe(Box::new(child.stderr.take().unwrap()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            stdout.join().unwrap();
            stderr.join().unwrap();
            panic!("font test subprocess exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    std::process::Output { status, stdout: stdout.join().unwrap(), stderr: stderr.join().unwrap() }
}

// Real fallback warnings preserve stage/count identity while both WARN and DEBUG exclude requested text.
#[test]
fn fallback_resolution_diagnostics_exclude_requested_text() {
    const CHILD: &str = "SONICTERM_RESOLVER_LOG_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for filter in ["sonicterm=warn", "sonicterm=debug", "sonicterm=trace"] {
            let output = child_output(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "lib_tests::fallback_resolution_diagnostics_exclude_requested_text",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .env("RUST_LOG", filter),
            );
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    struct FailingLocator;
    impl crate::locator::FontLocator for FailingLocator {
        fn load_fonts(
            &self,
            _: &[config::FontAttributes],
            _: &mut std::collections::HashSet<config::FontAttributes>,
            _: u16,
        ) -> anyhow::Result<Vec<ParsedFont>> {
            Ok(Vec::new())
        }
        fn locate_fallback_for_codepoints(&self, _: &[char]) -> anyhow::Result<Vec<ParsedFont>> {
            Err(anyhow::anyhow!("synthetic-secret-\u{1f600}"))
        }
    }
    let directory =
        std::env::temp_dir().join(format!("sonicterm-resolver-log-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    for warn in [true, false] {
        let mut config = config::Config::default();
        config.warn_about_missing_glyphs = warn;
        crate::FallbackResolveInfo {
            no_glyphs: vec!['\u{1f600}'],
            pending: Default::default(),
            completion: Box::new(|| {}),
            font_dirs: std::sync::Arc::new(crate::db::FontDatabase::new()),
            built_in: std::sync::Arc::new(crate::db::FontDatabase::new()),
            locator: std::sync::Arc::new(FailingLocator),
            config: config::ConfigHandle::new(config),
        }
        .process();
    }
    tracing::warn!(target: "sonicterm_font::control", "resolver-positive-control");
    let path =
        sonicterm_logging::crash::__test_write_dump(&directory.join("dump"), "synthetic").unwrap();
    let history = std::fs::read_to_string(path).unwrap();
    drop(guard);
    let mut sink = String::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("sonicterm.log") {
            sink.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    let trace_requested = std::env::var("RUST_LOG").unwrap() == "sonicterm=trace";
    assert_eq!(sink.contains("synthetic-secret"), trace_requested);
    assert_eq!(sink.contains("sonicterm_font::payload"), trace_requested);
    assert!(history.contains("resolver-positive-control"));
    assert!(history.contains("stage=font-locator requested=1 error=font"));
    assert!(history.contains("1 unresolved codepoints"));
    assert!(
        !history.contains("synthetic-secret")
            && !history.contains('\u{1f600}')
            && !history.contains("1f600")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

fn fallback_fixture(coverage_chars: &[char], is_math_font: bool) -> ParsedFont {
    let mut coverage = RangeSet::new();
    for ch in coverage_chars {
        coverage.add(*ch as u32);
    }
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: Some(coverage),
    };
    let mut font = ParsedFont::from_locator(&handle).unwrap();
    font.is_math_font = is_math_font;
    font
}

#[test]
fn exports_color_primitives() {
    assert_eq!(linear_u8_to_srgb8(0), 0);
    assert_eq!(SrgbaPixel::rgba(1, 2, 3, 4).as_rgba(), (1, 2, 3, 4));
}

#[test]
fn fallback_selection_prefers_text_for_shared_symbols_and_keeps_math_only_coverage() {
    // Contract: math coverage cannot displace text coverage or be dropped when it alone fills a gap.
    let shared = '\u{23fa}';
    let math_only = '\u{2211}';
    let math = fallback_fixture(&[shared, math_only], true);
    let text = fallback_fixture(&[shared], false);
    let mut wanted = RangeSet::new();
    wanted.add(shared as u32);
    wanted.add(math_only as u32);

    let mut selected = vec![math, text];
    select_fallback_fonts(&mut selected, &mut wanted, true);

    assert_eq!(selected.len(), 2);
    assert!(!selected[0].is_math_font);
    assert!(selected[1].is_math_font);
    assert!(wanted.is_empty());
}

#[test]
fn gdi_font_creation_failures_are_rejected_before_use() {
    const SOURCE: &str = include_str!("locator/gdi.rs");

    assert!(SOURCE.contains("anyhow::ensure!(!font.is_null(), \"font handle is null\")"));
    assert!(SOURCE.contains("anyhow::ensure!(!hdc.is_null(), \"CreateCompatibleDC failed\")"));
    assert!(SOURCE.contains("if previous.is_null()"));
    assert!(SOURCE.contains("SelectObject(hdc, previous)"));
    assert_eq!(
        SOURCE.matches("anyhow::ensure!(!font.is_null(), \"CreateFontIndirectW failed\")").count(),
        2
    );
}
