use super::*;

#[test]
fn normalizes_posix_and_windows_paths_to_the_same_key() {
    assert_eq!(normalize_proc_name("/usr/local/bin/Claude"), "claude");
    assert_eq!(normalize_proc_name(r"C:\Program Files\Claude\Claude.EXE"), "claude");
}

#[test]
fn strips_one_login_prefix_from_the_basename() {
    assert_eq!(normalize_proc_name("/bin/-ZSH"), "zsh");
    assert_eq!(normalize_proc_name("--zsh"), "-zsh");
}

#[test]
fn strips_one_case_insensitive_exe_suffix() {
    assert_eq!(normalize_proc_name("PowerShell.EXE"), "powershell");
    assert_eq!(normalize_proc_name("tool.exe.exe"), "tool.exe");
}

#[test]
fn accepts_basename_only_and_preserves_non_exe_suffixes() {
    assert_eq!(normalize_proc_name("NVIM"), "nvim");
    assert_eq!(normalize_proc_name("python3.12"), "python3.12");
    assert_eq!(normalize_proc_name("archive.com"), "archive.com");
}

#[test]
fn dotted_path_components_do_not_affect_basename_suffix_handling() {
    assert_eq!(normalize_proc_name("/opt/tools.v2/bin/Node.EXE"), "node");
    assert_eq!(normalize_proc_name(r"C:\tools.v2\bin\Node.EXE"), "node");
}

#[test]
fn normalization_does_not_parse_arguments() {
    assert_eq!(normalize_proc_name(r"C:\bin\node.exe --version"), "node.exe --version");
}

/// Run `walk` on another thread and fail after 5 s instead of hanging the suite on a cycle.
fn within_deadline<T: Send + 'static>(walk: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(walk());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the process-tree walk did not finish: it looped on a cycle")
}

#[test]
fn deepest_descendant_follows_the_longest_chain() {
    // Protect the ordinary choice: the deepest descendant of the shell, not a shallower sibling.
    let links = [(20, 10), (30, 20), (40, 10), (50, 30)];
    assert_eq!(deepest_descendant(10, &links), 50);
    assert_eq!(deepest_descendant(99, &links), 99);
}

#[test]
fn deepest_descendant_ends_on_self_parent_and_cycles() {
    // Protect the macOS probe from looping forever when a non-atomic process snapshot links pids
    // in a cycle, including a pid listed as its own parent; each pid is visited at most once.
    let self_parent = within_deadline(|| deepest_descendant(10, &[(20, 10), (20, 20)]));
    assert_eq!(self_parent, 20);
    let two_cycle = within_deadline(|| deepest_descendant(10, &[(20, 10), (30, 20), (20, 30)]));
    assert_eq!(two_cycle, 30);
    let three_cycle =
        within_deadline(|| deepest_descendant(10, &[(20, 10), (30, 20), (40, 30), (20, 40)]));
    assert_eq!(three_cycle, 40);
    let root_cycle = within_deadline(|| deepest_descendant(10, &[(20, 10), (10, 20)]));
    assert_eq!(root_cycle, 20);
}
