use super::*;

fn env_str<'a>(builder: &'a CommandBuilder, name: &str) -> &'a str {
    builder.get_env(name).and_then(|v| v.to_str()).unwrap()
}

#[test]
fn default_shell_spawn_opts_keep_sonicterm_term_program() {
    let opts = ShellSpawnOpts::default();
    assert_eq!(opts.term_program, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);
}

#[test]
fn child_pty_env_uses_configured_term_program() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, "WezTerm");

    assert_eq!(env_str(&builder, "TERM"), "xterm-256color");
    assert_eq!(env_str(&builder, "COLORTERM"), "truecolor");
    assert_eq!(env_str(&builder, "TERM_PROGRAM"), "WezTerm");
    assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), env!("CARGO_PKG_VERSION"));
}

#[cfg(target_os = "windows")]
#[test]
fn windowsapps_filter_skips_user_alias_but_allows_store_package() {
    assert!(is_windowsapps_alias_stub_path(
        "c:\\users\\dotan\\appdata\\local\\microsoft\\windowsapps\\pwsh.exe"
    ));
    assert!(!is_windowsapps_alias_stub_path(
        "c:\\program files\\windowsapps\\microsoft.powershell_7.6.2.0_x64__8wekyb3d8bbwe\\pwsh.exe"
    ));
}

#[cfg(target_os = "windows")]
#[test]
fn powershell_interactive_args_force_utf8_codepage() {
    let args = interactive_shell_args("pwsh.exe");
    assert!(args.iter().any(|a| a == "-NoLogo"));
    assert!(args.iter().any(|a| a == "-NoExit"));
    let command = args.last().expect("command arg present");
    assert!(command.contains("InputEncoding"));
    assert!(command.contains("OutputEncoding"));
    assert!(command.contains("chcp 65001"));
}

#[test]
fn shell_spawn_opts_default_has_no_shell_override() {
    assert_eq!(ShellSpawnOpts::default().shell, None);
}

#[test]
fn resolve_spawn_shell_prefers_nonempty_override() {
    // An explicit, non-empty override wins verbatim (trimmed).
    assert_eq!(resolve_spawn_shell(Some("powershell.exe")), "powershell.exe");
    assert_eq!(resolve_spawn_shell(Some("  pwsh.exe  ")), "pwsh.exe");
    // None / empty / whitespace fall back to auto-detect (= default_shell()).
    assert_eq!(resolve_spawn_shell(None), default_shell());
    assert_eq!(resolve_spawn_shell(Some("")), default_shell());
    assert_eq!(resolve_spawn_shell(Some("   ")), default_shell());
}

#[cfg(target_os = "windows")]
#[test]
fn store_pkg_version_parses_dir_name() {
    use std::path::PathBuf;
    let p = PathBuf::from(
        r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\pwsh.exe",
    );
    assert_eq!(store_pkg_version(&p), [7, 6, 2, 0]);
    // Unparseable / missing version sorts to all-zero.
    let q = PathBuf::from(r"C:\nope\pwsh.exe");
    assert_eq!(store_pkg_version(&q), [0, 0, 0, 0]);
}

#[cfg(target_os = "windows")]
#[test]
fn pick_highest_pwsh_chooses_newest_version() {
    use std::path::PathBuf;
    let wa = r"C:\Program Files\WindowsApps";
    let candidates = vec![
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.4.0.0_x64__8wekyb3d8bbwe\pwsh.exe")),
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\pwsh.exe")),
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.10.0.0_x64__8wekyb3d8bbwe\pwsh.exe")),
    ];
    let picked = pick_highest_pwsh(&candidates).expect("a candidate");
    // 7.10 must beat 7.6 (numeric, not lexical).
    assert!(picked.to_string_lossy().contains("7.10.0.0"));
    // Empty list -> None.
    assert_eq!(pick_highest_pwsh(&[]), None);
}

#[cfg(target_os = "macos")]
#[test]
fn production_macos_zsh_starts_as_login_shell() {
    assert_eq!(shell_startup_args("/bin/zsh", ShellSpawnOpts::default()), vec!["-l"]);
}

#[cfg(target_os = "macos")]
#[test]
fn production_macos_bash_and_fish_start_as_login_shells() {
    assert_eq!(shell_startup_args("/bin/bash", ShellSpawnOpts::default()), vec!["--login"]);
    assert_eq!(
        shell_startup_args("/opt/homebrew/bin/fish", ShellSpawnOpts::default()),
        vec!["--login"]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn clean_e2e_keeps_profile_suppression() {
    let opts = ShellSpawnOpts { clean_e2e: true, ..ShellSpawnOpts::default() };
    assert_eq!(shell_startup_args("/bin/zsh", opts), vec!["-f"]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn production_non_macos_shell_startup_stays_unchanged() {
    assert!(shell_startup_args("/bin/zsh", ShellSpawnOpts::default()).is_empty());
}

#[test]
fn applies_utf8_locale_when_launch_environment_has_no_locale() {
    assert!(should_apply_utf8_locale_fallback(None, None, None));
    assert_eq!(default_lang_utf8_locale(), "en_US.UTF-8");
    assert_eq!(default_lc_ctype_utf8_locale(), "UTF-8");
}

#[test]
fn skips_fallback_when_effective_locale_is_already_utf8() {
    assert!(!should_apply_utf8_locale_fallback(None, Some("UTF-8"), None));
    assert!(!should_apply_utf8_locale_fallback(None, None, Some("zh_CN.UTF-8")));
    assert!(!should_apply_utf8_locale_fallback(None, None, Some("en_US.UTF8")));
}

#[test]
fn fills_lc_ctype_when_lang_is_present_but_not_utf8() {
    assert!(should_apply_utf8_locale_fallback(None, None, Some("C")));
}

#[test]
fn preserves_explicit_lc_all_override() {
    assert!(!should_apply_utf8_locale_fallback(Some("C"), None, None));
}
