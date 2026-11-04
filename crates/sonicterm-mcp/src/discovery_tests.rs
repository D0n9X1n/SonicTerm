//! Pins discovery order and its error text: an argument beats the environment, which beats the
//! directory; zero or several live endpoints is an error that says what to do.

use super::*;

/// A live candidate at `address` owned by `pid`.
fn candidate(address: &str, pid: u32) -> Candidate {
    Candidate { address: address.to_owned(), pid }
}

/// `--endpoint` wins over `SONICTERM_CONTROL`, which wins over the directory; empty values are
/// ignored.
#[test]
fn the_order_is_argument_environment_directory() {
    let found = || vec![candidate("/run/sonicterm/9.sock", 9)];
    let chosen = choose(Some("arg".into()), Some("env".into()), found(), None).unwrap();
    assert_eq!((chosen.address.as_str(), chosen.source), ("arg", Source::Argument));
    let chosen = choose(Some(String::new()), Some("env".into()), found(), None).unwrap();
    assert_eq!((chosen.address.as_str(), chosen.source), ("env", Source::Environment));
    let chosen = choose(None, Some(String::new()), found(), None).unwrap();
    assert_eq!(
        (chosen.address.as_str(), chosen.source),
        ("/run/sonicterm/9.sock", Source::Directory)
    );
}

/// No live endpoint says the server is probably off and names the directory searched; several
/// list every address and ask for `--endpoint`.
#[test]
fn zero_or_several_endpoints_explain_themselves() {
    let none = choose(None, None, Vec::new(), Some("/run/sonicterm".into())).unwrap_err();
    let text = none.to_string();
    assert!(text.contains("/run/sonicterm") && text.contains("[mcp] enabled = true"), "{text}");
    let several =
        choose(None, None, vec![candidate("a.sock", 1), candidate("b.sock", 2)], None).unwrap_err();
    let text = several.to_string();
    assert!(
        text.contains("--endpoint") && text.contains("a.sock") && text.contains("b.sock"),
        "{text}"
    );
}

/// Directory entries name their pid as `<pid>.sock` or `<pid>`; dead pids, records with no
/// listener and other files are skipped, and the result is ordered by pid.
#[test]
fn the_directory_scan_keeps_live_endpoints_only() {
    let root = tempfile::tempdir().unwrap();
    for name in ["30.sock", "10.sock", "20", "notes.txt", "x.sock"] {
        std::fs::write(root.path().join(name), name).unwrap();
    }
    let address = |path: &Path| Some(path.file_name()?.to_str()?.to_owned());
    let found = scan(root.path(), address, |pid| pid != 30, |_| true);
    assert_eq!(found, vec![candidate("10.sock", 10), candidate("20", 20)]);
    let stale = scan(root.path(), address, |_| true, |address| address != "10.sock");
    assert_eq!(
        stale,
        vec![candidate("20", 20), candidate("30.sock", 30)],
        "no listener, no candidate"
    );
    assert_eq!(entry_pid("42.sock"), Some(42));
    assert_eq!(entry_pid("42"), Some(42));
    assert_eq!(entry_pid("abc"), None);
    assert!(scan(&root.path().join("missing"), address, |_| true, |_| true).is_empty());
}
