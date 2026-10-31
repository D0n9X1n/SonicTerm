use super::*;

const OPTIONS: [&str; 6] = [
    "FT_CONFIG_OPTION_ERROR_STRINGS",
    "FT_CONFIG_OPTION_SYSTEM_ZLIB",
    "FT_CONFIG_OPTION_USE_PNG",
    "PCF_CONFIG_OPTION_LONG_FAMILY_NAMES",
    "FT_CONFIG_OPTION_SUBPIXEL_RENDERING",
    "TT_CONFIG_OPTION_SUBPIXEL_HINTING",
];

/// The HVF driver switch, commented out as upstream ships it.
const HVF_DISABLED: &str = "/* #define FT_CONFIG_OPTION_HVF */\n";

/// The required options, each commented out as upstream may ship them.
fn required() -> String {
    OPTIONS.iter().map(|name| format!("/* #define {name} */\n")).collect()
}

/// A synthetic `ftoption.h`: every required option plus the HVF switch, all disabled.
fn header() -> String {
    required() + HVF_DISABLED
}

#[test]
fn enables_every_required_boolean_option() {
    // All required options must be effective even when upstream disables them by default.
    let configured = configure_freetype(&header(), false).unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn accepts_existing_boolean_defaults_without_rewriting_other_options() {
    // Already-enabled upstream defaults are valid and unrelated configuration remains untouched.
    let source = required().replace("/* #define", "#define").replace(" */", "")
        + HVF_DISABLED
        + "#define UNRELATED_OPTION 7\n";
    assert_eq!(configure_freetype(&source, false).unwrap(), source);
}

#[test]
fn accepts_whitespace_and_crlf_in_option_declarations() {
    // Upstream formatting may change without changing the supported option contract.
    let source = header().replace("/* #define ", "  /*\t#  define\t").replace(" */\n", "\t*/\r\n");
    let configured = configure_freetype(&source, false).unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn rejects_a_missing_required_option() {
    // Removed or renamed upstream options must stop the build rather than silently disable a feature.
    let source = header().replace("/* #define FT_CONFIG_OPTION_USE_PNG */\n", "");
    assert!(configure_freetype(&source, false).unwrap_err().contains("FT_CONFIG_OPTION_USE_PNG"));
}

#[test]
fn rejects_ambiguous_duplicate_options() {
    // Two declarations cannot establish one deterministic feature state.
    let source = header() + "#define FT_CONFIG_OPTION_USE_PNG\n";
    assert!(configure_freetype(&source, false).unwrap_err().contains("FT_CONFIG_OPTION_USE_PNG"));
}

#[test]
fn rejects_obsolete_numeric_hinting_modes() {
    // Current FreeType uses a boolean; silently restoring removed numeric modes masks an upstream change.
    let source = header().replace(
        "/* #define TT_CONFIG_OPTION_SUBPIXEL_HINTING */",
        "#define TT_CONFIG_OPTION_SUBPIXEL_HINTING  2",
    );
    assert!(configure_freetype(&source, false)
        .unwrap_err()
        .contains("TT_CONFIG_OPTION_SUBPIXEL_HINTING"));
}

#[test]
fn does_not_accept_documentation_as_an_option_definition() {
    // A macro mentioned inside a multiline comment is not an effective declaration.
    let source = header().replace(
        "/* #define FT_CONFIG_OPTION_USE_PNG */",
        "/*\n#define FT_CONFIG_OPTION_USE_PNG\n*/",
    );
    assert!(configure_freetype(&source, false).is_err());
}

#[test]
fn binding_regeneration_uses_the_native_build_configuration() {
    // Both binding generators must read the same configured header before the pristine upstream headers.
    let helper = include_str!("../../scripts/freetype-config.rs");
    assert!(helper.contains("build_config::configure_freetype"));
    assert!(helper.contains("../crates/sonicterm-freetype/build_config.rs"));
    for script in [
        include_str!("../../scripts/regenerate-freetype.sh"),
        include_str!("../../scripts/regenerate-harfbuzz.sh"),
    ] {
        assert!(script.contains("$ROOT/scripts/freetype-config.rs"));
        let headers = script.lines().filter(|line| line.trim_start().starts_with("-- -I"));
        let mut count = 0;
        for headers in headers {
            let configured = headers.find("-I\"$scratch/include\"").unwrap();
            let upstream = headers.find("freetype2/include").unwrap();
            assert!(configured < upstream);
            count += 1;
        }
        assert!(count > 0);
    }
}

#[test]
fn compiler_probe_checks_every_effective_option() {
    // The compiler, not the text transform, decides whether upstream conditionals leave features enabled.
    let probe = configuration_probe(false);
    assert!(probe.starts_with("#include <freetype/config/ftoption.h>\n"));
    for option in OPTIONS {
        assert!(probe.contains(&format!("#ifndef {option}\n#error")));
    }
}

#[test]
fn current_vendor_header_preserves_required_rendering_features() {
    // The checked-in header must satisfy the same contract exercised by synthetic drift cases.
    let configured =
        configure_freetype(include_str!("freetype2/include/freetype/config/ftoption.h"), false)
            .unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn hvf_stays_disabled_unless_requested() {
    // Without an SDK that ships `hvf/Scaler.h`, the driver must remain upstream's rejecting stub.
    let configured = configure_freetype(&header(), false).unwrap();
    assert!(configured.lines().any(|line| line == HVF_DISABLED.trim_end()));
    assert!(!configured.lines().any(|line| line == "#define FT_CONFIG_OPTION_HVF"));
}

#[test]
fn hvf_is_enabled_when_requested() {
    // An HVF-capable Apple SDK turns the commented upstream switch into an effective definition.
    let configured = configure_freetype(&header(), true).unwrap();
    assert!(configured.lines().any(|line| line == "#define FT_CONFIG_OPTION_HVF"));
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn an_upstream_enabled_hvf_switch_is_rejected_when_not_requested() {
    // Upstream enabling HVF by default would include `hvf/Scaler.h` on hosts without it; stop instead.
    let source = required() + "#define FT_CONFIG_OPTION_HVF\n";
    assert!(configure_freetype(&source, false).unwrap_err().contains("FT_CONFIG_OPTION_HVF"));
    assert!(configure_freetype(&source, true).is_ok(), "an enabled switch satisfies a request");
}

#[test]
fn a_missing_repeated_or_valued_hvf_switch_is_rejected_in_both_modes() {
    // The HVF switch follows the same single-boolean-declaration contract as the required options.
    let missing = required();
    let repeated = header() + HVF_DISABLED;
    let valued = required() + "/* #define FT_CONFIG_OPTION_HVF 1 */\n";
    for hvf in [false, true] {
        for source in [&missing, &repeated, &valued] {
            let error = configure_freetype(source, hvf).unwrap_err();
            assert!(error.contains("FT_CONFIG_OPTION_HVF"), "{}", error);
        }
    }
}

#[test]
fn compiler_probe_checks_the_requested_hvf_state() {
    // The preprocessor must agree with the request in both directions, so a stray definition fails.
    assert!(configuration_probe(true).contains("#ifndef FT_CONFIG_OPTION_HVF\n#error"));
    assert!(configuration_probe(false).contains("#ifdef FT_CONFIG_OPTION_HVF\n#error"));
}

#[test]
fn current_vendor_header_can_enable_hvf() {
    // The checked-in header carries exactly one commented HVF switch for the Apple build to enable.
    let configured =
        configure_freetype(include_str!("freetype2/include/freetype/config/ftoption.h"), true)
            .unwrap();
    assert!(configured.lines().any(|line| line == "#define FT_CONFIG_OPTION_HVF"));
}

#[test]
fn macos_versions_parse_to_major_and_minor() {
    // Deployment targets are written as `major[.minor[.patch]]`; anything else is unreadable.
    assert_eq!(parse_macos_version("15.0"), Some((15, 0)));
    assert_eq!(parse_macos_version(" 15.4.1 "), Some((15, 4)));
    assert_eq!(parse_macos_version("27"), Some((27, 0)));
    assert_eq!(parse_macos_version(""), None);
    assert_eq!(parse_macos_version("fifteen"), None);
    assert_eq!(parse_macos_version("15.x"), None);
}

#[test]
fn hvf_needs_an_sdk_and_a_deployment_target_below_the_runtime_floor() {
    // Below 15.4 `__builtin_available` stays a runtime check and `libhvf` loads weakly; an unset or
    // newer target folds the check away and makes the load strong, so the build must stop.
    assert_eq!(hvf_build_decision(true, Some("15.0"), false), Ok(true));
    assert_eq!(hvf_build_decision(true, Some("15.3.9"), true), Ok(true));
    for target in [None, Some("15.4"), Some("16.0"), Some("garbage")] {
        let error = hvf_build_decision(true, target, false).unwrap_err();
        assert!(error.contains("MACOSX_DEPLOYMENT_TARGET"), "{}", error);
    }
}

#[test]
fn an_sdk_without_hvf_builds_the_stub_unless_hvf_is_required() {
    // Local SDKs without `hvf/Scaler.h` keep today's build; CI and release require HVF and fail.
    assert_eq!(hvf_build_decision(false, Some("15.0"), false), Ok(false));
    assert_eq!(hvf_build_decision(false, None, false), Ok(false));
    let error = hvf_build_decision(false, Some("15.0"), true).unwrap_err();
    assert!(error.contains("SONICTERM_REQUIRE_HVF"), "{}", error);
}
