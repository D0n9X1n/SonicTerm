#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

use sonicterm_io::pty::PtyHandle;
use sonicterm_types::shell_quote_posix;

fn unique_marker(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sonicterm-{name}-{}", std::process::id()))
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    path.exists()
}

#[test]
fn draft_remains_unsubmitted_until_enter() {
    let marker = unique_marker("draft-marker");
    let _ = std::fs::remove_file(&marker);
    let pty = PtyHandle::spawn_with_args("/bin/sh", &[], 80, 24).expect("spawn interactive sh");
    let draft = format!("touch {}", shell_quote_posix(marker.to_str().unwrap()));

    pty.send_input_nonblocking(draft.into_bytes()).expect("queue draft");
    assert!(
        !wait_for_path(&marker, Duration::from_millis(250)),
        "draft produced its side effect before Enter"
    );

    pty.send_input_nonblocking(b"\r".to_vec()).expect("submit draft");
    assert!(wait_for_path(&marker, Duration::from_secs(2)), "draft did not run after Enter");

    drop(pty);
    std::fs::remove_file(marker).unwrap();
}

#[test]
fn hostile_reader_can_execute_draft_without_enter() {
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let marker = unique_marker("hostile-startup-marker");
    let ready = unique_marker("hostile-startup-ready");
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&ready);
    let draft = format!("touch {}", shell_quote_posix(marker.to_str().unwrap()));
    let script = format!(
        "stty -icanon min 1 time 0; touch {}; command=$(dd bs=1 count={} 2>/dev/null); eval \"$command\"",
        shell_quote_posix(ready.to_str().unwrap()),
        draft.len()
    );
    let args = vec!["--noprofile".to_string(), "--norc".to_string(), "-c".to_string(), script];
    let pty = PtyHandle::spawn_with_args("/bin/bash", &args, 80, 24).expect("spawn hostile reader");
    assert!(wait_for_path(&ready, Duration::from_secs(2)), "hostile reader never became ready");

    pty.send_input_nonblocking(draft.into_bytes()).expect("queue draft");
    assert!(
        wait_for_path(&marker, Duration::from_secs(2)),
        "fixture must pin that startup code can consume and execute an unterminated draft"
    );

    drop(pty);
    std::fs::remove_file(marker).unwrap();
    let _ = std::fs::remove_file(ready);
}
