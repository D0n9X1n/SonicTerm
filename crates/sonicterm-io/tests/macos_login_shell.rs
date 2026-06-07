use sonicterm_io::pty::{shell_startup_args, ShellSpawnOpts};

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
    let opts = ShellSpawnOpts { clean_e2e: true };
    assert_eq!(shell_startup_args("/bin/zsh", opts), vec!["-f"]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn production_non_macos_shell_startup_stays_unchanged() {
    assert!(shell_startup_args("/bin/zsh", ShellSpawnOpts::default()).is_empty());
}
