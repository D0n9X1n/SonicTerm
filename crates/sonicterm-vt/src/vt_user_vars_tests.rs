//! Pins OSC 1337 `SetUserVar` records: the WezTerm form, kitty's multi-record and delete forms,
//! strict decoding that drops only the bad record, control characters shown as spaces, the entry
//! and byte caps with oldest-first eviction, survival across RIS, and a `Debug` without contents.

use base64::Engine;
use sonicterm_grid::grid::Grid;

use super::*;
use crate::vt::{CaptureStagingPool, Parser};

/// A parser with a private staging pool, as the crate's unit tests require.
fn parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(40, 4), None, CaptureStagingPool::new())
}

/// `bytes` in standard padded base64, as shells encode a value.
fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The parser's variables as owned pairs, oldest first.
fn vars(parser: &Parser) -> Vec<(String, String)> {
    parser.user_vars().iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect()
}

/// `expected` as owned pairs, to compare with [`vars`].
fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
}

/// The WezTerm shell-integration form sets one variable, ended by BEL or by ST, and the text after
/// it prints normally.
#[test]
fn the_wezterm_form_sets_a_variable() {
    for terminator in [b"\x07".as_slice(), b"\x1b\\"] {
        let mut parser = parser();
        let mut sequence = b"\x1b]1337;SetUserVar=WEZTERM_PROG=dmlt".to_vec();
        sequence.extend_from_slice(terminator);
        sequence.push(b'Z');
        parser.advance(&sequence);
        assert_eq!(vars(&parser), pairs(&[("WEZTERM_PROG", "vim")]), "{terminator:?}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    }
}

/// Every `SetUserVar=` record of one OSC applies in order: unpadded base64 decodes, `NAME=` sets
/// an empty value, and `NAME` without `=` deletes.
#[test]
fn records_apply_in_order_with_empty_values_and_deletes() {
    let mut parser = parser();
    parser.advance(b"\x1b]1337;SetUserVar=a=MQ==;SetUserVar=b=Mg;SetUserVar=c=;SetUserVar=a\x07");
    assert_eq!(vars(&parser), pairs(&[("b", "2"), ("c", "")]));
}

/// Invalid base64, a value that is not UTF-8, a name that is not UTF-8, an empty name and a record
/// that is not `SetUserVar=` are each dropped alone; the valid records around them still apply.
#[test]
fn a_bad_record_drops_only_itself() {
    let mut parser = parser();
    let not_utf8 = encode(&[0xff, 0xfe]);
    let mut sequence = b"\x1b]1337;SetUserVar=bad=!!!;SetUserVar=first=MQ==;".to_vec();
    sequence.extend_from_slice(format!("SetUserVar=value={not_utf8};").as_bytes());
    sequence.extend_from_slice(b"SetUserVar=n\xffame=MQ==;SetUserVar==MQ==;SetUserVar=;");
    sequence.extend_from_slice(b"CurrentDir=/tmp;SetUserVar=last=Mg==\x07");
    parser.advance(&sequence);
    assert_eq!(vars(&parser), pairs(&[("first", "1"), ("last", "2")]));
}

/// C0 controls, DEL and C1 controls in a name or a decoded value are shown as spaces, so a value
/// cannot carry an escape sequence to whatever displays it.
#[test]
fn control_characters_become_spaces() {
    let mut parser = parser();
    let value = encode("x\ty\u{85}z\u{1b}[31m".as_bytes());
    let sequence = format!("\x1b]1337;SetUserVar=a\x01b\x7fc\u{9b}d={value}\x07");
    parser.advance(sequence.as_bytes());
    assert_eq!(vars(&parser), pairs(&[("a b c d", "x y z [31m")]));
}

/// The store holds at most 64 entries: a 65th evicts the oldest, and setting an existing name
/// moves it to the newest place, so the next eviction takes the one after it.
#[test]
fn sixty_four_entries_evict_the_oldest_and_resetting_makes_newest() {
    let mut parser = parser();
    for index in 0..=MAX_USER_VARS {
        parser.advance(format!("\x1b]1337;SetUserVar=v{index}=MQ==\x07").as_bytes());
    }
    let names: Vec<String> = vars(&parser).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names.len(), MAX_USER_VARS);
    assert_eq!(names.first().map(String::as_str), Some("v1"), "v0 was evicted");
    parser.advance(b"\x1b]1337;SetUserVar=v1=Mg==;SetUserVar=extra=Mw==\x07");
    let held = vars(&parser);
    assert_eq!(held.len(), MAX_USER_VARS);
    assert_eq!(held[0].0, "v3", "v2 was the oldest once v1 was set again");
    assert_eq!(held[held.len() - 2], ("v1".to_owned(), "2".to_owned()));
    assert_eq!(held[held.len() - 1].0, "extra");
}

/// Names and values share a 16 KiB budget: an insert past it evicts the oldest entries first.
#[test]
fn the_byte_budget_evicts_the_oldest_first() {
    let mut parser = parser();
    let value = encode(&[b'a'; 4000]);
    // Four 4002-byte entries fit 16 KiB; the fifth pushes the oldest out.
    let records: Vec<String> = (0..5).map(|index| format!("SetUserVar=n{index}={value}")).collect();
    parser.advance(format!("\x1b]1337;{}\x07", records.join(";")).as_bytes());
    let held = vars(&parser);
    let names: Vec<&str> = held.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["n1", "n2", "n3", "n4"]);
    let total: usize = held.iter().map(|(name, value)| name.len() + value.len()).sum();
    assert!(total <= MAX_USER_VAR_STORE_BYTES, "{total}");
}

/// A value decoding to 4 KiB and a 128-byte name are kept; one byte more of either drops that
/// record whole.
#[test]
fn an_over_long_value_or_name_is_dropped() {
    let mut parser = parser();
    let fits = encode(&[b'v'; MAX_USER_VAR_VALUE_BYTES]);
    let over = encode(&[b'v'; MAX_USER_VAR_VALUE_BYTES + 1]);
    let long_name = "n".repeat(MAX_USER_VAR_NAME_BYTES);
    let over_name = "o".repeat(MAX_USER_VAR_NAME_BYTES + 1);
    let sequence = format!(
        "\x1b]1337;SetUserVar=fits={fits};SetUserVar=over={over};\
         SetUserVar={long_name}=MQ==;SetUserVar={over_name}=MQ==\x07"
    );
    parser.advance(sequence.as_bytes());
    let held = vars(&parser);
    let names: Vec<&str> = held.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["fits", long_name.as_str()]);
    assert_eq!(held[0].1.len(), MAX_USER_VAR_VALUE_BYTES);
}

/// RIS resets the terminal but keeps the variables, which describe the program, not the screen.
#[test]
fn variables_survive_ris() {
    let mut parser = parser();
    parser.advance(b"\x1b]1337;SetUserVar=kept=MQ==\x07\x1bc");
    assert_eq!(vars(&parser), pairs(&[("kept", "1")]));
}

/// `Debug` shows how many variables and bytes are held, never a name or a value, so a logged
/// parser state cannot leak them.
#[test]
fn debug_shows_counts_only() {
    let mut parser = parser();
    let value = encode(b"secret-value");
    parser.advance(format!("\x1b]1337;SetUserVar=secret-name={value}\x07").as_bytes());
    let shown = format!("{:?}", parser.user_vars());
    assert!(!shown.contains("secret"), "{shown}");
    assert!(shown.contains("count: 1"), "{shown}");
    assert!(shown.contains("bytes: 23"), "{shown}");
}

/// A snapshot taken before a later record keeps what it held, so a reader may serialize it after
/// the parser lock is released while the parser goes on.
#[test]
fn a_snapshot_is_unchanged_by_later_records() {
    let mut parser = parser();
    parser.advance(b"\x1b]1337;SetUserVar=a=MQ==\x07");
    let snapshot = parser.user_vars();
    parser.advance(b"\x1b]1337;SetUserVar=a=Mg==;SetUserVar=b=Mw==\x07");
    let before: Vec<(&str, &str)> = snapshot.iter().collect();
    assert_eq!(before, [("a", "1")]);
    assert_eq!(vars(&parser), pairs(&[("a", "2"), ("b", "3")]));
}
