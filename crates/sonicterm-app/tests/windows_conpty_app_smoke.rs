#![cfg(target_os = "windows")]
//! F-ConPTY app rows: a child behind a real pseudoconsole sends each query, and the host answers
//! through a SonicTerm pane running the production VT publisher
//! (`App::__test_clipboard_reply_pane`), so
//! the child reports the exact reply bytes a pane's reply path wrote back through ConPTY. Every
//! row's expected reply is a literal pinned here ([`QUERY_REPLIES`] for the color, pointer and
//! program-status queries, then [`OSC52_READ_REPLY`], [`KITTY_READ_REPLY`], [`KITTY_WRITE_DONE`] and
//! [`DECRQM_5522_REPLY`]), never recomputed by another pane, so a pane that stopped answering fails
//! its row instead of matching an empty expectation. Every pane settles clipboard writes against a
//! fake clipboard and reads
//! clipboard text under `osc52_read = "allow"` from one holding [`CLIPBOARD_FIXTURE`] beside fixed
//! HTML, PNG and custom-type representations, never the user's clipboard.
//!
//! Opt-in: `cargo test --locked -p sonicterm-io -p sonicterm-app --test windows_conpty_protocol_forms
//! --test windows_conpty_app_smoke -- --ignored --nocapture`.

#![warn(clippy::min_ident_chars)]

use sonicterm_app::app::{App, TestReplyPane};
use sonicterm_io::conpty_fixture::{run, Case, Engine, InputPhase, PTY_COLS, PTY_ROWS};

/// The synthetic text every app-smoke pane's fake clipboard holds.
const CLIPBOARD_FIXTURE: &str = "fixture";

/// The exact reply to `52;cp;?` under `allow`: `c` carries the fixture's Base64 and `p` is empty,
/// as Windows has no PRIMARY selection; both are ST-terminated though the query ended with BEL.
const OSC52_READ_REPLY: &[u8] = b"\x1b]52;c;Zml4dHVyZQ==\x1b\\\x1b]52;p;\x1b\\";

/// The name of the clipboard-read row, whose expectation is [`OSC52_READ_REPLY`].
const OSC52_READ_CASE: &str = "osc52-read";

/// An OSC 5522 write of the fixture as `text/plain`: open, one chunk, then the BEL commit.
const KITTY_WRITE: &[u8] = b"\x1b]5522;type=write:id=fixture\x07\
\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\\
\x1b]5522;type=wdata\x07";

/// The exact status [`KITTY_WRITE`] earns under `allow`: `DONE` with the opening id, terminated as
/// the commit was.
const KITTY_WRITE_DONE: &[u8] = b"\x1b]5522;type=write:status=DONE:id=fixture\x07";

/// The name of the OSC 5522 write row, whose expectation is [`KITTY_WRITE_DONE`].
const KITTY_WRITE_CASE: &str = "osc5522-write-done";

/// The exact reply to an OSC 5522 `text/plain` read under `allow`: OK, one DATA packet repeating
/// the id and MIME with the fixture's Base64, then DONE, each ST-terminated as the request was.
const KITTY_READ_REPLY: &[u8] = b"\x1b]5522;type=read:status=OK:id=r\x1b\\\
\x1b]5522;type=read:status=DATA:id=r:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\\
\x1b]5522;type=read:status=DONE:id=r\x1b\\";

/// The name of the OSC 5522 read row, whose expectation is [`KITTY_READ_REPLY`].
const KITTY_READ_CASE: &str = "osc5522-read";

/// DECSET 5522 then DECRQM `?5522`: the pane advertises paste events once the program sets them.
const DECRQM_5522: &str = "\x1b[?5522h\x1b[?5522$p";

/// The exact reply to [`DECRQM_5522`]: mode 5522 is set (`1`).
const DECRQM_5522_REPLY: &[u8] = b"\x1b[?5522;1$y";

/// An OSC 5522 write of `text/html`, `image/png`, a custom MIME type and `text/plain`; Windows
/// holds every one, so the commit posts one offer and the write is `DONE`.
const KITTY_MULTI_WRITE: &[u8] = b"\x1b]5522;type=write:id=multi\x07\
\x1b]5522;type=wdata:mime=dGV4dC9odG1s;PGI+Zml4dHVyZTwvYj4=\x1b\\\
\x1b]5522;type=wdata:mime=aW1hZ2UvcG5n;UE5H\x1b\\\
\x1b]5522;type=wdata:mime=YXBwbGljYXRpb24veC1zb25pY3Rlcm0tdGVzdA==;Y3VzdG9t\x1b\\\
\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\\
\x1b]5522;type=wdata\x07";

/// The exact reply to [`KITTY_MULTI_WRITE`].
const KITTY_MULTI_WRITE_DONE: &[u8] = b"\x1b]5522;type=write:status=DONE:id=multi\x07";

/// The name of the multi-type OSC 5522 write row.
const KITTY_MULTI_WRITE_CASE: &str = "osc5522-write-multi";

/// An OSC 5522 read of `text/html`, `image/png`, a custom MIME type and `text/plain`.
const KITTY_MULTI_READ: &str = "\x1b]5522;type=read:id=m;\
dGV4dC9odG1sIGltYWdlL3BuZyBhcHBsaWNhdGlvbi94LXNvbmljdGVybS10ZXN0IHRleHQvcGxhaW4=\x1b\\";

/// The exact reply to [`KITTY_MULTI_READ`]: every requested type the fake clipboard holds, in
/// request order: `text/html`, `image/png`, the custom type and `text/plain`.
const KITTY_MULTI_READ_REPLY: &[u8] = b"\x1b]5522;type=read:status=OK:id=m\x1b\\\
\x1b]5522;type=read:status=DATA:id=m:mime=dGV4dC9odG1s;PGI+Zml4dHVyZTwvYj4=\x1b\\\
\x1b]5522;type=read:status=DATA:id=m:mime=aW1hZ2UvcG5n;UE5H\x1b\\\
\x1b]5522;type=read:status=DATA:id=m:mime=YXBwbGljYXRpb24veC1zb25pY3Rlcm0tdGVzdA==;Y3VzdG9t\x1b\\\
\x1b]5522;type=read:status=DATA:id=m:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\\
\x1b]5522;type=read:status=DONE:id=m\x1b\\";

/// The name of the multi-type OSC 5522 read row.
const KITTY_MULTI_READ_CASE: &str = "osc5522-read-multi";

/// The name of the DECRQM 5522 row, whose expectation is [`DECRQM_5522_REPLY`].
const DECRQM_5522_CASE: &str = "decrqm-5522";

/// Each query row and its exact reply under the default theme: palette entry 1, the foreground
/// and background, OSC 21's foreground with an unknown key named in unpadded Base64, the pointer
/// shape query (`1` supported, `0` not), the program-status capability list, and the theme's
/// selection background through OSC 17 (16-bit) and OSC 21 (8-bit).
const QUERY_REPLIES: [(&str, &str, &[u8]); 7] = [
    ("osc4-query", "\x1b]4;1;?\x1b\\", b"\x1b]4;1;rgb:fbfb/4949/3434\x1b\\"),
    (
        "osc10-query-chain",
        "\x1b]10;?;?\x07",
        b"\x1b]10;rgb:cfcf/bcbc/9797\x07\x1b]11;rgb:1414/1616/1717\x07",
    ),
    (
        "osc21-query",
        "\x1b]21;foreground=?;nonsense=?\x1b\\",
        b"\x1b]21;foreground=rgb:cf/bc/97;unknown=bm9uc2Vuc2U\x1b\\",
    ),
    ("osc22-query", "\x1b]22;?pointer,nonsense\x07", b"\x1b]22;1,0\x07"),
    (
        "osc7501-query",
        "\x1b]7501;?\x07",
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07",
    ),
    ("osc17-query", "\x1b]17;?\x07", b"\x1b]17;rgb:3c3c/3838/3636\x07"),
    (
        "osc21-selection-query",
        "\x1b]21;selection_background=?\x1b\\",
        b"\x1b]21;selection_background=rgb:3c/38/36\x1b\\",
    ),
];

/// A reply pane whose clipboard reads answer from the fake clipboard.
fn clipboard_pane() -> TestReplyPane {
    App::__test_clipboard_reply_pane(PTY_COLS, PTY_ROWS, CLIPBOARD_FIXTURE)
}

/// The live pane the child talks to; each case's expectation is a pinned literal.
struct AppEngine {
    live: TestReplyPane,
}

impl Engine for AppEngine {
    fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.live.replies(bytes)
    }

    fn reference_reply(&mut self, case: &Case) -> Vec<u8> {
        if case.name == OSC52_READ_CASE {
            // When: the case is the clipboard read, its exact bytes are pinned, not recomputed.
            return OSC52_READ_REPLY.to_vec();
        }
        if case.name == KITTY_WRITE_CASE {
            // When: the case is the OSC 5522 write, `DONE` is pinned, so no other status passes.
            return KITTY_WRITE_DONE.to_vec();
        }
        if case.name == KITTY_READ_CASE {
            // When: the case is the OSC 5522 read, its OK/DATA/DONE bytes are pinned.
            return KITTY_READ_REPLY.to_vec();
        }
        if case.name == KITTY_MULTI_WRITE_CASE {
            // When: the case is the multi-type OSC 5522 write, `DONE` is pinned.
            return KITTY_MULTI_WRITE_DONE.to_vec();
        }
        if case.name == KITTY_MULTI_READ_CASE {
            // When: the case is the multi-type OSC 5522 read, its every-type reply is pinned.
            return KITTY_MULTI_READ_REPLY.to_vec();
        }
        if case.name == DECRQM_5522_CASE {
            // When: the case is the DECRQM 5522 query, the advertised `1` is pinned.
            return DECRQM_5522_REPLY.to_vec();
        }
        match QUERY_REPLIES.iter().find(|(name, _, _)| *name == case.name) {
            Some((_, _, reply)) => reply.to_vec(),
            // When: no reply is pinned, the row is the positive control, which expects no reply.
            None => Vec::new(),
        }
    }
}

/// Reply rows: each query's exact reply must reach the child. Queries change no pane state, so
/// the reference pane's answer is the live pane's. Synthetic strings only.
fn cases() -> Vec<Case> {
    let reply = || InputPhase::Echo(Vec::new());
    vec![
        Case::new("control", "CONTROL-OK")
            .input(InputPhase::Echo(b"ping".to_vec()))
            .output_required()
            .input_required(),
        Case::new(QUERY_REPLIES[0].0, QUERY_REPLIES[0].1).input(reply()).input_required(),
        Case::new(QUERY_REPLIES[1].0, QUERY_REPLIES[1].1).input(reply()).input_required(),
        Case::new(QUERY_REPLIES[2].0, QUERY_REPLIES[2].1).input(reply()).input_required(),
        Case::new(QUERY_REPLIES[3].0, QUERY_REPLIES[3].1).input(reply()).input_required(),
        Case::new(QUERY_REPLIES[4].0, QUERY_REPLIES[4].1).input(reply()).input_required(),
        Case::new(QUERY_REPLIES[5].0, QUERY_REPLIES[5].1).input(reply()).input_required(),
        // Recorded: kitty's OSC 21 selection key, the only terminal with it.
        Case::new(QUERY_REPLIES[6].0, QUERY_REPLIES[6].1).input(reply()),
        Case::new(OSC52_READ_CASE, "\x1b]52;cp;?\x07").input(reply()).input_required(),
        Case::new(KITTY_WRITE_CASE, KITTY_WRITE).input(reply()).input_required(),
        Case::new(KITTY_READ_CASE, "\x1b]5522;type=read:id=r;dGV4dC9wbGFpbg==\x1b\\")
            .input(reply())
            .input_required(),
        Case::new(KITTY_MULTI_WRITE_CASE, KITTY_MULTI_WRITE).input(reply()).input_required(),
        Case::new(KITTY_MULTI_READ_CASE, KITTY_MULTI_READ).input(reply()).input_required(),
        // The trailer resets the mode, so the live pane's later rows paste as before.
        Case::new(DECRQM_5522_CASE, DECRQM_5522)
            .input(reply())
            .input_required()
            .trailer("\x1b[?5522l"),
    ]
}

/// Contract: a pane's replies to color, pointer, program-status and clipboard-read queries reach
/// a program behind a real pseudoconsole byte for byte on the packaged runtime, the clipboard
/// read under `allow` as the exact fake-clipboard reply, an OSC 5522 write as exactly
/// `type=write:status=DONE`, an OSC 5522 read as its exact OK/DATA/DONE sequence and DECRQM 5522 as
/// set once DECSET 5522 ran; the positive control is exact on both runtimes.
#[test]
#[ignore = "needs a staged ConPTY runtime; run with --ignored (the windows-conpty-forms gate step)"]
fn windows_conpty_app_smoke() {
    run(
        "windows_conpty_app_smoke",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        &cases(),
        || AppEngine { live: clipboard_pane() },
    );
}
