use super::*;
use crate::{shell_quote_posix, shell_quote_powershell, ShellDialect};
use std::path::PathBuf;

fn target(dialect: ShellDialect, bracketed: bool) -> PasteTarget {
    PasteTarget { bracketed, dialect }
}

fn paths(values: &[&str]) -> UserPayload {
    UserPayload::Paths(values.iter().map(PathBuf::from).collect())
}

fn assert_encoding(payload: &UserPayload, target: PasteTarget, expected: &[u8]) {
    let needed = encoded_len(payload, target).expect("valid payload length");
    let encoded = crate::encode_payload(payload, target, expected.len()).expect("encode payload");
    assert_eq!(encoded, expected);
    assert_eq!(needed, encoded.len(), "the preflight length must equal the emitted bytes");
    if !expected.is_empty() {
        assert_eq!(
            crate::encode_payload(payload, target, expected.len() - 1),
            Err(PasteRefusal::TooLarge { needed: expected.len() })
        );
    }
}

#[test]
fn text_preserves_wrap_paste_bytes_with_and_without_brackets() {
    // Text is untouched, including control bytes and empty input; only DECSET 2004 adds guards.
    for (text, wrapped) in [
        ("", "\x1b[200~\x1b[201~"),
        ("hello", "\x1b[200~hello\x1b[201~"),
        ("a\r\nb\n", "\x1b[200~a\r\nb\n\x1b[201~"),
        ("日本 ü\t\0\x1b[201~", "\x1b[200~日本 ü\t\0\x1b[201~\x1b[201~"),
    ] {
        for dialect in [
            ShellDialect::Posix,
            ShellDialect::PowerShell,
            ShellDialect::Cmd,
            ShellDialect::Unknown,
        ] {
            let payload = UserPayload::Text(text.to_owned());
            assert_encoding(&payload, target(dialect, false), text.as_bytes());
            assert_encoding(&payload, target(dialect, true), wrapped.as_bytes());
        }
    }
}

#[test]
fn powershell_paths_double_ascii_and_typographic_single_quotes() {
    // A quote inside a path remains one literal character after PowerShell parses the argument.
    for (path, expected) in [
        (r"C:\O'Brien\a.txt", r"'C:\O''Brien\a.txt'"),
        (r"C:\O’Brien\a.txt", r"'C:\O’’Brien\a.txt'"),
        (r"C:\‘a’\b.txt", r"'C:\‘‘a’’\b.txt'"),
        (r"C:\‚b‛\c.txt", r"'C:\‚‚b‛‛\c.txt'"),
        (r#"C:\“x”\"„.txt"#, r#"'C:\“x”\"„.txt'"#),
    ] {
        assert_encoding(
            &paths(&[path]),
            target(ShellDialect::PowerShell, false),
            expected.as_bytes(),
        );
    }
}

#[test]
fn cmd_paths_use_double_quotes_and_refuse_expansion_characters() {
    // cmd receives a spaced path as one argument; its quote and expansion characters fail closed.
    assert_encoding(
        &paths(&[r"C:\My Files\a.txt"]),
        target(ShellDialect::Cmd, false),
        br#""C:\My Files\a.txt""#,
    );
    for path in [r#"C:\a"b.txt"#, r"C:\%TEMP%\a.txt", r"C:\wow!\a.txt"] {
        assert_eq!(
            crate::encode_payload(&paths(&[path]), target(ShellDialect::Cmd, false), usize::MAX),
            Err(PasteRefusal::CmdUnsafeCharacter)
        );
    }
}

#[test]
fn unknown_paths_match_posix_quoting() {
    // An unknown shell uses the existing POSIX contract, including embedded apostrophes.
    let payload = paths(&["/tmp/it's/a.txt", "/tmp/a&b/c.txt", ""]);
    let expected = b"'/tmp/it'\\''s/a.txt' '/tmp/a&b/c.txt' ''";
    assert_encoding(&payload, target(ShellDialect::Posix, false), expected);
    assert_encoding(&payload, target(ShellDialect::Unknown, false), expected);
}

#[test]
fn paths_join_with_one_space_and_wrap_as_one_paste() {
    // The complete Unicode path list gets one pair of paste guards, not one pair per argument.
    let payload = paths(&["日本 ü/a.txt", "my files/b.txt"]);
    for dialect in [ShellDialect::Posix, ShellDialect::PowerShell, ShellDialect::Unknown] {
        assert_encoding(
            &payload,
            target(dialect, false),
            "'日本 ü/a.txt' 'my files/b.txt'".as_bytes(),
        );
        assert_encoding(
            &payload,
            target(dialect, true),
            "\x1b[200~'日本 ü/a.txt' 'my files/b.txt'\x1b[201~".as_bytes(),
        );
    }
    assert_encoding(
        &payload,
        target(ShellDialect::Cmd, true),
        "\x1b[200~\"日本 ü/a.txt\" \"my files/b.txt\"\x1b[201~".as_bytes(),
    );
}

#[test]
fn empty_path_list_emits_no_bytes_even_when_bracketed() {
    // No dropped paths means no input, whereas empty Text still follows wrap_paste's guard rule.
    for bracketed in [false, true] {
        for dialect in [
            ShellDialect::Posix,
            ShellDialect::PowerShell,
            ShellDialect::Cmd,
            ShellDialect::Unknown,
        ] {
            assert_encoding(&paths(&[]), target(dialect, bracketed), b"");
        }
    }
}

#[test]
fn path_controls_refuse_the_whole_list_before_size_admission() {
    // Validate every member before sizing or emitting an accepted prefix, including Unicode controls.
    for control in ['\0', '\n', '\r', '\t', '\x1b', '\u{7f}', '\u{85}'] {
        let invalid = format!("bad{control}path");
        for dialect in [
            ShellDialect::Posix,
            ShellDialect::PowerShell,
            ShellDialect::Cmd,
            ShellDialect::Unknown,
        ] {
            assert_eq!(
                crate::encode_payload(&paths(&["valid path", &invalid]), target(dialect, true), 0),
                Err(PasteRefusal::ControlCharacter)
            );
        }
    }
}

#[test]
fn one_cmd_unsafe_member_refuses_the_whole_list() {
    // A safe prefix never bypasses validation of a later cmd expansion character.
    for invalid in ["percent%", "bang!", "double\"quote"] {
        assert_eq!(
            crate::encode_payload(
                &paths(&["valid path", invalid]),
                target(ShellDialect::Cmd, false),
                0
            ),
            Err(PasteRefusal::CmdUnsafeCharacter)
        );
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_unix_member_refuses_without_lossy_conversion() {
    // Native filename bytes must remain distinguishable from a replacement-character path.
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let payload = UserPayload::Paths(vec![
        PathBuf::from("valid path"),
        PathBuf::from(OsStr::from_bytes(b"/tmp/invalid-\xff")),
    ]);
    assert_eq!(
        crate::encode_payload(&payload, target(ShellDialect::Posix, false), 0),
        Err(PasteRefusal::NonUnicodePath)
    );
}

#[cfg(windows)]
#[test]
fn non_unicode_windows_member_refuses_without_lossy_conversion() {
    // An unpaired UTF-16 surrogate is a native filename unit, not a Unicode replacement character.
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let payload = UserPayload::Paths(vec![
        PathBuf::from("valid path"),
        PathBuf::from(OsString::from_wide(&[0x0043, 0x003a, 0x005c, 0xd800])),
    ]);
    assert_eq!(
        crate::encode_payload(&payload, target(ShellDialect::PowerShell, false), 0),
        Err(PasteRefusal::NonUnicodePath)
    );
}

#[test]
fn exact_byte_limit_includes_multibyte_quote_doubling_and_guards() {
    // Admission counts the doubled UTF-8 scalar and both guards, accepting equality and rejecting one byte less.
    let payload = paths(&["’"]);
    assert_encoding(&payload, target(ShellDialect::PowerShell, false), "'’’'".as_bytes());
    assert_encoding(
        &payload,
        target(ShellDialect::PowerShell, true),
        "\x1b[200~'’’'\x1b[201~".as_bytes(),
    );
}

#[test]
fn checked_encoded_length_saturates_overflow_without_wrapping() {
    // Synthetic length terms reach the arithmetic boundary without allocating an impossible payload.
    assert_eq!(checked_add_len(usize::MAX - 2, 1), Ok(usize::MAX - 1));
    assert_eq!(checked_add_len(usize::MAX - 1, 1), Ok(usize::MAX));
    assert_eq!(
        checked_add_len(usize::MAX - 1, 2),
        Err(PasteRefusal::TooLarge { needed: usize::MAX })
    );
    assert_eq!(checked_add_len(usize::MAX, 1), Err(PasteRefusal::TooLarge { needed: usize::MAX }));
}

#[test]
fn encoded_lengths_and_quote_helpers_agree_for_all_supported_dialects() {
    // Sizing and final emission share the existing quote rules across empty, special and Unicode values.
    for value in ["", "plain", "a b", "a'b", "‘’‚‛", "a&b", "日本ü"] {
        for dialect in [
            ShellDialect::Posix,
            ShellDialect::PowerShell,
            ShellDialect::Cmd,
            ShellDialect::Unknown,
        ] {
            let quoted = match dialect {
                ShellDialect::Posix | ShellDialect::Unknown => shell_quote_posix(value),
                ShellDialect::PowerShell => shell_quote_powershell(value),
                ShellDialect::Cmd => format!("\"{value}\""),
            };
            for bracketed in [false, true] {
                let expected =
                    if bracketed { format!("\x1b[200~{quoted}\x1b[201~") } else { quoted.clone() };
                assert_encoding(&paths(&[value]), target(dialect, bracketed), expected.as_bytes());
            }
        }
    }
}
