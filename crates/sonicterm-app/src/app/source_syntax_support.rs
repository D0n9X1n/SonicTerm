//! Test-only syntax-tree view of Rust source: which `fn` items and struct fields a file defines,
//! and whether each is compiled only for tests. Structural source checks read this instead of
//! matching text, so a cfg on an enclosing `impl` or module counts and a name in a comment or a
//! string literal never does.
//!
//! The test-support audit and the parser-write publication contract use it. Other source tests
//! still match text through `source_scan_support`: they pin an exact statement or call order inside
//! one function (lock order, publication order, a specific guard), which a token view states more
//! directly than a tree walk; behavior contracts belong in behavior tests, not here.

use syn::visit::Visit;

/// One `fn` or struct field found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceItem {
    /// The function's or field's name.
    pub(super) name: String,
    /// The struct that holds a field; `None` for a function.
    pub(super) owner: Option<String>,
    /// The item, or anything enclosing it, carries `#[cfg(test)]` or `#[cfg(any(test, ...))]`.
    pub(super) test_only: bool,
}

/// Whether `attributes` hold a cfg every build of which requires `test`: `cfg(test)`, or
/// `cfg(any(test, feature = "test-support"))`, the gate the test-support feature uses.
fn requires_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            // When: the attribute is not a `cfg`, it gates nothing.
            return false;
        }
        let text = attribute.meta.require_list().map(|list| list.tokens.to_string());
        text.is_ok_and(|tokens| {
            let tokens = tokens.replace(' ', "");
            tokens == "test" || tokens == "any(test,feature=\"test-support\")"
        })
    })
}

/// Collects items while tracking whether an enclosing item was test-only.
struct Collector {
    /// Items found so far.
    items: Vec<SourceItem>,
    /// How many enclosing items are test-only; nonzero means everything inside is.
    gated_depth: usize,
}

impl Collector {
    /// Visit `inner` with the gate depth raised when `attributes` require `test`.
    fn within(&mut self, attributes: &[syn::Attribute], inner: impl FnOnce(&mut Self)) {
        let gated = requires_test(attributes);
        self.gated_depth += usize::from(gated);
        inner(self);
        self.gated_depth -= usize::from(gated);
    }

    /// Record `name` as found, gated by `attributes` or any enclosing gate.
    fn record(&mut self, name: String, owner: Option<String>, attributes: &[syn::Attribute]) {
        let test_only = self.gated_depth > 0 || requires_test(attributes);
        self.items.push(SourceItem { name, owner, test_only });
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.record(item.sig.ident.to_string(), None, &item.attrs);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.record(item.sig.ident.to_string(), None, &item.attrs);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        self.within(&item.attrs, |collector| syn::visit::visit_item_impl(collector, item));
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        self.within(&item.attrs, |collector| syn::visit::visit_item_mod(collector, item));
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        let owner = item.ident.to_string();
        self.within(&item.attrs, |collector| {
            for field in &item.fields {
                if let Some(name) = field.ident.as_ref() {
                    collector.record(name.to_string(), Some(owner.clone()), &field.attrs);
                }
            }
        });
    }
}

/// Counts method calls by name, skipping anything compiled only for tests.
struct CallCounter<'names> {
    /// The method names to count.
    names: &'names [&'names str],
    /// Calls found outside test-only items.
    count: usize,
    /// How many enclosing items are test-only.
    gated_depth: usize,
}

impl<'ast> Visit<'ast> for CallCounter<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if self.gated_depth == 0 && self.names.contains(&call.method.to_string().as_str()) {
            self.count += 1;
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    /// A path call (`App::name(..)` or `name(..)`) counts like a method call.
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            let last = path.path.segments.last().map(|segment| segment.ident.to_string());
            if self.gated_depth == 0 && last.is_some_and(|name| self.names.contains(&name.as_str()))
            {
                self.count += 1;
            }
        }
        syn::visit::visit_expr_call(self, call);
    }

    /// A test-only `impl` gates every call inside it, as a test-only `fn` or module does.
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let gated = usize::from(requires_test(&item.attrs));
        self.gated_depth += gated;
        syn::visit::visit_item_impl(self, item);
        self.gated_depth -= gated;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let gated = usize::from(requires_test(&item.attrs));
        self.gated_depth += gated;
        syn::visit::visit_item_fn(self, item);
        self.gated_depth -= gated;
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let gated = usize::from(requires_test(&item.attrs));
        self.gated_depth += gated;
        syn::visit::visit_impl_item_fn(self, item);
        self.gated_depth -= gated;
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let gated = usize::from(requires_test(&item.attrs));
        self.gated_depth += gated;
        syn::visit::visit_item_mod(self, item);
        self.gated_depth -= gated;
    }
}

/// How many `.name(..)` method calls for any of `names` `source` makes outside test-only items.
/// `None` when `source` does not parse.
pub(super) fn method_calls(source: &str, names: &[&str]) -> Option<usize> {
    let file = syn::parse_file(source).ok()?;
    let mut counter = CallCounter { names, count: 0, gated_depth: 0 };
    counter.visit_file(&file);
    Some(counter.count)
}

/// Every `fn` (free or in an `impl`) and named struct field in `source`, with whether it is
/// compiled only for tests. `None` when `source` does not parse.
pub(super) fn source_items(source: &str) -> Option<Vec<SourceItem>> {
    let file = syn::parse_file(source).ok()?;
    let mut collector = Collector { items: Vec::new(), gated_depth: 0 };
    collector.visit_file(&file);
    Some(collector.items)
}

#[cfg(test)]
#[path = "source_syntax_support_tests.rs"]
mod source_syntax_support_tests;
