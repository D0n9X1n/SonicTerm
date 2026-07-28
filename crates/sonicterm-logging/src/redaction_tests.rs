//! Diagnostic events must never carry user content.
//!
//! Logs are what a user pastes into a bug report. A memory diagnostic that
//! names a URL they visited, a path they typed, or a command they ran turns
//! "here is my log" into an unintended disclosure — and the person pasting it
//! has no way to know, because the field that leaks looks like every other
//! field.
//!
//! The property held when this was written: all 68 fields across every
//! `target: "memory"` site are counts, byte sizes, dimensions, or enums.
//! Nothing carries content. But it held by *habit* — a new
//! `tracing::warn!(uri = uri, ...)` would have passed every test in the
//! workspace.
//!
//! These tests scan the real sources rather than asserting against a list of
//! known-good fields, so they catch a leak added anywhere, including in code
//! written after them.

// No `use super::*`: these tests read the shipped sources as strings and
// assert on their contents, so they deliberately touch nothing in this crate.

/// Sources that emit resource/memory diagnostics.
///
/// Compiled in via `include_str!` so the test reads exactly what ships. A test
/// that re-listed the fields it expected would only ever check the fields its
/// author already knew about.
const MEDIA_SRC: &str = include_str!("../../sonicterm-app/src/app/media.rs");
const RETENTION_SRC: &str = include_str!("../../sonicterm-app/src/app/retention.rs");
const VT_SRC: &str = include_str!("../../sonicterm-vt/src/vt.rs");
const HYPERLINK_SRC: &str = include_str!("../../sonicterm-grid/src/hyperlink.rs");

fn sources() -> [(&'static str, &'static str); 4] {
    [
        ("sonicterm-app/src/app/media.rs", MEDIA_SRC),
        ("sonicterm-app/src/app/retention.rs", RETENTION_SRC),
        ("sonicterm-vt/src/vt.rs", VT_SRC),
        ("sonicterm-grid/src/hyperlink.rs", HYPERLINK_SRC),
    ]
}

/// Extract the field bindings of every `tracing::` macro call in `src`.
///
/// Returns `(line number, field name, bound expression)` for each `name =
/// expr` inside a tracing macro. Deliberately simple: it over-reports rather
/// than under-reports, because a privacy check that misses a call site is
/// worse than one that occasionally flags an innocent field.
fn logged_fields(src: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut in_macro = false;
    let mut depth = 0i32;

    for (index, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }
        if !in_macro && line.contains("tracing::") && line.contains('!') {
            in_macro = true;
            depth = 0;
        }
        if !in_macro {
            continue;
        }

        depth += line.matches('(').count() as i32;
        depth -= line.matches(')').count() as i32;

        if let Some((name, value)) = trimmed.split_once('=') {
            let name = name.trim();
            let value = value.trim().trim_end_matches(',').trim();
            let is_field_binding = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && !value.starts_with('=') // not `==`
                && !name.ends_with('!');
            if is_field_binding {
                out.push((index + 1, name.to_string(), value.to_string()));
            }
        }

        if depth <= 0 && (trimmed.ends_with(");") || trimmed.ends_with(")")) {
            in_macro = false;
        }
    }
    out
}

/// No diagnostic field may be bound to raw user content.
///
/// The check is on the **bound expression**, not the field name. A field named
/// `uri_bytes` is fine when it holds `uri.len()`; the same name bound to `uri`
/// would be a leak. Naming alone cannot distinguish them.
#[test]
fn diagnostics_never_bind_raw_user_content() {
    // Identifiers that hold user-controlled content in these modules.
    const CONTENT_BINDINGS: &[&str] =
        &["uri", "url", "path", "command", "cmd", "title", "text", "data", "payload", "content"];

    let mut leaks = Vec::new();
    for (file, src) in sources() {
        for (line, field, value) in logged_fields(src) {
            // A length, capacity, or count of user content is a measurement,
            // not the content itself.
            let is_measurement = value.ends_with(".len()")
                || value.ends_with(".capacity()")
                || value.contains(".map_or(0,")
                || value.contains("map_or(0,");
            if is_measurement {
                continue;
            }
            let leaks_content = CONTENT_BINDINGS.iter().any(|needle| {
                value == *needle
                    || value == format!("&{needle}")
                    || value == format!("{needle}.clone()")
                    || value == format!("{needle}.to_string()")
                    || value.starts_with(&format!("{needle}."))
                        && !value.contains("len()")
                        && !value.contains("capacity()")
            });
            if leaks_content {
                leaks.push(format!("{file}:{line}  {field} = {value}"));
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "diagnostic fields bound to raw user content — logs get pasted into bug \
         reports, so a URL, path, or command here is an unintended disclosure the \
         person pasting cannot see:\n  {}",
        leaks.join("\n  ")
    );
}

/// Every logged field is a scalar, not a collection or a borrowed buffer.
///
/// A `Vec`, slice, or `String` in a diagnostic is both a formatting cost on a
/// hot path and a way for content to arrive indirectly — `images` renders every
/// image's debug form, which is not obviously a disclosure until it is.
#[test]
fn diagnostics_log_scalars_not_buffers() {
    let mut offenders = Vec::new();
    for (file, src) in sources() {
        for (line, field, value) in logged_fields(src) {
            let looks_like_buffer = value.starts_with("&self.")
                && !value.contains("len()")
                && !value.contains("bytes")
                && !value.contains("count");
            let is_vec_or_slice =
                value.ends_with(".to_vec()") || value.ends_with(".collect()") || value == "images";
            if looks_like_buffer || is_vec_or_slice {
                offenders.push(format!("{file}:{line}  {field} = {value}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "diagnostic fields that are buffers or collections rather than scalars:\n  {}",
        offenders.join("\n  ")
    );
}

/// The scanner actually finds the fields it claims to check.
///
/// Without this the two tests above would pass vacuously on an empty scan —
/// the exact failure mode that has already produced three non-discriminating
/// tests in this milestone. If the sources are refactored so the scanner stops
/// matching, this fails rather than silently passing.
#[test]
fn the_scanner_finds_the_diagnostic_fields_it_checks() {
    let total: usize = sources().iter().map(|(_, src)| logged_fields(src).len()).sum();
    assert!(
        total >= 20,
        "the field scanner found only {total} logged fields across four modules that \
         are known to emit many — it has stopped matching, so the privacy checks \
         above are passing without inspecting anything"
    );

    // And it must find the specific redacted-by-design field, proving it reads
    // the shape these checks depend on.
    let vt_fields = logged_fields(VT_SRC);
    let uri_bytes = vt_fields.iter().find(|(_, name, _)| name == "uri_bytes");
    let (_, _, value) = uri_bytes.expect(
        "the scanner must find `uri_bytes` in vt.rs — it is the canonical example of \
         logging a measurement of user content rather than the content",
    );
    assert!(
        value.contains("len()"),
        "`uri_bytes` must be bound to a length, not the URI itself, found: {value}"
    );
}
