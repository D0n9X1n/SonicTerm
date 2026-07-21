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
