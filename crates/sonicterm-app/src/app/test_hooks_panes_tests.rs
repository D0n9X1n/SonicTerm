use super::*;

/// The reply pane returns exactly the bytes the production VT publisher hands a pane's reply
/// path, and plain output returns none, so the F-ConPTY app smoke's expected replies come from
/// the code a live pane runs. The OSC 7501 answer is fixed, which also pins the canned reply the
/// transport row injects.
#[test]
fn reply_pane_returns_the_production_reply_bytes() {
    let mut pane = App::__test_reply_pane(80, 24);
    assert!(pane.replies(b"plain text\r\n").is_empty());
    assert_eq!(pane.replies(b"\x1b[6n"), b"\x1b[2;1R".to_vec());
    let palette = pane.replies(b"\x1b]4;1;?\x1b\\");
    assert!(palette.starts_with(b"\x1b]4;1;rgb:") && palette.ends_with(b"\x1b\\"), "{palette:?}");
    assert_eq!(
        pane.replies(b"\x1b]7501;?\x07"),
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x07"
            .to_vec()
    );
}

/// Two fresh reply panes answer the same query identically, so a reference pane can stand for
/// the live pane's answer to a query that changes no state.
#[test]
fn fresh_reply_panes_answer_identically() {
    let query = b"\x1b]10;?;?\x07\x1b]21;foreground=?\x1b\\";
    let first = App::__test_reply_pane(80, 24).replies(query);
    assert!(!first.is_empty());
    assert_eq!(App::__test_reply_pane(80, 24).replies(query), first);
}

/// Contract: the F-ConPTY app smoke's clipboard pane answers a read under `allow` from its fake
/// clipboard with exact ST-terminated bytes, `c` carrying the text and `p` empty except on Linux,
/// so the smoke's expected reply is the bytes this pins.
#[test]
fn clipboard_reply_pane_answers_reads_from_its_fake_clipboard() {
    let mut pane = App::__test_clipboard_reply_pane(80, 24, "fixture");
    assert_eq!(pane.replies(b"\x1b]52;c;?\x07"), b"\x1b]52;c;Zml4dHVyZQ==\x1b\\".to_vec());
    let primary = if cfg!(target_os = "linux") { "Zml4dHVyZQ==" } else { "" };
    let expected = format!("\x1b]52;c;Zml4dHVyZQ==\x1b\\\x1b]52;p;{primary}\x1b\\");
    assert_eq!(pane.replies(b"\x1b]52;cp;?\x1b\\"), expected.into_bytes());
    // OSC 52 prints nothing, so the cursor is still home.
    assert_eq!(pane.replies(b"\x1b[6n"), b"\x1b[1;1R".to_vec(), "other replies are unchanged");
}

/// Contract: the F-ConPTY app smoke's clipboard pane settles an OSC 5522 write under `allow`
/// against its fake clipboard and answers exactly `type=write:status=DONE` with the opening id and
/// the commit's terminator, whether the packets arrive in one batch or one per batch, so the
/// smoke's expected reply is the bytes this pins.
#[test]
fn clipboard_reply_pane_answers_a_kitty_write_done() {
    let write = b"\x1b]5522;type=write:id=fixture\x07";
    let data = b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;Zml4dHVyZQ==\x1b\\";
    let commit = b"\x1b]5522;type=wdata\x07";
    let done = b"\x1b]5522;type=write:status=DONE:id=fixture\x07".to_vec();
    let mut pane = App::__test_clipboard_reply_pane(80, 24, "fixture");
    assert!(pane.replies(write).is_empty());
    assert!(pane.replies(data).is_empty());
    assert_eq!(pane.replies(commit), done);
    let whole = [&write[..], &data[..], &commit[..]].concat();
    assert_eq!(App::__test_clipboard_reply_pane(80, 24, "fixture").replies(&whole), done);
}
