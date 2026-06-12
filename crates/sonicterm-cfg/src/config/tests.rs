
use super::*;

#[test]
fn default_terminal_term_program_is_sonicterm() {
    let cfg = Config::default();
    assert_eq!(cfg.terminal.term_program, "SonicTerm");
}

#[test]
fn parses_terminal_term_program_override() {
    let cfg: Config = toml::from_str(
        r#"
[terminal]
term_program = "WezTerm"
"#,
    )
    .unwrap();
    assert_eq!(cfg.terminal.term_program, "WezTerm");
    assert_eq!(cfg.terminal.scrollback, TerminalConfig::default().scrollback);
}

#[test]
fn default_template_documents_term_program_compatibility_override() {
    let template = default_config_template();
    assert!(template.contains("term_program = \"SonicTerm\""));
    assert!(template.contains("Some tools, such as Copilot"));
    assert!(template.contains("setting term_program = \"WezTerm\""));
    assert!(template.contains("enable their WezTerm/new terminal UI path"));
}

#[test]
fn default_config_paths_live_under_dot_sonicterm() {
    let dir = default_config_dir().expect("home dir should exist in tests");
    assert!(dir.ends_with(".sonicterm"));
    assert_eq!(Config::default_path().unwrap(), dir.join("sonicterm.toml"));
}

#[test]
fn seeding_user_examples_writes_theme_and_platform_keymaps() {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-config-seed-{}-{}",
        std::process::id(),
        nonce
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed_user_examples(&dir).unwrap();
    assert!(dir.join("themes/wezterm.toml").exists());
    assert!(dir.join("keymaps/sonicterm-macos.toml").exists());
    assert!(dir.join("keymaps/sonicterm-windows.toml").exists());
    assert!(dir.join("keymaps/sonicterm-linux.toml").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
