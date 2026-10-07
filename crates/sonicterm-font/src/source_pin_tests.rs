use super::*;

/// `source` with each of `literals` (each occurring once) replaced by spaces of its UTF-8 byte length: what
/// `code_only` must produce when those are exactly the source's comments and literals.
fn blanked(source: &str, literals: &[&str]) -> String {
    literals.iter().fold(source.to_owned(), |text, literal| {
        assert_eq!(text.matches(literal).count(), 1, "fixture literal {literal:?} occurs once");
        text.replacen(literal, &" ".repeat(literal.len()), 1)
    })
}

/// A comment or literal never reaches the code view, and the delimiters around it stay: a `//` inside a string
/// is not a comment, and a `"` inside a character literal is not a string.
#[test]
fn line_comments_strings_and_char_literals_are_masked() {
    assert_eq!(
        code_only("let text = \"a // b\"; // note\nlet quote = '\"';"),
        "let text =         ;        \nlet quote =    ;"
    );
    assert_eq!(
        code_only("let lifetime: &'static str = \"x\"; let quote = '\\''; let raw = r#\"//\"#;"),
        "let lifetime: &'static str =    ; let quote =     ; let raw =        ;"
    );
}

/// Block comments nest: the comment ends at the `*/` that closes the outermost `/*`, so code after an inner
/// close, such as a method-shaped decoy, stays masked, and line breaks inside it are kept.
#[test]
fn nested_block_comments_are_masked_to_their_outer_close() {
    assert_eq!(
        code_only("a /* x /* y */ z */ b /* w */ c"),
        format!("a {} b {} c", " ".repeat(17), " ".repeat(7))
    );
    let source = "/* outer /* inner */\nfn decoy() {}\n*/\nfn real() {}\n";
    let masked = code_only(source);
    assert!(!masked.contains("decoy") && !masked.contains("*/"), "{masked}");
    assert!(masked.ends_with("\nfn real() {}\n"), "{masked}");
    assert_eq!(masked.lines().count(), source.lines().count());
}

/// A raw string (`r`), raw byte string (`br`) and raw C string (`cr`) with zero to three hashes ends only at a
/// quote followed by its own hash count: a quote with fewer hashes inside it, or a `//`, stays masked, and the
/// code after the literal stays code. `r#name` is a raw identifier, which is code.
#[test]
fn raw_strings_of_every_prefix_and_hash_count_are_masked_whole() {
    for prefix in ["r", "br", "cr"] {
        for hash_count in 0..=3_usize {
            let hashes = "#".repeat(hash_count);
            let body = if hash_count == 0 {
                "x // z".to_owned()
            } else {
                format!("x \"{} y // z", "#".repeat(hash_count - 1))
            };
            let literal = format!("{prefix}{hashes}\"{body}\"{hashes}");
            let source = format!("let value = {literal}; let after = 1;");
            assert_eq!(
                code_only(&source),
                blanked(&source, &[&literal]),
                "{prefix} with {hash_count} hashes"
            );
        }
    }
    let identifiers = "let r#type = 1; let r#fn = r#type;";
    assert_eq!(code_only(identifiers), identifiers);
}

/// Escaped strings with every prefix (`"…"`, `b"…"`, `c"…"`) end at their first unescaped quote, and byte and
/// escaped character literals are masked, while lifetimes and labels, which also start with `'`, stay code.
#[test]
fn escaped_strings_c_strings_and_char_literals_are_masked_but_lifetimes_kept() {
    let literals = ["\"q\\\"// x\"", "b\"y\"", "c\"z\\n\"", "'\\''", "b'\"'", "'\"'"];
    let source = format!(
        "fn keep<'a>(value: &'a str) -> &'a str {{ 'outer: loop {{ break 'outer; }} \
         let text = {}; let bytes = {}; let cstr = {}; let quote = {}; let byte = {}; let dquote = {}; value }}",
        literals[0], literals[1], literals[2], literals[3], literals[4], literals[5]
    );
    assert_eq!(code_only(&source), blanked(&source, &literals));
}

/// Unicode never splits: a multi-byte character literal, string and escape is masked to spaces of its byte
/// length, a Unicode identifier stays code (and keeps a following `r` from opening a raw string), and even an
/// invalid escape of a multi-byte character neither panics nor leaves a partial character.
#[test]
fn unicode_literals_are_masked_whole_and_unicode_identifiers_stay_code() {
    let literals = ["'界'", "\"界\\\"面\"", "'\\u{754c}'", "'\\界'", "\"a\\\"b\""];
    let source = format!(
        "let glyph = {};\nlet 名前 = {};\nfn 界面<'a>(名r: &'a str) -> char {{ {} }}\nlet bad = {};\nmacro_call!(名r{} kept);\n",
        literals[0], literals[1], literals[2], literals[3], literals[4]
    );
    let masked = code_only(&source);
    assert_eq!(masked, blanked(&source, &literals));
    assert_eq!(masked.len(), source.len(), "byte offsets are preserved");
    assert!(
        masked.contains("fn 界面<'a>(名r: &'a str)") && masked.contains("let 名前 ="),
        "{masked}"
    );
}

/// The fixture shared by the item tests: an impl whose method mentions its own signature only in a comment and
/// a string, with `decoy` inserted before it.
fn item_fixture(decoy: &str) -> String {
    format!(
        "{decoy}impl Face {{\n    pub fn target(&self) -> u8 {{\n        // pub fn target(\n        \
         let label = \"pub fn target(\";\n        1\n    }}\n}}\n"
    )
}

/// The item body is the code of the one definition, up to its closing brace at its own indentation, with the
/// comment and string that repeat the signature masked out.
#[test]
fn a_unique_item_body_is_read_as_code_only() {
    let body = item_body(&item_fixture(""), "pub fn target(").expect("one definition");
    assert!(body.starts_with("pub fn target(&self) -> u8 {"), "{body}");
    assert!(body.ends_with("        1"), "{body}");
    assert_eq!(body.matches("pub fn target(").count(), 1, "{body}");
    assert_eq!(
        item_body("fn other() {}\n", "pub fn target("),
        Err("pub fn target( is not defined".to_owned())
    );
    assert_eq!(
        item_body("    pub fn target() {\n        1\n", "pub fn target("),
        Err("pub fn target( does not end".to_owned())
    );
}

/// A second definition in code, in an unused macro or a cfg-disabled impl, before or after the real one, makes
/// the source ambiguous and the read fails closed; the same definition inside a nested comment, a hashed raw
/// string of any prefix or a C string is not code and leaves the read unambiguous.
#[test]
fn a_second_definition_in_code_is_ambiguous_but_one_in_a_comment_or_literal_is_not() {
    let copy = "    pub fn target(&self) -> u8 {\n        2\n    }\n";
    let macro_decoy = format!("macro_rules! decoy {{\n    () => {{\n{copy}    }};\n}}\n");
    let disabled_impl = format!("#[cfg(any())]\nimpl Face {{\n{copy}}}\n");
    for (label, decoy) in [("macro", &macro_decoy), ("disabled impl", &disabled_impl)] {
        let after = format!("{}{decoy}", item_fixture(""));
        for (place, source) in [("before", item_fixture(decoy)), ("after", after)] {
            assert_eq!(
                item_body(&source, "pub fn target("),
                Err("ambiguous source: pub fn target( occurs 2 times".to_owned()),
                "{label} {place}"
            );
        }
    }
    let masked_decoys = [
        format!("/* outer /* inner */\n{copy}*/\n"),
        format!("const RAW: &str = r#\"say \"\n{copy}\"#;\n"),
        format!("const BYTES: &[u8] = br##\"say \"#\n{copy}\"##;\n"),
        format!("const CRAW: &std::ffi::CStr = cr#\"say \"\n{copy}\"#;\n"),
        format!("const CSTR: &std::ffi::CStr = c\"\n{copy}\";\n"),
    ];
    for decoy in &masked_decoys {
        let body = item_body(&item_fixture(decoy), "pub fn target(").expect(decoy);
        assert!(body.ends_with("        1"), "{decoy}: {body}");
    }
}
