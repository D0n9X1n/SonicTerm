use super::*;

const OPTIONS: [&str; 6] = [
    "FT_CONFIG_OPTION_ERROR_STRINGS",
    "FT_CONFIG_OPTION_SYSTEM_ZLIB",
    "FT_CONFIG_OPTION_USE_PNG",
    "PCF_CONFIG_OPTION_LONG_FAMILY_NAMES",
    "FT_CONFIG_OPTION_SUBPIXEL_RENDERING",
    "TT_CONFIG_OPTION_SUBPIXEL_HINTING",
];

fn header() -> String {
    OPTIONS.iter().map(|name| format!("/* #define {name} */\n")).collect()
}

#[test]
fn enables_every_required_boolean_option() {
    // All required options must be effective even when upstream disables them by default.
    let configured = configure_freetype(&header()).unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn accepts_existing_boolean_defaults_without_rewriting_other_options() {
    // Already-enabled upstream defaults are valid and unrelated configuration remains untouched.
    let source = header().replace("/* #define", "#define").replace(" */", "")
        + "#define UNRELATED_OPTION 7\n";
    assert_eq!(configure_freetype(&source).unwrap(), source);
}

#[test]
fn accepts_whitespace_and_crlf_in_option_declarations() {
    // Upstream formatting may change without changing the supported option contract.
    let source = header().replace("/* #define ", "  /*\t#  define\t").replace(" */\n", "\t*/\r\n");
    let configured = configure_freetype(&source).unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}

#[test]
fn rejects_a_missing_required_option() {
    // Removed or renamed upstream options must stop the build rather than silently disable a feature.
    let source = header().replace("/* #define FT_CONFIG_OPTION_USE_PNG */\n", "");
    assert!(configure_freetype(&source).unwrap_err().contains("FT_CONFIG_OPTION_USE_PNG"));
}

#[test]
fn rejects_ambiguous_duplicate_options() {
    // Two declarations cannot establish one deterministic feature state.
    let source = header() + "#define FT_CONFIG_OPTION_USE_PNG\n";
    assert!(configure_freetype(&source).unwrap_err().contains("FT_CONFIG_OPTION_USE_PNG"));
}

#[test]
fn rejects_obsolete_numeric_hinting_modes() {
    // Current FreeType uses a boolean; silently restoring removed numeric modes masks an upstream change.
    let source = header().replace(
        "/* #define TT_CONFIG_OPTION_SUBPIXEL_HINTING */",
        "#define TT_CONFIG_OPTION_SUBPIXEL_HINTING  2",
    );
    assert!(configure_freetype(&source).unwrap_err().contains("TT_CONFIG_OPTION_SUBPIXEL_HINTING"));
}

#[test]
fn does_not_accept_documentation_as_an_option_definition() {
    // A macro mentioned inside a multiline comment is not an effective declaration.
    let source = header().replace(
        "/* #define FT_CONFIG_OPTION_USE_PNG */",
        "/*\n#define FT_CONFIG_OPTION_USE_PNG\n*/",
    );
    assert!(configure_freetype(&source).is_err());
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
    let probe = configuration_probe();
    assert!(probe.starts_with("#include <freetype/config/ftoption.h>\n"));
    for option in OPTIONS {
        assert!(probe.contains(&format!("#ifndef {option}\n#error")));
    }
}

#[test]
fn current_vendor_header_preserves_required_rendering_features() {
    // The checked-in header must satisfy the same contract exercised by synthetic drift cases.
    let configured =
        configure_freetype(include_str!("freetype2/include/freetype/config/ftoption.h")).unwrap();
    for option in OPTIONS {
        assert!(configured.lines().any(|line| line == format!("#define {option}")));
    }
}
