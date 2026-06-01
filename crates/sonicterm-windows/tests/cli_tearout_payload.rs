#[path = "../src/cli.rs"]
mod cli;

use sonicterm_app::os_drag::TabPayload;

fn sample_payload() -> TabPayload {
    TabPayload {
        pty_pid: 249,
        tab_title: "PowerShell".to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"PS C:\\> git status\n"),
        cwd: "C:\\src\\sonic".to_string(),
        cmd: "pwsh.exe".to_string(),
        env: vec![("TERM".to_string(), "xterm-256color".to_string())],
    }
}

#[test]
fn parses_tearout_payload_cli_arg() {
    let payload = sample_payload();
    let json = payload.to_json().expect("encode payload");

    let parsed = cli::parse_tearout_payload_from([
        "sonicterm-windows.exe".to_string(),
        "--tear-out-payload".to_string(),
        json,
    ])
    .expect("parse args")
    .expect("payload present");

    assert_eq!(parsed, payload);
}

#[test]
fn returns_none_when_flag_absent() {
    assert!(cli::parse_tearout_payload_from(["sonicterm-windows.exe"])
        .expect("parse args")
        .is_none());
}
