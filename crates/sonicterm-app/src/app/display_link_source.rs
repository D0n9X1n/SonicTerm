//! Test-only source corpus and structural scanners for display-link ownership.

#![cfg(test)]

/// `source` with comments blanked and CRLF normalized, so a scan matches only real code.
fn code_of(source: &str) -> String {
    crate::app::source_scan_support::code_views(source).0
}

/// The body of the item `signature` opens, up to the closing brace at the signature's indentation.
fn body_of<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} exists"));
    let line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let indent = &source[line_start..start];
    let close = format!("\n{indent}}}\n");
    let end = source[start..].find(&close).map_or(source.len(), |offset| start + offset);
    &source[start..end]
}

/// Whether `body` assigns `field` (`field =`, not `field ==`).
fn assigns(body: &str, field: &str) -> bool {
    body.match_indices(&format!("{field} =")).any(|(offset, matched)| {
        body.as_bytes().get(offset + matched.len()) != Some(&b'=')
            && !body[..offset].ends_with(|byte: char| byte.is_alphanumeric() || byte == '_')
    })
}

/// Complete logical owner sources, including the flat extraction modules.
const REDRAW_SOURCES: &str = concat!(
    include_str!("redraw.rs"),
    "\n",
    include_str!("redraw_admission.rs"),
    "\n",
    include_str!("redraw_state.rs"),
    "\n",
    include_str!("redraw_window.rs")
);

#[cfg(test)]
#[path = "display_link_source_tests.rs"]
mod display_link_source_tests;
