use sonicterm_io::pty::ShellSpawnOpts;

#[test]
fn exports_pty_spawn_options() {
    let opts = ShellSpawnOpts::default();
    assert!(!opts.clean_e2e);
}
