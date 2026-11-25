#![cfg(target_os = "windows")]
//! F-ConPTY transport rows: each protocol form a SonicTerm pane already handles, printed by a
//! child behind a real pseudoconsole, packaged and system runtimes alike. The host only answers
//! the console's own queries and injects canned replies, so these rows prove forwarding, not
//! behavior; app behavior is proved by unit tests and `windows_conpty_app_smoke`.
//!
//! Opt-in: `cargo test --locked -p sonicterm-io -p sonicterm-app --test windows_conpty_protocol_forms
//! --test windows_conpty_app_smoke -- --ignored --nocapture`.

#![warn(clippy::min_ident_chars)]

use sonicterm_io::conpty_fixture::{run, Case, InputPhase, TransportEngine};

/// Every row, the positive control first and the resize row last, so its repaint cannot reach a
/// later case. Synthetic strings only.
fn cases() -> Vec<Case> {
    let echo = |bytes: &[u8]| InputPhase::Echo(bytes.to_vec());
    let mouse_off = "\x1b[?1000l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l";
    let palette: String =
        (0..=255).map(|index| format!("{index}=#{index:02x}{index:02x}{index:02x};")).collect();
    vec![
        Case::new("control", "CONTROL-OK").input(echo(b"ping")).output_required().input_required(),
        Case::new("osc4-query", "\x1b]4;1;?\x1b\\")
            .input(echo(b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\"))
            .output_required()
            .input_required(),
        Case::new("osc10-query-chain", "\x1b]10;?;?\x07")
            .input(echo(b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b]11;rgb:0000/0000/0000\x07"))
            .output_required()
            .input_required(),
        Case::new("osc4-set", "\x1b]4;1;#ff0000;2;rgb:00/ff/00\x07"),
        Case::new("osc104-reset", "\x1b]104\x07"),
        Case::new("osc104-list", "\x1b]104;1;3\x07"),
        Case::new("osc11-12-set", "\x1b]11;#101010\x07\x1b]12;#e0e0e0\x07"),
        Case::new("osc110-arg", "\x1b]110;x\x07"),
        Case::new("osc111-119-reset", "\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07"),
        Case::new("osc110-query", "\x1b]110;?\x07"),
        Case::new("osc17-19-set", "\x1b]17;#334455\x07\x1b]19;#ddeeff\x07"),
        Case::new("osc17-query", "\x1b]17;?\x07").output_required(),
        Case::new("osc21-selection-query", "\x1b]21;selection_background=?\x1b\\"),
        Case::new("osc21-query", "\x1b]21;foreground=?;nonsense=?\x1b\\")
            .input(echo(b"\x1b]21;foreground=rgb:ff/ff/ff;nonsense=\x1b\\"))
            .output_required()
            .input_required(),
        Case::new("osc21-palette", format!("\x1b]21;{}\x1b\\", palette.trim_end_matches(';'))),
        Case::new("osc22-set", "\x1b]22;pointer\x07"),
        Case::new("osc22-query", "\x1b]22;?pointer,nonsense\x07")
            .input(echo(b"\x1b]22;1,0\x07"))
            .output_required()
            .input_required(),
        Case::new("decset1005", "\x1b[?1005h")
            .preamble("\x1b[?1000h")
            .input(echo("\x1b[M\u{20}\u{a0}\u{a0}".as_bytes()))
            .trailer(mouse_off),
        Case::new("decset1006", "\x1b[?1006h").preamble("\x1b[?1000h").trailer(mouse_off),
        Case::new("decset1015", "\x1b[?1015h")
            .preamble("\x1b[?1000h")
            .input(echo(b"\x1b[32;5;5M"))
            .trailer(mouse_off),
        Case::new("decset1016", "\x1b[?1016h")
            .preamble("\x1b[?1000h\x1b[?1006h")
            .input(echo(b"\x1b[<0;12;34M"))
            .input_required()
            .trailer(mouse_off),
        Case::new("osc1-icon-title", "\x1b]1;fixture tab\x07").output_required(),
        Case::new("osc1337-set-user-var", "\x1b]1337;SetUserVar=fixture=dmFsdWU=\x07")
            .output_required(),
        Case::new("osc1337-current-dir", "\x1b]1337;CurrentDir=file://localhost/C:/fixture\x07"),
        // OSC 52 writes and OSC 1337 `Copy=:`.
        Case::new("osc52-c", "\x1b]52;c;Zml4dHVyZQ==\x07").output_required(),
        Case::new("osc52-cp", "\x1b]52;cp;Zml4dHVyZQ==\x1b\\").output_required(),
        Case::new("osc52-bare-c", "\x1b]52;c\x07").output_required(),
        Case::new("osc52-read", "\x1b]52;c;?\x07")
            .input(echo(b"\x1b]52;c;\x1b\\"))
            .output_required()
            .input_required(),
        Case::new("osc1337-copy", "\x1b]1337;Copy=:Zml4dHVyZQ==\x07").output_required(),
        // OSC 5522 write forms: open, one text/plain chunk, commit.
        Case::new("osc5522-write", "\x1b]5522;type=write:id=fixture\x07").output_required(),
        Case::new("osc5522-wdata-text", "\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\")
            .output_required(),
        Case::new("osc5522-commit", "\x1b]5522;type=wdata\x07").output_required(),
        // OSC 5522 read: the request reaches the host, and an injected OK/DATA/DONE reaches the child.
        Case::new("osc5522-read", "\x1b]5522;type=read:id=r;dGV4dC9wbGFpbg==\x1b\\")
            .input(echo(
                b"\x1b]5522;type=read:status=OK:id=r\x1b\\\
\x1b]5522;type=read:status=DATA:id=r:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\\
\x1b]5522;type=read:status=DONE:id=r\x1b\\",
            ))
            .output_required(),
        // DECSET 5522 paste event: the mode reaches the host, and an injected event (OK, the `.`
        // listing of `text/plain`, DONE, each with a one-time `pw`) reaches the child.
        Case::new("decset5522-paste-event", "\x1b[?5522h")
            .input(echo(
                b"\x1b]5522;type=read:status=OK:pw=b3Rw\x1b\\\
\x1b]5522;type=read:status=DATA:mime=Lg==:pw=b3Rw;dGV4dC9wbGFpbgo=\x1b\\\
\x1b]5522;type=read:status=DONE:pw=b3Rw\x1b\\",
            ))
            .trailer("\x1b[?5522l"),
        // The follow-up read naming itself and echoing the one-time `pw` reaches the host, and an
        // injected OK/DATA/DONE reaches the child.
        // Its own id and payload, so a reply carried from the read above cannot match this row.
        Case::new(
            "osc5522-read-paste-otp",
            "\x1b]5522;type=read:id=p:name=UGFzdGUgZXZlbnQ=:pw=b3Rw;dGV4dC9wbGFpbg==\x1b\\",
        )
        .input(echo(
            b"\x1b]5522;type=read:status=OK:id=p\x1b\\\
\x1b]5522;type=read:status=DATA:id=p:mime=dGV4dC9wbGFpbg==;cGFzdGVk\x1b\\\
\x1b]5522;type=read:status=DONE:id=p\x1b\\",
        ))
        .output_required(),
        Case::new("osc7501-report", "\x1b]7501;app=fixture:state=working:progress=40\x07"),
        Case::new("osc7501-clear", "\x1b]7501;state=clear\x07"),
        Case::new("osc7501-query", "\x1b]7501;?\x07")
            .input(echo(b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07"))
            .output_required()
            .input_required(),
        Case::new("osc9-9-quoted", "\x1b]9;9;\"C:\\fixture\"\x1b\\").output_required(),
        Case::new("osc7-localhost-backslash", "\x1b]7;file://localhost/C:\\fixture\x1b\\"),
        Case::new("osc7-host-forward", "\x1b]7;file://FIXTURE/C:/fixture\x1b\\"),
        Case::new("osc7-kitty-shell-cwd", "\x1b]7;kitty-shell-cwd://localhost/C:/fixture\x1b\\"),
        Case::new(
            "osc133-annotations",
            "\x1b]133;L\x07\x1b]133;N\x07\x1b]133;P;k=c\x07\x1b]133;I\x07\
             \x1b]133;A;redraw=last;click_events=2\x07\x1b]133;C;cmdline_url=ls%20-l\x07\x1b]9;12\x07",
        )
        .output_required(),
        // A prompt click's input: the SGR press, an arrow key and the `special_key` CSI-u.
        Case::new("osc133-click-events", "\x1b]133;A;click_events=1\x07")
            .input(echo(b"\x1b[<0;5;1M\x1b[D\x1b[1;1u"))
            .input_required(),
        Case::new("osc9-4-lenient", "\x1b]9;4;1\x07\x1b]9;4;2;abc\x07\x1b]9;4;0\x07"),
        Case::new("osc777-notify", "\x1b]777;notify;Fixture\x07"),
        Case::new("prompt-resize", "\x1b]133;A;redraw=last\x07")
            .input(InputPhase::Resize { total_rows: 60, rows: 20 }),
        // Recorded, not required, and last, so its time cannot crowd out a required row.
        Case::new("osc52-c-4mib", format!("\x1b]52;c;{}\x07", "QUFB".repeat(1024 * 1024))),
    ]
}

/// Contract: every form above crosses a real pseudoconsole on both runtimes; the positive control
/// is exact on both, every required row is exact on the packaged runtime, and every other row's
/// outcome is printed as evidence for the wiki's per-runtime Windows column.
#[test]
#[ignore = "needs a staged ConPTY runtime; run with --ignored (the windows-conpty-forms gate step)"]
fn windows_conpty_protocol_forms() {
    run(
        "windows_conpty_protocol_forms",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        &cases(),
        TransportEngine::default,
    );
}
