use super::*;

/// Blanking keeps the byte length and every line break, so offsets and line numbers taken from the
/// stripped code point at the same place in the original source.
#[test]
fn code_only_keeps_length_and_line_breaks() {
    let source = "let a = \"x\ny\"; /* one\ntwo */ call();\r\n// tail\nend();";
    let code = code_only(source);
    assert_eq!(code.len(), source.len());
    assert_eq!(code.matches('\n').count(), source.matches('\n').count());
    assert_eq!(line_number(source, source.find("end").unwrap()), 5);
    assert_eq!(line_number(&code, code.find("end").unwrap()), 5);
}

/// Line comments, block comments and nested block comments are blanked whole, and the code after a
/// comment closes is kept.
#[test]
fn code_only_blanks_line_block_and_nested_comments() {
    let code = code_only("keep(); // gone()\n/* gone() /* inner() */ still_gone() */ after();");
    assert!(code.contains("keep();") && code.contains("after();"));
    for removed in ["gone", "inner", "still_gone"] {
        assert!(!code.contains(removed), "{removed} survived: {code:?}");
    }
}

/// String contents are blanked through escaped quotes, so a `//` or `/*` inside a string, such as a
/// URL, never starts a comment that would hide the code after the string.
#[test]
fn code_only_blanks_strings_without_eating_the_code_after_them() {
    let code = code_only(
        r#"let note = " // "; read(); let url = "https://a.co/*"; next(); let quoted = "a\"b // c"; last();"#,
    );
    for kept in ["read();", "next();", "last();"] {
        assert!(code.contains(kept), "{kept} was lost: {code:?}");
    }
    assert!(!code.contains("https") && !code.contains("a.co") && !code.contains(" c\""));
}

/// Raw strings with and without hashes, byte strings, raw byte strings and C strings are blanked,
/// and a raw identifier such as `r#type` is code.
#[test]
fn code_only_blanks_raw_and_prefixed_strings() {
    let source = concat!(
        "a(r\"raw_one\"); b(r#\"raw \"quoted\" two\"#); c(b\"bytes\"); d(br##\"raw#\"# bytes\"##);",
        " e(c\"cstr\"); let r#type = 1; f();"
    );
    let code = code_only(source);
    for removed in ["raw_one", "quoted", "two", "bytes", "cstr"] {
        assert!(!code.contains(removed), "{removed} survived: {code:?}");
    }
    for kept in ["a(", "b(", "c(", "d(", "e(", "r#type", "f();"] {
        assert!(code.contains(kept), "{kept} was lost: {code:?}");
    }
}

/// Char and byte-char literals are blanked, including a quote, a brace and escapes, while a
/// lifetime or loop label stays code and does not swallow what follows it.
#[test]
fn code_only_blanks_char_literals_but_keeps_lifetimes() {
    let code = code_only(
        "let quote = '\"'; one(); let brace = b'{'; two(); let tick = '\\''; three(); \
         let wide = '\\u{41}'; four(); fn hold<'a>(text: &'a str) {} 'outer: loop {} five();",
    );
    for kept in ["one();", "two();", "three();", "four();", "<'a>", "&'a str", "'outer:", "five();"]
    {
        assert!(code.contains(kept), "{kept} was lost: {code:?}");
    }
    assert_eq!(code.matches('{').count(), 2, "only the two real block braces remain: {code:?}");
    assert!(!code.contains("u{41}"));
}

/// A token sequence matches across spaces and line breaks, compares whole tokens, and reports the
/// offset of its first token.
#[test]
fn token_matches_ignores_layout_and_compares_whole_tokens() {
    let code = "frame.grid()\n    .row(0); grid . row (1); my_grid.row(2); grid.rows(3);";
    assert_eq!(token_matches(code, "grid().row("), vec![6]);
    assert_eq!(token_matches(code, "grid.row(").len(), 1, "only the spaced `grid . row (`");
    assert!(token_matches(code, "").is_empty(), "an empty pattern matches nothing");
}

/// A block span runs from the first brace at or after the offset to its matching brace, counting
/// nested blocks, and is `None` for a block that never closes.
#[test]
fn block_span_finds_the_matching_brace() {
    let code = "fn outer() { if ready { inner(); } done(); } next();";
    let span = block_span(code, 0).expect("the body closes");
    assert_eq!(&code[span.clone()], "{ if ready { inner(); } done(); }");
    assert_eq!(block_span(code, span.end), None, "no brace follows");
    assert_eq!(block_span("fn open() { never", 0), None);
}
