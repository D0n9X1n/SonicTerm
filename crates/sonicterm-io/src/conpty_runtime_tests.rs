use super::*;

/// Every production spawn admits the runtime before portable-pty can initialize its process-wide loader.
#[test]
fn runtime_admission_precedes_native_pty_open() {
    let source = include_str!("pty_spawn.rs");
    let body = source.split_once("    fn spawn_built(").unwrap().1;
    let admission = body.find("crate::conpty_runtime::ensure_ready()?").expect("runtime admission");
    let open = body.find("native_pty_system()").unwrap();
    assert!(admission < open, "runtime validation must precede native library discovery");
}

/// SHA-256 uses the system provider and matches known empty and short-message digests.
#[test]
fn system_sha256_matches_known_vectors() {
    assert_eq!(
        sha256(b"").unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc").unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Development absence is distinct from a malformed present path; neither case executes a DLL.
#[test]
fn runtime_directory_distinguishes_absence_and_invalid_presence() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("conpty");
    assert!(open_directory(&runtime).unwrap().is_none());
    std::fs::write(&runtime, b"not a directory").unwrap();
    assert!(open_directory(&runtime).is_err());
    std::fs::remove_file(&runtime).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let held = open_directory(&runtime).unwrap().unwrap();
    // A retained directory must not disappear while the pinned DLL can still resolve its companion.
    assert!(std::fs::remove_dir(&runtime).is_err());
    drop(held);
    std::fs::remove_dir(&runtime).unwrap();
}

/// Size and digest pins reject altered files before any load, and successful admission denies replacement.
#[test]
fn runtime_member_is_pinned_and_retained_against_replacement() {
    let root = tempfile::tempdir().unwrap();
    let member = root.path().join("fixture.bin");
    std::fs::write(&member, b"abc").unwrap();
    let digest = sha256(b"abc").unwrap();
    assert!(verify_file(&member, 4, &digest).is_err());
    assert!(verify_file(&member, 3, &sha256(b"abd").unwrap()).is_err());
    let held = verify_file(&member, 3, &digest).unwrap();
    assert!(OpenOptions::new().write(true).open(&member).is_err());
    assert!(std::fs::remove_file(&member).is_err());
    drop(held);
    std::fs::remove_file(&member).unwrap();
}

/// A directory cannot stand in for a pinned binary even when the requested size would otherwise fit.
#[test]
fn runtime_member_refuses_directory() {
    let root = tempfile::tempdir().unwrap();
    assert!(verify_file(root.path(), 0, &sha256(b"").unwrap()).is_err());
}

/// A system module path, canonicalized the way the loader compares paths.
fn system_module(name: &str) -> PathBuf {
    let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
    PathBuf::from(system_root).join("System32").join(name).canonicalize().unwrap()
}

/// A module already loaded under the runtime's name must be the verified file; any other file is refused,
/// because portable-pty's bare-name load would reuse it. kernel32.dll stands in as an always-loaded module.
#[test]
fn already_loaded_module_must_be_the_verified_file() {
    let loaded = HSTRING::from("kernel32.dll");
    let foreign = ensure_no_foreign_module(&loaded, Path::new(r"C:\not\the\verified\conpty.dll"));
    assert!(
        format!("{:#}", foreign.unwrap_err()).contains("a different conpty.dll is already loaded")
    );
    ensure_no_foreign_module(&loaded, &system_module("kernel32.dll")).unwrap();
    // A name with no loaded module leaves the exact-path load to decide.
    let absent = HSTRING::from("sonicterm-no-such-module.dll");
    ensure_no_foreign_module(&absent, Path::new(r"C:\unused.dll")).unwrap();
}

/// After loading, the module must sit at the verified path and export every pseudoconsole API, or the
/// upstream loader would silently fall back to kernel32. ntdll.dll is loaded but exports none of them.
#[test]
fn loaded_module_path_and_exports_are_enforced() {
    let kernel32 =
        // SAFETY: the name is a live NUL-terminated wide string and kernel32 stays loaded for the process.
        unsafe { GetModuleHandleW(&HSTRING::from("kernel32.dll")) }.unwrap();
    let ntdll =
        // SAFETY: the name is a live NUL-terminated wide string and ntdll stays loaded for the process.
        unsafe { GetModuleHandleW(&HSTRING::from("ntdll.dll")) }.unwrap();
    let wrong_path = validate_module(kernel32, Path::new(r"C:\not\the\verified\conpty.dll"));
    assert!(format!("{:#}", wrong_path.unwrap_err()).contains("differs from verified path"));
    let no_exports = validate_module(ntdll, &system_module("ntdll.dll"));
    assert!(format!("{:#}", no_exports.unwrap_err()).contains("missing a required API export"));
    validate_module(kernel32, &system_module("kernel32.dll")).unwrap();
}

/// A junction in place of the runtime folder is refused before anything inside it is opened.
#[test]
fn runtime_directory_refuses_junction() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    let runtime = root.path().join("conpty");
    let created = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&runtime)
        .arg(&target)
        .output()
        .unwrap();
    assert!(created.status.success(), "mklink /J needs no privilege: {created:?}");
    let refused = open_directory(&runtime);
    assert!(format!("{:#}", refused.unwrap_err()).contains("reparse point"));
}

/// A symbolic link in place of a pinned binary is refused even when its target has the pinned bytes.
#[test]
fn runtime_member_refuses_symbolic_link() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("real.bin");
    std::fs::write(&target, b"abc").unwrap();
    let link = root.path().join("conpty.dll");
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
        // When: symlink_file fails, only ERROR_PRIVILEGE_NOT_HELD (1314) may skip; the junction test covers reparse refusal.
        assert_eq!(error.raw_os_error(), Some(1314), "symlink setup failed: {error}");
        eprintln!("skipped: symbolic link privilege not held");
        return;
    }
    let refused = verify_file(&link, 3, &sha256(b"abc").unwrap());
    assert!(format!("{:#}", refused.unwrap_err()).contains("reparse point"));
}
