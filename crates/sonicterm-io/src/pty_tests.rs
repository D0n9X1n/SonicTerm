use super::*;

#[test]
fn default_shell_spawn_opts_keep_sonicterm_term_program() {
    let opts = ShellSpawnOpts::default();
    assert_eq!(opts.term_program, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);
}

#[test]
fn shell_spawn_opts_default_has_no_shell_or_cwd_override() {
    let opts = ShellSpawnOpts::default();
    assert_eq!(opts.shell, None);
    assert_eq!(opts.cwd, None);
}
