//! Regression contracts for the native operations owned by this sibling.

use crate::lib_tests::child_output;
use std::path::PathBuf;

#[test]
fn configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors() {
    const CHILD: &str = "SONICTERM_CONFIGURED_FONT_LOG_CHILD";
    const MISSING: &str = "SonicTerm Missing Diagnostic Primary";
    const OTHER: &str = "SonicTerm Missing Diagnostic Style";
    const FALLBACK: &str = "SonicTerm Missing Diagnostic Fallback";
    const REC_MONO: &str = "Rec Mono St.Helens";
    const URL: &str = "https://github.com/D0n9X1n/SonicTerm/wiki/Configuration";
    /// How a diagnostic names the family it failed to load; the replacement it names differs.
    fn reported_identity(family: &str) -> String {
        format!("{family:?} (weight=")
    }
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
                    "font_resolution::font_resolution_tests::configured_font_diagnostics_use_sonicterm_guidance_without_hiding_errors",
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
            assert!(errors.iter().all(|line| !line.contains(&reported_identity(REC_MONO))));
            let default_family = reported_identity(config::DEFAULT_MONOSPACE_FAMILIES[0]);
            assert!(errors.iter().all(|line| !line.contains(&default_family)), "{stderr}");
            let primary = errors
                .iter()
                .find(|line| line.contains(MISSING) && line.contains("Regular"))
                .unwrap();
            assert!(primary.contains("configured primary font"), "{primary}");
            assert!(primary.contains(&format!("Using \"{REC_MONO}\" instead")), "{primary}");
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
    settings.font_dirs =
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-fonts/rec-mono")];
    settings.font.font = vec![
        primary.clone(),
        config::FontAttributes::new_fallback(FALLBACK),
        config::FontAttributes::new_fallback(REC_MONO),
    ];
    let settings = config::ConfigHandle::new(settings);
    let fonts = crate::FontConfigInner::new(Some(settings.clone()), 72).unwrap();
    let (_, handles) = fonts.resolve_font_helper(&settings.font, &settings, 14).unwrap();
    assert_eq!(handles.first().unwrap().names().family, REC_MONO);
    // A repeat at another size, as a zoom or a scale change does, reports the same miss only once.
    fonts.resolve_font_helper(&settings.font, &settings, 18).unwrap();
    for synthetic in [settings.font.make_bold(), settings.font.make_italic()] {
        fonts.resolve_font_helper(&synthetic, &settings, 14).unwrap();
    }

    // These nonsynthetic requests exercise the private helper's alternate wording, not the shipping style-selection path.
    let requests = [
        config::FontAttributes { weight: config::FontWeight::BOLD, ..primary },
        config::FontAttributes::new(OTHER),
        config::FontAttributes::new(FALLBACK),
        config::FontAttributes::new(REC_MONO),
    ];
    for requested in requests {
        let style = config::TextStyle {
            font: vec![requested, config::FontAttributes::new_fallback(REC_MONO)],
            foreground: None,
        };
        let (_, handles) = fonts.resolve_font_helper(&style, &settings, 14).unwrap();
        assert_eq!(handles.first().unwrap().names().family, REC_MONO);
    }
    // A platform default the host lacks, such as Cascadia Mono on an older Windows, is no user error.
    let default_missing = config::TextStyle {
        font: vec![
            config::FontAttributes::new(config::DEFAULT_MONOSPACE_FAMILIES[0]),
            config::FontAttributes::new_fallback(REC_MONO),
        ],
        foreground: None,
    };
    fonts.resolve_font_helper(&default_missing, &settings, 14).unwrap();
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
                .any(|line| line.contains(&reported_identity(REC_MONO))));
        } else {
            assert!(!captured.contains("configured-font-error-positive-control"));
            assert!(!captured.contains("Unable to load"));
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
