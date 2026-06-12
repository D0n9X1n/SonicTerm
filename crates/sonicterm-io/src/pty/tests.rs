
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
