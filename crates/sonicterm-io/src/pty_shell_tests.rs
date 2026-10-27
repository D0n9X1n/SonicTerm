use super::*;
#[cfg(windows)]
use crate::pty::pty_test_support::lock_live_pty_test;

fn env_str<'a>(builder: &'a CommandBuilder, name: &str) -> &'a str {
    builder.get_env(name).and_then(|v| v.to_str()).unwrap()
}

#[test]
fn child_pty_env_uses_configured_term_program() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, "WezTerm");

    assert_eq!(env_str(&builder, "TERM"), "xterm-256color");
    assert_eq!(env_str(&builder, "COLORTERM"), "truecolor");
    assert_eq!(env_str(&builder, "TERM_PROGRAM"), "WezTerm");
    // Not SonicTerm's own version. See
    // `an_advertised_name_carries_a_version_that_names_gates_accept`.
    assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), "20230712-072601");
}

#[test]
fn running_as_ourselves_advertises_our_own_version() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);

    assert_eq!(env_str(&builder, "TERM_PROGRAM"), "SonicTerm");
    assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), env!("CARGO_PKG_VERSION"));
}

#[test]
fn an_advertised_name_carries_a_version_that_names_gates_accept() {
    // A name/version pair has to be internally consistent to be useful:
    // programs gate features on the version *of the terminal the name
    // claims*. WezTerm versions by datestamp and consumers compare those
    // lexically, so a semver string sorts below every datestamp — the tool
    // takes its WezTerm branch on the name, then disables the features it
    // just decided the terminal was too old for.
    //
    // Asserting the pair rather than either field: checking only that the
    // version is non-empty, or only that the name is passed through, would
    // both pass against the mismatch this exists to prevent.
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, "WezTerm");
    let advertised = env_str(&builder, "TERM_PROGRAM_VERSION");

    // The threshold editors test for styled-underline support.
    const CONSUMER_GATE: &str = "20210203-095643";
    assert!(
        advertised > CONSUMER_GATE,
        "advertised WezTerm version {advertised:?} sorts below the gate \
         {CONSUMER_GATE:?}, so capability checks against the claimed identity fail"
    );

    // And SonicTerm's own version would NOT have cleared it, which is the
    // defect this replaced.
    assert!(
        env!("CARGO_PKG_VERSION") < CONSUMER_GATE,
        "this test is vacuous if our own version already cleared the gate"
    );
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

/// Interactive PowerShell installs process-local file display links without changing clean test startup.
#[cfg(target_os = "windows")]
#[test]
fn powershell_links_are_embedded_only_in_interactive_startup() {
    let normal = shell_startup_args("pwsh.exe", ShellSpawnOpts::default());
    let command = normal.last().unwrap();
    assert!(command.contains("Format-SonicTermFileItem"));
    assert!(command.contains("$uri.AbsoluteUri"));
    assert!(!command.contains("Set-Alias"));
    let clean = shell_startup_args(
        "pwsh.exe",
        ShellSpawnOpts { clean_e2e: true, ..ShellSpawnOpts::default() },
    );
    assert!(!clean.iter().any(|arg| arg.contains("Format-SonicTermFileItem")));
}

/// The normal PTY path runs linked-directory regression without profile edits and reaps its shell on a deadline.
#[cfg(target_os = "windows")]
#[test]
fn powershell_link_view_executes_through_pty() {
    let _live_pty_guard = lock_live_pty_test();
    let shell = resolve_windows_default_shell_with(
        || path_lookup("pwsh.exe"),
        registered_pwsh,
        windowsapps_store_pwsh,
        || None,
    );
    assert_eq!(
        shell_file_name(&shell),
        "pwsh.exe",
        "PowerShell 7 is required for linked directory verification"
    );
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/test-powershell-integration.ps1");
    let command = format!("& '{}'", script.to_string_lossy().replace('\'', "''"));
    let pty = PtyHandle::spawn_with_args(
        &shell,
        &["-NoLogo".into(), "-NoProfile".into(), "-Command".into(), command],
        120,
        30,
    )
    .unwrap();
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).unwrap();
    let probe = pty.child_exit_probe();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        while let Ok(chunk) = pty.out_rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        if probe.has_exited().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe.has_exited().unwrap(), "PowerShell link checks timed out");
    while let Ok(chunk) = pty.out_rx.try_recv() {
        output.extend_from_slice(&chunk);
    }
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("PowerShell file links: PASS"), "{output}");
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
fn resolve_spawn_shell_prefers_nonempty_override() {
    // An explicit, non-empty override wins verbatim (trimmed).
    assert_eq!(resolve_spawn_shell(Some("powershell.exe")), "powershell.exe");
    assert_eq!(resolve_spawn_shell(Some("  pwsh.exe  ")), "pwsh.exe");
    // None / empty / whitespace fall back to auto-detect (= default_shell()).
    assert_eq!(resolve_spawn_shell(None), default_shell());
    assert_eq!(resolve_spawn_shell(Some("")), default_shell());
    assert_eq!(resolve_spawn_shell(Some("   ")), default_shell());
}

/// Unix shell selection accepts only executable candidates and preserves priority.
#[cfg(unix)]
#[test]
fn unix_default_shell_prefers_valid_environment_then_passwd_then_sh() {
    let executable =
        |path: &Path| matches!(path.to_str(), Some("/env/sh" | "/passwd/sh" | "/bin/sh"));

    assert_eq!(
        resolve_unix_default_shell_with(Some("/env/sh"), Some("/passwd/sh"), executable),
        "/env/sh"
    );
    assert_eq!(
        resolve_unix_default_shell_with(Some("/missing"), Some("/passwd/sh"), executable),
        "/passwd/sh"
    );
    assert_eq!(
        resolve_unix_default_shell_with(Some("  "), Some("/missing"), executable),
        "/bin/sh"
    );
}

/// Unix shell selection rejects a non-executable file rather than trusting its pathname.
#[cfg(unix)]
#[test]
fn unix_default_shell_rejects_non_executable_candidates() {
    let executable = |path: &Path| path == Path::new("/bin/sh");

    assert_eq!(
        resolve_unix_default_shell_with(
            Some("/tmp/plain-file"),
            Some("/tmp/passwd-file"),
            executable
        ),
        "/bin/sh"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_default_shell_prefers_registered_pwsh_before_store_and_legacy() {
    let chosen = resolve_windows_default_shell_with(
        || None,
        || Some("registered-pwsh.exe".into()),
        || Some("store-pwsh.exe".into()),
        || Some("powershell.exe".into()),
    );
    assert_eq!(chosen, "registered-pwsh.exe");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_default_shell_priority_and_fallback_are_stable() {
    assert_eq!(
        resolve_windows_default_shell_with(
            || Some("path-pwsh.exe".into()),
            || Some("registered-pwsh.exe".into()),
            || Some("store-pwsh.exe".into()),
            || Some("powershell.exe".into()),
        ),
        "path-pwsh.exe"
    );
    assert_eq!(resolve_windows_default_shell_with(|| None, || None, || None, || None), "cmd.exe");
}

#[cfg(target_os = "windows")]
#[test]
fn registered_pwsh_resolves_to_a_real_powershell_7_when_present() {
    let Some(path) = registered_pwsh() else { return };
    assert!(std::path::Path::new(&path).is_file());
    assert_eq!(shell_file_name(&path), "pwsh.exe");
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

#[test]
fn clean_e2e_disables_cmd_autorun() {
    // Protect Windows smoke from registry AutoRun commands while preserving ordinary cmd startup.
    let opts = ShellSpawnOpts { clean_e2e: true, ..ShellSpawnOpts::default() };
    assert_eq!(shell_startup_args("cmd.exe", opts), vec!["/D"]);
    assert!(shell_startup_args("cmd.exe", ShellSpawnOpts::default()).is_empty());
}

#[test]
fn clean_e2e_removes_only_shell_hook_environment() {
    // Protect deterministic POSIX smoke startup without replacing HOME or unrelated environment.
    let mut builder = CommandBuilder::new("/bin/sh");
    builder.env("ENV", "/tmp/env-hook");
    builder.env("BASH_ENV", "/tmp/bash-hook");
    builder.env("HOME", "/home/preserved");
    apply_clean_e2e_environment(&mut builder, true);
    assert!(builder.get_env("ENV").is_none());
    assert!(builder.get_env("BASH_ENV").is_none());
    assert_eq!(builder.get_env("HOME").and_then(|value| value.to_str()), Some("/home/preserved"));

    let mut ordinary = CommandBuilder::new("/bin/sh");
    ordinary.env("ENV", "/tmp/env-hook");
    ordinary.env("BASH_ENV", "/tmp/bash-hook");
    apply_clean_e2e_environment(&mut ordinary, false);
    assert_eq!(ordinary.get_env("ENV").and_then(|value| value.to_str()), Some("/tmp/env-hook"));
    assert_eq!(
        ordinary.get_env("BASH_ENV").and_then(|value| value.to_str()),
        Some("/tmp/bash-hook")
    );
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
