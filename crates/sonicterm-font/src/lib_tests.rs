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
            timing: None,
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
    assert!(sink.contains("https://github.com/D0n9X1n/SonicTerm/wiki/Configuration"));
    assert!(sink.contains("[font].family"));
    assert!(!sink.contains("wezterm.org"));
    assert!(!sink.contains("warn_about_missing_glyphs=false"));
    assert!(
        !history.contains("synthetic-secret")
            && !history.contains('\u{1f600}')
            && !history.contains("1f600")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors() {
    const CHILD: &str = "SONICTERM_CONFIGURED_FONT_LOG_CHILD";
    const MISSING: &str = "SonicTerm Missing Diagnostic Primary";
    const OTHER: &str = "SonicTerm Missing Diagnostic Style";
    const FALLBACK: &str = "SonicTerm Missing Diagnostic Fallback";
    const PACKAGED: &str = "Rec Mono St.Helens";
    const URL: &str = "https://github.com/D0n9X1n/SonicTerm/wiki/Configuration";
    // These targets must survive a font capture; a replacement filter can silently drop them from files and crash history.
    const RETAINED_WARNINGS: [&str; 5] = [
        "font-capture-exit-control",
        "font-capture-atlas-control",
        "font-capture-reclaimed-control",
        "font-capture-wgpu-control",
        "font-capture-naga-control",
    ];

    // Isolated production logging proves missing requests remain visible and valid or synthetic requests stay quiet.
    let Some(mode) = std::env::var_os(CHILD) else {
        let capture_filter = format!("config=error,{}", sonicterm_logging::DEFAULT_FILTER);
        // Both published recipes must match the filter whose retained warnings this test exercises.
        for documentation in [
            include_str!("../../../wiki/Logging.md"),
            include_str!("../../../wiki/Logging-zh-CN.md"),
        ] {
            assert!(documentation.contains(&format!("RUST_LOG={capture_filter}")));
        }
        for mode in ["default", "admitted"] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "lib_tests::configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env_remove("RUST_LOG");
            if mode == "admitted" {
                command.env("RUST_LOG", &capture_filter);
            }
            let output = child_output(&mut command);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{}\n{stderr}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(stderr.contains("font-diagnostic-positive-control"));
            assert!(stderr.contains("configured-font-error-positive-control"));
            for marker in RETAINED_WARNINGS {
                assert!(stderr.contains(marker), "missing stderr warning {marker}");
            }
            let errors: Vec<_> =
                stderr.lines().filter(|line| line.contains("Unable to load")).collect();
            assert_eq!(errors.len(), 4, "{stderr}");
            assert!(errors.iter().all(|line| line.contains("ERROR") && line.contains("config")));
            assert_eq!(errors.iter().filter(|line| line.contains(MISSING)).count(), 2);
            assert_eq!(errors.iter().filter(|line| line.contains(OTHER)).count(), 1);
            assert_eq!(errors.iter().filter(|line| line.contains(FALLBACK)).count(), 1);
            assert!(errors.iter().all(|line| !line.contains(PACKAGED)));
            let primary = errors
                .iter()
                .find(|line| line.contains(MISSING) && line.contains("Regular"))
                .unwrap();
            assert!(primary.contains("configured primary font"), "{primary}");
            assert!(
                primary.contains("weight=\"Regular\"")
                    && primary.contains("stretch=Normal")
                    && primary.contains("style=Normal")
            );
            let derived =
                errors.iter().find(|line| line.contains(MISSING) && line.contains("Bold")).unwrap();
            assert!(derived.contains("derived from the configured primary font"), "{derived}");
            for family in [OTHER, FALLBACK] {
                let requested = errors.iter().find(|line| line.contains(family)).unwrap();
                assert!(requested.contains("requested font"), "{requested}");
                assert!(!requested.contains("configured primary"), "{requested}");
            }
            for line in errors {
                assert!(line.contains(URL), "{line}");
                assert!(line.contains("[font].family"), "{line}");
                assert!(!line.contains("wezterm"), "{line}");
                assert!(!line.contains("font_rules"), "{line}");
                assert!(!line.contains("sonicterm.font("), "{line}");
                assert!(!line.contains("separate font file"), "{line}");
            }
        }
        return;
    };

    let directory =
        std::env::temp_dir().join(format!("sonicterm-font-diagnostic-{}", std::process::id()));
    let guard =
        sonicterm_logging::init_in(&sonicterm_logging::LoggingConfig::default(), &directory)
            .unwrap();
    tracing::warn!(target: "sonicterm_font::control", "font-diagnostic-positive-control");
    tracing::warn!(target: "sonic_exit", "font-capture-exit-control");
    tracing::warn!(target: "sonic::glyph_atlas", "font-capture-atlas-control");
    tracing::warn!(target: "memory::reclaimed", "font-capture-reclaimed-control");
    tracing::warn!(target: "wgpu", "font-capture-wgpu-control");
    tracing::warn!(target: "naga", "font-capture-naga-control");
    config::show_error("configured-font-error-positive-control");
    let primary = config::FontAttributes::new(MISSING);
    let mut settings = config::Config::default();
    settings.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
    settings.font_dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    settings.font.font = vec![
        primary.clone(),
        config::FontAttributes::new_fallback(FALLBACK),
        config::FontAttributes::new_fallback(PACKAGED),
    ];
    let settings = config::ConfigHandle::new(settings);
    let fonts = crate::FontConfigInner::new(Some(settings.clone()), 72).unwrap();
    let (_, handles) = fonts.resolve_font_helper(&settings.font, &settings, 14).unwrap();
    assert_eq!(handles.first().unwrap().names().family, PACKAGED);
    for synthetic in [settings.font.make_bold(), settings.font.make_italic()] {
        fonts.resolve_font_helper(&synthetic, &settings, 14).unwrap();
    }

    // These nonsynthetic requests exercise the private helper's alternate wording, not the shipping style-selection path.
    let requests = [
        config::FontAttributes { weight: config::FontWeight::BOLD, ..primary },
        config::FontAttributes::new(OTHER),
        config::FontAttributes::new(FALLBACK),
        config::FontAttributes::new(PACKAGED),
    ];
    for requested in requests {
        let style = config::TextStyle {
            font: vec![requested, config::FontAttributes::new_fallback(PACKAGED)],
            foreground: None,
        };
        let (_, handles) = fonts.resolve_font_helper(&style, &settings, 14).unwrap();
        assert_eq!(handles.first().unwrap().names().family, PACKAGED);
    }
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
    for captured in [&sink, &history] {
        assert!(captured.contains("font-diagnostic-positive-control"));
        for marker in RETAINED_WARNINGS {
            assert!(captured.contains(marker), "missing retained warning {marker}");
        }
        if mode == "admitted" {
            assert!(captured.contains("configured-font-error-positive-control"));
            assert_eq!(captured.lines().filter(|line| line.contains("Unable to load")).count(), 4);
            assert!(
                captured.contains(MISSING)
                    && captured.contains(OTHER)
                    && captured.contains(FALLBACK)
            );
            assert!(!captured
                .lines()
                .filter(|line| line.contains("Unable to load"))
                .any(|line| line.contains(PACKAGED)));
        } else {
            assert!(!captured.contains("configured-font-error-positive-control"));
            assert!(!captured.contains("Unable to load"));
        }
    }
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
