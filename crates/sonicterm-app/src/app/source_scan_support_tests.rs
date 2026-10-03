use super::*;

/// Each view keeps byte offsets and newlines; `code` keeps literal contents while `bare` blanks
/// them, both blank nested block comments whole, an escaped quote char literal ends where Rust
/// ends it, a raw string ends only at its own closer, and `\r\n` reads as `\n`.
#[test]
fn views_blank_comments_and_literal_contents_but_keep_code() {
    let source = "a('\\'');/* x /* y */ z */b(r#\"q\"w\"#);c(\"s\\\"t\");// d\r\ne()";
    let (code, bare) = code_views(source);
    assert_eq!(code.len(), bare.len());
    assert!(!code.contains('\r') && !bare.contains('\r'));
    for real in ["a(", "b(", "c(", "e()"] {
        assert!(bare.contains(real), "{real} survives: {bare:?}");
    }
    for hidden in ["x", "y", "z", "d"] {
        assert!(!code.contains(hidden) && !bare.contains(hidden), "{hidden} blanked: {bare:?}");
    }
    assert!(code.contains("q\"w") && !bare.contains("q\"w"), "raw string contents: {bare:?}");
    assert!(code.contains("s\\\"t") && !bare.contains('s'), "string contents: {bare:?}");
    assert_eq!(code.lines().count(), 2, "the newline survives");
}
