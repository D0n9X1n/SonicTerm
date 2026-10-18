use super::*;
use std::os::unix::fs::PermissionsExt;

/// The on-disk bytes of a little-endian 64-bit Mach-O header (`MH_MAGIC_64`).
const THIN_MACH_O_BYTES: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];

/// A scratch directory and its canonical path (macOS temp dirs sit behind `/var` -> `/private/var`).
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    (dir, canonical)
}

/// Write `bytes` to `path` with permission `mode`.
fn write_file(path: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Classify `path` as a document open would.
fn classify_path(path: &Path) -> Disposition {
    classify(OpenScriptRequest::resolve(path.to_path_buf(), Path::new("/")).unwrap())
}

/// The refusal and fallback directory of a refused disposition.
fn refused(disposition: Disposition) -> (Refusal, Option<PathBuf>) {
    match disposition {
        Disposition::Refuse { refusal, cwd, .. } => (refusal, cwd),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Mark `path` quarantined the way a browser download does.
fn quarantine(path: &Path) {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(QUARANTINE_ATTRIBUTE).unwrap();
    let value = b"0081;00000000;Safari;";
    let (path_ptr, name_ptr, value_ptr) = (c_path.as_ptr(), name.as_ptr(), value.as_ptr().cast());
    let status =
        // SAFETY: both C strings are NUL-terminated and outlive the call; setxattr only reads `value`.
        unsafe { libc::setxattr(path_ptr, name_ptr, value_ptr, value.len(), 0, 0) };
    assert_eq!(status, 0, "setxattr: {}", std::io::Error::last_os_error());
}

/// A valid, executable, unquarantined Mach-O runs at its canonical path, in its own directory.
#[test]
fn an_executable_mach_o_runs_in_its_canonical_parent() {
    let (_dir, root) = scratch();
    let tool = root.join("tool");
    write_file(&tool, THIN_MACH_O_BYTES, 0o755);
    let Disposition::Run { program, cwd, identity } = classify_path(&tool) else {
        panic!("a verified Mach-O must run");
    };
    assert_eq!(program, tool);
    assert_eq!(cwd, root);
    let metadata = std::fs::metadata(&tool).unwrap();
    assert_eq!(identity, FileIdentity { device: metadata.dev(), inode: metadata.ino() });
}

/// A fat Mach-O with a small architecture count is native; the running test binary is one too.
#[test]
fn fat_and_real_binaries_are_recognised_as_mach_o() {
    assert!(is_mach_o(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]));
    assert!(is_mach_o(&[0xca, 0xfe, 0xba, 0xbf, 0, 0, 0, 1]));
    let test_binary = std::env::current_exe().unwrap();
    let header = read_header(&File::open(test_binary).unwrap()).unwrap();
    assert!(is_mach_o(&header), "the test binary is Mach-O: {header:02x?}");
}

/// A Java class file shares the fat magic but its version field is no architecture count.
#[test]
fn a_java_class_file_is_not_mach_o_and_never_runs() {
    let (_dir, root) = scratch();
    let class_file = root.join("Main");
    // Magic, minor version 0, major version 52 (Java 8): read as a count, 52 exceeds the bound.
    write_file(&class_file, &[0xca, 0xfe, 0xba, 0xbe, 0x00, 0x00, 0x00, 0x34], 0o755);
    assert!(matches!(classify_path(&class_file), Disposition::Draft(_)));
    assert!(!is_mach_o(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 0]), "zero architectures");
    assert!(!is_mach_o(&[0xca, 0xfe, 0xba, 0xbe]), "a fat header without its count");
}

/// A Mach-O without an execute bit is refused with a warning, and no bit is added.
#[test]
fn a_non_executable_mach_o_is_refused() {
    let (_dir, root) = scratch();
    let tool = root.join("tool");
    write_file(&tool, THIN_MACH_O_BYTES, 0o644);
    assert_eq!(refused(classify_path(&tool)), (Refusal::NotExecutable, Some(root)));
    assert_eq!(std::fs::metadata(&tool).unwrap().permissions().mode() & 0o777, 0o644);
}

/// A quarantined executable Mach-O never runs; its attribute stays and a shell opens beside it.
#[test]
fn a_quarantined_mach_o_is_refused_and_keeps_its_quarantine() {
    let (_dir, root) = scratch();
    let tool = root.join("tool");
    write_file(&tool, THIN_MACH_O_BYTES, 0o755);
    quarantine(&tool);
    assert_eq!(refused(classify_path(&tool)), (Refusal::Quarantined, Some(root)));
    assert!(
        quarantine_present(&File::open(&tool).unwrap()).unwrap(),
        "quarantine is never removed"
    );
    assert_eq!(
        Refusal::Quarantined.notice("tool"),
        "Not run: tool is quarantined (downloaded). Run it yourself after review."
    );
}

/// An executable `#!` script without an extension is drafted (today's warning path), never run.
#[test]
fn an_extensionless_executable_script_is_never_run() {
    let (_dir, root) = scratch();
    let script = root.join("deploy");
    write_file(&script, b"#!/bin/sh\ntouch marker\n", 0o755);
    let Disposition::Draft(request) = classify_path(&script) else {
        panic!("a script must take the draft path");
    };
    assert_eq!(request.launch_path, script, "the draft keeps the path as opened");
}

/// A `.sh` file is a script even when its bytes are Mach-O, so it is drafted, never executed.
#[test]
fn a_script_extension_wins_over_mach_o_bytes() {
    let (_dir, root) = scratch();
    let script = root.join("build.sh");
    write_file(&script, b"echo hi\n", 0o755);
    assert!(matches!(classify_path(&script), Disposition::Draft(_)));
    let disguised = root.join("payload.command");
    write_file(&disguised, THIN_MACH_O_BYTES, 0o755);
    assert!(matches!(classify_path(&disguised), Disposition::Draft(_)));
}

/// A broken symlink fails closed in home, with no draft and nothing run.
#[test]
fn a_broken_link_fails_closed_in_home() {
    let (_dir, root) = scratch();
    let link = root.join("dangling");
    std::os::unix::fs::symlink(root.join("missing"), &link).unwrap();
    assert_eq!(refused(classify_path(&link)), (Refusal::Unresolvable, None));
}

/// An application bundle, a link to one, and a binary inside one never run or open inside it.
#[test]
fn bundles_and_their_contents_never_run() {
    let (_dir, root) = scratch();
    let bundle = root.join("Foo.APP");
    let macos_dir = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&macos_dir).unwrap();
    let inner = macos_dir.join("Foo");
    write_file(&inner, THIN_MACH_O_BYTES, 0o755);
    let link = root.join("shortcut");
    std::os::unix::fs::symlink(&bundle, &link).unwrap();

    for path in [&bundle, &link, &inner] {
        assert_eq!(
            refused(classify_path(path)),
            (Refusal::Bundle, Some(root.clone())),
            "{}",
            path.display()
        );
    }
}

/// A folder opens a shell tab beside it and nothing runs.
#[test]
fn a_directory_is_refused() {
    let (_dir, root) = scratch();
    let folder = root.join("folder");
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(refused(classify_path(&folder)), (Refusal::Directory, Some(root)));
}

/// A FIFO is not a regular file; classification neither blocks on it nor runs it.
#[test]
fn a_fifo_is_refused_without_blocking() {
    let (_dir, root) = scratch();
    let fifo = root.join("pipe");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    let status =
        // SAFETY: `c_path` is a NUL-terminated C string that outlives the call; mkfifo only reads it.
        unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(status, 0);
    assert_eq!(refused(classify_path(&fifo)), (Refusal::NotRegular, Some(root)));
}

/// Refusal reasons are stable and every notice names only the file name it is given.
#[test]
fn refusal_reasons_and_notices_are_stable() {
    assert_eq!(Refusal::NotExecutable.notice("tool"), "Not run: tool is not executable.");
    for refusal in [
        Refusal::Unresolvable,
        Refusal::Bundle,
        Refusal::Directory,
        Refusal::NotRegular,
        Refusal::Unreadable,
        Refusal::Quarantined,
        Refusal::NotExecutable,
    ] {
        assert!(!refusal.as_str().contains('/'));
        assert!(refusal.notice("NAME").contains("NAME"));
    }
}
