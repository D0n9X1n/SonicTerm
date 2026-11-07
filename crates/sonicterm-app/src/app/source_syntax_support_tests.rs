//! Pins the syntax-tree item view: a gate on an enclosing impl or module counts, comments and
//! strings never define items, and struct fields carry their owner.

use super::source_items;

/// A gate on the item, on its `impl` or on its module marks it test-only; an ungated sibling is not.
#[test]
fn enclosing_gates_count() {
    let source = r#"
        pub struct App {
            #[cfg(test)]
            test_seen: bool,
            live: u8,
        }
        impl App {
            #[cfg(any(test, feature = "test-support"))]
            pub fn __test_gated(&self) {}
            pub fn __test_open(&self) {}
        }
        #[cfg(any(test, feature = "test-support"))]
        impl App {
            pub fn __test_in_gated_impl(&self) {}
        }
        #[cfg(test)]
        mod helpers {
            pub fn __test_in_gated_mod() {}
        }
        // fn __test_in_a_comment() {}
        const TEXT: &str = "fn __test_in_a_string() {}";
    "#;
    let items = source_items(source).expect("parses");
    let gated = |name: &str| items.iter().find(|item| item.name == name).map(|item| item.test_only);
    assert_eq!(gated("__test_gated"), Some(true));
    assert_eq!(gated("__test_open"), Some(false));
    assert_eq!(gated("__test_in_gated_impl"), Some(true));
    assert_eq!(gated("__test_in_gated_mod"), Some(true));
    assert_eq!(gated("__test_in_a_comment"), None);
    assert_eq!(gated("__test_in_a_string"), None);
    assert_eq!(gated("test_seen"), Some(true));
    assert_eq!(gated("live"), Some(false));
    let field = items.iter().find(|item| item.name == "live").unwrap();
    assert_eq!(field.owner.as_deref(), Some("App"));
}

/// A cfg that does not require `test` (a platform, a feature alone) is not a test gate.
#[test]
fn other_cfgs_are_not_test_gates() {
    let source = r#"
        #[cfg(windows)]
        pub fn __test_windows() {}
        #[cfg(feature = "test-support")]
        pub fn __test_feature_alone() {}
        #[cfg(not(test))]
        pub fn __test_not_test() {}
    "#;
    let items = source_items(source).expect("parses");
    assert!(items.iter().all(|item| !item.test_only), "{items:?}");
}

/// Method calls are counted from the tree: comments, strings and test-only functions never count.
#[test]
fn method_calls_skip_comments_strings_and_test_items() {
    let source = r#"
        fn live(parser: &mut Parser) {
            parser.advance(b"x");
            // parser.advance(b"comment");
            let _ = "parser.advance(b\"string\")";
        }
        #[cfg(test)]
        fn fixture(parser: &mut Parser) {
            parser.advance(b"test");
        }
    "#;
    assert_eq!(super::method_calls(source, &["advance"]), Some(1));
}
