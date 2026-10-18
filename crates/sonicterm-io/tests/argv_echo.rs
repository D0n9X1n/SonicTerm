//! Exact-argv check for direct command launches.
//!
//! This binary is its own child: run with `--argv-echo-child OUT`, it writes
//! its cwd and then each later argument as one hex line (raw bytes on Unix,
//! UTF-16 code units on Windows) to `OUT`. Run plainly, it spawns itself
//! through `PtyHandle::spawn_command` and compares those lines exactly, so the
//! assertion never depends on shell or console rendering.

#![warn(clippy::min_ident_chars)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sonicterm_io::pty::{CommandSpawn, PtyHandle, ShellSpawnOpts};

const CHILD_FLAG: &str = "--argv-echo-child";

/// One argument as a hex line in the platform's native unit.
fn hex_line(arg: &std::ffi::OsStr) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        arg.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        arg.encode_wide().map(|unit| format!("{unit:04x}")).collect()
    }
}

/// Child mode: record the cwd and every argument after the output path.
fn run_child(args: &[OsString]) {
    let out = PathBuf::from(&args[0]);
    let cwd = std::env::current_dir().expect("child cwd");
    let mut text = hex_line(cwd.as_os_str());
    text.push('\n');
    for arg in &args[1..] {
        text.push_str(&hex_line(arg));
        text.push('\n');
    }
    // Write then rename, so the parent never reads a partial file.
    let partial = out.with_extension("partial");
    std::fs::write(&partial, text).expect("write argv record");
    std::fs::rename(&partial, &out).expect("publish argv record");
}

/// The arguments whose exact delivery the launch contract promises.
fn tricky_args() -> Vec<OsString> {
    let mut args: Vec<OsString> =
        ["a b", "\"quoted\"", "back\\slash\\", "", "-T", "é漢字", "trail\\\"quote", "--hold"]
            .iter()
            .map(OsString::from)
            .collect();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        args.push(OsString::from_vec(b"raw-\xff\xfe".to_vec()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        args.push(OsString::from_wide(&[0x0061, 0xD800, 0x0062]));
    }
    args
}

/// Parent mode: spawn this binary through `spawn_command` and compare its record.
fn run_parent() {
    let root = tempfile::tempdir().expect("tempdir");
    let out = root.path().join("argv.txt");
    let expected = tricky_args();
    let mut args = vec![OsString::from(CHILD_FLAG), out.clone().into_os_string()];
    args.extend(expected.iter().cloned());
    let spawn = CommandSpawn {
        program: std::env::current_exe().expect("current exe").into_os_string(),
        args,
        cwd: root.path().to_path_buf(),
    };
    let pty = PtyHandle::spawn_command(&spawn, 120, 30, &ShellSpawnOpts::default())
        .expect("spawn argv echo child");
    #[cfg(windows)]
    {
        // ConPTY's inherited-cursor query must receive a position reply before the child runs.
        let _ = pty.send_input_nonblocking(b"\x1b[1;1R".to_vec());
    }
    let record = wait_for_record(&pty, &out);
    drop(pty);
    let mut lines = record.lines();
    let cwd_line = lines.next().expect("cwd line");
    let canonical_root = std::fs::canonicalize(root.path()).expect("canonical root");
    let canonical_child = decode_path(cwd_line);
    assert_eq!(
        std::fs::canonicalize(canonical_child).expect("canonical child cwd"),
        canonical_root,
        "the child must start in the requested cwd"
    );
    let received: Vec<&str> = lines.collect();
    let wanted: Vec<String> = expected.iter().map(|arg| hex_line(arg)).collect();
    assert_eq!(received, wanted, "the child must receive every argument exactly");
    println!("argv_echo: {} arguments delivered exactly", wanted.len());
}

/// Poll for the child's record while draining PTY output, within a fixed bound.
fn wait_for_record(pty: &PtyHandle, out: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        while pty.out_rx.try_recv().is_ok() {}
        if let Ok(text) = std::fs::read_to_string(out) {
            // When: the record exists, the child finished its atomic rename.
            return text;
        }
        assert!(Instant::now() < deadline, "the argv echo child wrote no record in time");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Decode a hex cwd line back into a path.
fn decode_path(line: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bytes: Vec<u8> = (0..line.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&line[index..index + 2], 16).expect("hex byte"))
            .collect();
        PathBuf::from(OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = (0..line.len())
            .step_by(4)
            .map(|index| u16::from_str_radix(&line[index..index + 4], 16).expect("hex unit"))
            .collect();
        PathBuf::from(OsString::from_wide(&units))
    }
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|first| first == CHILD_FLAG) {
        // When: the first argument is the child flag, this process is the spawned echo child.
        run_child(&args[1..]);
        return;
    }
    run_parent();
}
