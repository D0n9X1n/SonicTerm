//! Starter configuration documents the same defaults the typed schema uses.

use super::*;

/// The generated user config states the safe default and the explicit stripe-order choices.
#[test]
fn default_template_documents_subpixel_aa_policy() {
    let template = default_config_template();

    assert!(template.contains("subpixel_aa = \"off\""));
    assert!(template.contains("\"rgb\""));
    assert!(template.contains("\"bgr\""));
    assert!(template.contains("opaque"));
    assert!(template.contains("Windows"));
}

/// The starter template explains the explicit TERM_PROGRAM compatibility override.
#[test]
fn default_template_documents_term_program_compatibility_override() {
    let template = default_config_template();
    assert!(template.contains("term_program = \"SonicTerm\""));
    assert!(template.contains("Some tools, such as Copilot"));
    assert!(template.contains("setting term_program = \"WezTerm\""));
    assert!(template.contains("enable their WezTerm/new terminal UI path"));
}

/// The generated config lists both width keys, each under its own comment with its default,
/// and parses back to the same limits.
#[test]
fn default_template_documents_both_tab_width_limits() {
    let template = default_config_template();
    assert!(template.contains("tab_min_width = 240"), "{template}");
    assert!(template.contains("tab_max_width = 320"), "{template}");
    assert!(template.contains("Default 240.\ntab_min_width = 240"));
    assert!(template.contains("Default 320.\ntab_max_width = 320"));
    let parsed: Config = toml::from_str(&template).unwrap();
    assert_eq!((parsed.tab_min_width, parsed.tab_max_width), (240.0, 320.0));
}
