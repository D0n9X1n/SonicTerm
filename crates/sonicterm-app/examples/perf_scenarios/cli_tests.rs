//! Pins argument parsing and the refusals that happen before any window opens.

use std::ffi::OsStr;
use std::path::PathBuf;

use super::*;
use crate::scenarios::Host;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn run_parses_every_flag_in_any_order_after_the_id() {
    // The comparison script passes these flags; the scratch directory may come before them.
    let full = parse_run(&args(&[
        "S2",
        "--variant",
        "flood",
        "--managed",
        "--short",
        "--laps",
        "--harness-hash",
        "0a1B2c",
        "/tmp/perf-s2",
    ]));
    let expected = RunArgs {
        scenario: "S2",
        variant: "flood",
        managed: true,
        short: true,
        laps: true,
        counters: false,
        harness_hash: Some("0a1B2c".into()),
        scratch: "/tmp/perf-s2".into(),
        capture_delivery: false,
    };
    assert_eq!(full, Ok(expected));
    let plain = RunArgs {
        scenario: "S1",
        variant: "default",
        managed: false,
        short: false,
        laps: false,
        counters: false,
        harness_hash: None,
        scratch: "/tmp/perf-s1".into(),
        capture_delivery: false,
    };
    assert_eq!(parse_run(&args(&["S1", "/tmp/perf-s1"])), Ok(plain));
    let trailing = parse_run(&args(&["S1", "/tmp/perf-s1", "--short"]));
    assert!(matches!(trailing, Ok(RunArgs { short: true, .. })));
}

#[test]
fn malformed_run_arguments_are_refused() {
    // Each refusal exits 2 before any window opens, so a typo never measures the wrong thing.
    let cases: &[&[&str]] = &[
        &[],
        &["S13", "/tmp/perf"],
        &["S1", "--variant", "flood", "/tmp/perf"],
        &["S1", "--variant"],
        &["S1"],
        &["S1", "/tmp/one", "/tmp/two"],
        &["S1", "--short", "--short", "/tmp/perf"],
        &["S2", "--variant", "flood", "--variant", "default", "/tmp/perf"],
        &["S1", "--harness-hash", "xyz", "/tmp/perf"],
        &["S1", "--harness-hash", "", "/tmp/perf"],
        &["S1", "--harness-hash"],
        &["S1", "--bogus", "/tmp/perf"],
    ];
    for bad in cases {
        assert!(parse_run(&args(bad)).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn list_alone_succeeds_and_anything_else_prints_usage() {
    // `--list` works on every platform; anything else that is not `--run` is a refusal.
    assert_eq!(run_code(&args(&["--list"]), None), 0);
    assert_eq!(run_code(&args(&["--list", "extra"]), None), 2);
    assert_eq!(run_code(&args(&[]), None), 2);
    assert_eq!(run_code(&args(&["--bogus"]), None), 2);
}

#[cfg(target_os = "macos")]
#[test]
fn malformed_run_is_refused_before_anything_is_created() {
    // Parsing and the scratch check fail first, so no directory or window is created.
    assert_eq!(run_code(&args(&["--run", "S13", "/tmp/perf-never"]), None), 2);
    assert_eq!(run_code(&args(&["--run", "S1", "relative-scratch"]), None), 2);
    assert!(!std::path::Path::new("relative-scratch").exists());
}

#[test]
fn scratch_must_be_a_new_absolute_directory_under_the_temp_root() {
    // A reused or outside directory could mix runs or write where the harness must not.
    let pid = std::process::id();
    let root = std::env::temp_dir().join(format!("perf-scenarios-cli-{pid}"));
    let outside = std::env::temp_dir().join(format!("perf-scenarios-cli-outside-{pid}"));
    for dir in [&root, &outside] {
        let _ = std::fs::remove_dir_all(dir);
    }
    std::fs::create_dir_all(root.join("existing")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let canonical_root = root.canonicalize().unwrap();
    let fresh = format!("{}/new-run", root.display());
    assert_eq!(check_scratch(&fresh, &canonical_root), Ok(PathBuf::from(&fresh)));
    for refused in [
        "relative/run".to_owned(),
        "/".to_owned(),
        format!("{}/existing", root.display()),
        format!("{}/missing-parent/run", root.display()),
        format!("{}/run", outside.display()),
        format!("{}/it's", root.display()),
        format!("{}/tab\there", root.display()),
    ] {
        assert!(check_scratch(&refused, &canonical_root).is_err(), "{refused} accepted");
    }
    for dir in [&root, &outside] {
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn inherited_no_color_or_rust_log_is_refused() {
    // NO_COLOR changes rendering and RUST_LOG replaces the configured log level.
    assert!(check_environment(None, None).is_ok());
    assert!(check_environment(Some(OsStr::new("1")), None).is_err());
    assert!(check_environment(Some(OsStr::new("")), None).is_err(), "set but empty still counts");
    assert!(check_environment(None, Some(OsStr::new("debug"))).is_err());
    assert!(check_environment(None, Some(OsStr::new(""))).is_err());
}

#[test]
fn counters_are_accepted_only_by_a_build_with_the_counter_api() {
    // A build without perf-counters refuses --counters with exit 2 before any window opens;
    // a build with it accepts the flag once.
    let parsed = parse_run(&args(&["S1", "--counters", "/tmp/perf-s1"]));
    if cfg!(feature = "perf-counters") {
        assert!(matches!(parsed, Ok(RunArgs { counters: true, .. })), "{parsed:?}");
    } else {
        let reason = parsed.unwrap_err();
        assert!(reason.contains("lacks the perf-counters feature"), "{reason}");
    }
    if cfg!(all(target_os = "macos", not(feature = "perf-counters"))) {
        // The refusal comes from parsing, so no scratch directory or window is created.
        let code = run_code(&args(&["--run", "S1", "--counters", "/tmp/perf-never-created"]), None);
        assert_eq!(code, REFUSED);
    }
    let twice = parse_run(&args(&["S1", "--counters", "--counters", "/tmp/perf-s1"]));
    assert_eq!(twice, Err("--counters given twice".to_owned()));
}

#[test]
fn program_mode_needs_no_arguments_and_a_scratch_variable() {
    // ConPTY starts a pane's shell with no arguments; any argument means the harness's own CLI.
    let scratch = || Some(std::ffi::OsString::from(r"C:\Temp\perf-run"));
    assert_eq!(program_scratch(&[], scratch()), Some(PathBuf::from(r"C:\Temp\perf-run")));
    assert_eq!(program_scratch(&args(&["--list"]), scratch()), None);
    assert_eq!(program_scratch(&args(&[""]), scratch()), None);
    assert_eq!(program_scratch(&[], None), None);
    assert_eq!(program_scratch(&[], Some(std::ffi::OsString::new())), None);
}

#[test]
fn harness_path_that_would_break_the_toml_is_refused() {
    // The harness path becomes a TOML literal string, which a quote or control character ends early.
    assert_eq!(check_harness_shell(r"C:\Temp\target\debug\examples\perf_scenarios.exe"), Ok(()));
    assert!(check_harness_shell(r"C:\Users\it's\perf_scenarios.exe").is_err());
    assert!(check_harness_shell("C:\\Temp\\tab\there.exe").is_err());
}

#[test]
fn presenter_variants_are_refused_off_windows() {
    // On macOS `force` only degrades pacing, so gdi, wgpu and role-exit run only on Windows.
    for variant in ["gdi", "wgpu", "role-exit"] {
        assert!(variant_supported(variant, Host::Windows), "{variant}");
        assert!(!variant_supported(variant, Host::Posix), "{variant}");
    }
    for variant in ["default", "flood", "sync", "selection-drag"] {
        assert!(
            variant_supported(variant, Host::Posix) && variant_supported(variant, Host::Windows)
        );
    }
    // parse_run applies the build host's answer, so the refusal comes before any window opens.
    let parsed = parse_run(&args(&["S1", "--variant", "gdi", "/tmp/perf-s1"]));
    assert_eq!(parsed.is_ok(), cfg!(windows), "{parsed:?}");
}

#[test]
fn capture_delivery_is_refused_off_windows() {
    // The replay runs the harness under ConPTY, so it exists only on Windows. It takes none of a
    // measured run's flags and replays only the scenarios whose delivery the comparison checks.
    let capture = ["S10", "--variant", "sync", "--short", "--capture-delivery", "C:/tmp/replay"];
    assert!(parse_run_on(&args(&capture), Host::Posix).is_err());
    let parsed = parse_run_on(&args(&capture), Host::Windows).unwrap();
    assert!(parsed.capture_delivery && parsed.short && !parsed.managed);
    assert_eq!((parsed.scenario, parsed.variant), ("S10", "sync"));
    assert_eq!(parsed.scratch, "C:/tmp/replay");
    // Every variant the catalog lists for a replayed scenario is accepted, S11's presenters included.
    for scenario in DELIVERY_SCENARIOS {
        for variant in scenarios::find(scenario).unwrap().variants {
            let request = ["--variant", variant, "--capture-delivery", "C:/tmp/replay"];
            let mut full = vec![scenario];
            full.extend(request);
            assert!(parse_run_on(&args(&full), Host::Windows).is_ok(), "{scenario}/{variant}");
        }
    }
    let refused: &[&[&str]] = &[
        &["S1", "--capture-delivery", "C:/tmp/replay"],
        &["S12", "--capture-delivery", "C:/tmp/replay"],
        &["S10", "--managed", "--capture-delivery", "C:/tmp/replay"],
        &["S10", "--laps", "--capture-delivery", "C:/tmp/replay"],
        &["S10", "--harness-hash", "ab", "--capture-delivery", "C:/tmp/replay"],
        &["S10", "--capture-delivery", "C:/tmp/replay", "C:/tmp/other"],
        &["S10", "--capture-delivery", "C:/tmp/one", "--capture-delivery", "C:/tmp/two"],
        &["S10", "--capture-delivery"],
    ];
    for bad in refused {
        assert!(parse_run_on(&args(bad), Host::Windows).is_err(), "{bad:?} parsed");
    }
    // A measured run is never a capture.
    let measured = parse_run_on(&args(&["S10", "C:/tmp/run"]), Host::Windows).unwrap();
    assert!(!measured.capture_delivery);
}
