use super::*;

/// Reply labels follow Ghostty for one letter (`c`, `s`, `p` echoed, anything else `c`) and kitty
/// for several: one reply per destination, clipboard `c` before PRIMARY `p`.
#[test]
fn label_matrix() {
    let cases: [(&str, &[u8]); 12] = [
        ("c", b"c"),
        ("s", b"s"),
        ("p", b"p"),
        ("", b"c"),
        ("0", b"c"),
        ("x", b"c"),
        ("cp", b"cp"),
        ("pc", b"cp"),
        ("sp", b"cp"),
        ("pp", b"p"),
        ("cs", b"c"),
        ("xy", b"c"),
    ];
    for (kinds, labels) in cases {
        assert_eq!(reply_labels(kinds.as_bytes()), labels, "{kinds}");
    }
}

/// Every reply is an empty OSC 52 answer terminated by ST, even for a BEL-terminated request,
/// so a reply never carries clipboard bytes.
#[test]
fn empty_replies_use_st_and_carry_no_data() {
    assert_eq!(empty_replies(b"c"), b"\x1b]52;c;\x1b\\");
    assert_eq!(empty_replies(b"cp"), b"\x1b]52;c;\x1b\\\x1b]52;p;\x1b\\");
    assert_eq!(empty_replies(b"s"), b"\x1b]52;s;\x1b\\");
    // The batch splitter relies on every answer having exactly this length.
    assert_eq!(empty_replies(b"p").len(), EMPTY_REPLY_BYTES);
}

/// A `p` read is answered with one empty `p` reply on every platform: no PRIMARY contents are
/// ever read, so Linux and the platforms without PRIMARY answer alike.
#[test]
fn p_primary_answered_empty_on_every_platform() {
    assert_eq!(empty_replies(b"p"), b"\x1b]52;p;\x1b\\");
}
