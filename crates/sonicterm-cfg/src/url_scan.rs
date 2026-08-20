//! Plain-text URL detection for terminal grid rows.
//!
//! Scans a row of terminal text and returns the byte ranges that look
//! like URLs we are willing to open via [`crate::url_open::open`]. The
//! scanner is deliberately narrow:
//!
//! - Only `http://`, `https://`, `mailto:` and `file://` schemes are
//!   recognised — matching the allow-list enforced by
//!   [`crate::url_open::validate`].
//! - URL characters are limited to RFC 3986 unreserved / sub-delims /
//!   reserved minus a handful of shell-meta and quote chars (`<`, `>`,
//!   `"`, `'`, backtick, whitespace, control). This intentionally
//!   under-matches at the edges (e.g. trailing punctuation like `.`
//!   or `)` is trimmed) but the result is always a string that will
//!   pass `validate()`.
//! - No regex / `once_cell` dependency: the scanner is a small hand
//!   loop so we can keep `sonicterm-cfg`'s dep surface minimal and avoid
//!   per-frame regex compilation cost.
//!
//! The contract is: every returned `(start, end)` slice satisfies
//! `validate(slice).is_ok()`. Tests below assert this.

use crate::url_open::validate;

/// One detected URL in a row of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlMatch {
    /// Byte offset (inclusive) of the URL in the input.
    pub start: usize,
    /// Byte offset (exclusive) of the URL in the input.
    pub end: usize,
    /// The matched URL string.
    pub url: String,
}

/// Provenance carried from text detection to click dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DetectedTarget {
    /// An allow-listed URI detected by the existing URL scanner.
    Uri(String),
    /// Native absolute or explicit-relative filesystem syntax.
    PathCandidate(String),
    /// One contextual filesystem component resolved only against trusted pane CWD.
    BareName(String),
}

/// One typed URI or path-candidate span in a terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetMatch {
    /// Byte offset (inclusive) in the scanned row.
    pub start: usize,
    /// Byte offset (exclusive) in the scanned row.
    pub end: usize,
    /// Detected value and its immutable provenance.
    pub target: DetectedTarget,
}

/// Filesystem grammar used when recognizing raw terminal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// POSIX root and slash-separated dot-relative syntax.
    Posix,
    /// Windows drive-rooted and slash/backslash dot-relative syntax.
    Windows,
}

impl PathStyle {
    /// Grammar native to the current build target.
    #[must_use]
    pub const fn native() -> Self {
        if cfg!(target_os = "windows") {
            // When: `target_os` is Windows, accept drive-rooted and backslash-relative path syntax.
            Self::Windows
        } else {
            // When: `target_os` is not Windows, restrict raw paths to POSIX syntax.
            Self::Posix
        }
    }
}

const SCHEMES: &[&str] = &["https://", "http://", "mailto:", "file://"];
const MAX_TARGET_BYTES: usize = 4096;

/// Return every URL substring of `text` whose scheme is on our
/// allow-list and which passes [`validate`].
pub fn find_urls(text: &str) -> Vec<UrlMatch> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find the next plausible scheme start. We anchor on ASCII
        // letters because every supported scheme begins with one.
        if !bytes[i].is_ascii_alphabetic() {
            // When: `bytes[i]` is not ASCII alphabetic, it cannot start an allow-listed scheme.
            i += 1;
            continue;
        }
        let mut matched_scheme = None;
        for s in SCHEMES {
            let sb = s.as_bytes();
            // Use `text.get(..)` rather than `&text[..]` so a byte
            // range that lands inside a multi-byte UTF-8 char (e.g.
            // `❯` from an oh-my-zsh prompt) returns `None` instead of
            // panicking. Schemes are pure ASCII so a non-boundary end
            // index can never be a real match anyway.
            if let Some(slice) = text.get(i..i + sb.len()) {
                // When: `text.get(...)` yields `slice`, the candidate ended on UTF-8 boundaries and can be compared safely.
                if slice.eq_ignore_ascii_case(s) {
                    // When: `slice` equals `s` ignoring case, retain this scheme length and stop probing alternatives.
                    matched_scheme = Some(sb.len());
                    break;
                }
            }
        }
        let Some(scheme_len) = matched_scheme else {
            // When: `matched_scheme` is absent, advance one byte and continue searching for a scheme start.
            i += 1;
            continue;
        };
        // A scheme match in the middle of a longer identifier
        // (e.g. `xhttp://`) should not count — the previous char,
        // if any, must not itself be a URL body char.
        if i > 0 && is_url_body_char(bytes[i - 1] as char) {
            // When: `i` follows a URL-body character, this scheme text is embedded in a larger token.
            i += 1;
            continue;
        }
        let mut end = i + scheme_len;
        while end < bytes.len() && is_url_body_char(bytes[end] as char) {
            end += 1;
        }
        // Trim trailing punctuation that's commonly adjacent to a
        // URL in prose (`)`, `.`, `,`, `;`, `:`, `!`, `?`).
        while end > i + scheme_len {
            let last = bytes[end - 1] as char;
            if matches!(last, ')' | ']' | '.' | ',' | ';' | ':' | '!' | '?') {
                end -= 1;
            } else {
                // When: `matches!(last, ...)` is false, preserve `last` as part of the candidate URL.
                break;
            }
        }
        // Require at least one body byte after the scheme.
        if end <= i + scheme_len {
            // When: `end` contains no body beyond `scheme_len`, skip the empty URL candidate.
            i += scheme_len;
            continue;
        }
        let url = &text[i..end];
        if validate(url).is_ok() {
            out.push(UrlMatch { start: i, end, url: url.to_string() });
        }
        i = end.max(i + 1);
    }
    out
}

/// Return the URL covering byte offset `byte_col`, if any.
pub fn url_at_byte(text: &str, byte_col: usize) -> Option<UrlMatch> {
    find_urls(text).into_iter().find(|m| byte_col >= m.start && byte_col < m.end)
}

/// Return the URL covering character column `col` (0-based, counting
/// `char`s not bytes — matches the terminal grid model).
pub fn url_at_char_col(text: &str, col: usize) -> Option<UrlMatch> {
    let mut byte = None;
    for (i, (b, _)) in text.char_indices().enumerate() {
        if i == col {
            // When: character index `i` reaches `col`, retain its UTF-8 byte offset for URL lookup.
            byte = Some(b);
            break;
        }
    }
    let byte = byte?;
    url_at_byte(text, byte)
}

/// Return every URI or raw native-path candidate in `text` for this platform.
#[must_use]
pub fn find_targets(text: &str) -> Vec<TargetMatch> {
    find_targets_for_style(text, PathStyle::native())
}

/// Return every URI or raw native-path candidate using an explicit grammar.
///
/// The explicit style keeps cross-platform syntax tests deterministic. URI
/// matches retain priority and the legacy [`find_urls`] implementation remains
/// the only source of URI spans.
#[must_use]
pub fn find_targets_for_style(text: &str, style: PathStyle) -> Vec<TargetMatch> {
    let urls = find_urls(text);
    let mut matches = urls
        .iter()
        .map(|url| TargetMatch {
            start: url.start,
            end: url.end,
            target: DetectedTarget::Uri(url.url.clone()),
        })
        .collect::<Vec<_>>();

    for (start, _) in text.char_indices() {
        if urls.iter().any(|url| start >= url.start && start < url.end) {
            // When: `start` lies inside a validated URI span, preserve URI provenance instead of rescanning its path-like text.
            continue;
        }
        if !is_path_start_boundary(text, start) || !has_path_prefix(&text[start..], style) {
            // When: `start` lacks either a token boundary or native path prefix, it cannot begin a raw path candidate.
            continue;
        }
        let mut end = text.len();
        let mut ambiguous_delimiter = false;
        for (offset, ch) in text[start..].char_indices().skip(1) {
            if is_path_delimiter(ch) {
                // When: `is_path_delimiter(ch)` truncates path syntax, record whether it makes the complete token ambiguous.
                ambiguous_delimiter = ch.is_control() || !ch.is_whitespace();
                end = start + offset;
                break;
            }
        }
        if ambiguous_delimiter {
            // When: `ambiguous_delimiter` split the token at a quote or wrapper, leave the complete token inert.
            continue;
        }
        end = trim_matching_wrapper(text, start, end);
        let Some(candidate) = text.get(start..end) else {
            // When: `start..end` is not a UTF-8 boundary range, discard the malformed candidate span.
            continue;
        };
        if !validate_path_candidate(candidate, style) {
            // When: `candidate` violates the selected native grammar, keep it inert terminal text.
            continue;
        }
        if urls.iter().any(|url| start < url.end && end > url.start) {
            // When: the raw candidate overlaps any URI span, URI provenance wins for the entire overlap.
            continue;
        }
        matches.push(TargetMatch {
            start,
            end,
            target: DetectedTarget::PathCandidate(candidate.to_string()),
        });
    }

    matches.sort_by_key(|matched| matched.start);
    matches.dedup_by(|right, left| right.start == left.start && right.end == left.end);
    matches
}

/// Return the typed target covering byte offset `byte_col` for this platform.
#[must_use]
pub fn target_at_byte(text: &str, byte_col: usize) -> Option<TargetMatch> {
    target_at_byte_for_style(text, byte_col, PathStyle::native())
}

/// Return the typed target covering byte offset `byte_col` for `style`.
#[must_use]
pub fn target_at_byte_for_style(
    text: &str,
    byte_col: usize,
    style: PathStyle,
) -> Option<TargetMatch> {
    find_targets_for_style(text, style)
        .into_iter()
        .find(|matched| byte_col >= matched.start && byte_col < matched.end)
}

/// Return the typed target covering character column `col` for this platform.
#[must_use]
pub fn target_at_char_col(text: &str, col: usize) -> Option<TargetMatch> {
    target_at_char_col_for_style(text, col, PathStyle::native())
}

/// Return the typed target covering character column `col` for `style`.
#[must_use]
pub fn target_at_char_col_for_style(
    text: &str,
    col: usize,
    style: PathStyle,
) -> Option<TargetMatch> {
    let byte = text.char_indices().nth(col).map(|(byte, _)| byte)?;
    target_at_byte_for_style(text, byte, style)
}

/// Return one contextual bare filesystem component covering character column `col`.
///
/// This API is deliberately separate from [`find_targets_for_style`]: callers
/// must already have pane CWD context and must preserve URI/explicit-path
/// precedence before considering ordinary terminal words as filesystem names.
#[must_use]
pub fn bare_name_at_char_col_for_style(
    text: &str,
    col: usize,
    style: PathStyle,
) -> Option<TargetMatch> {
    let (byte, clicked) = text.char_indices().nth(col)?;
    if is_path_delimiter(clicked) {
        // When: `clicked` is a token delimiter, do not transfer a neighboring component's identity onto this cell.
        return None;
    }
    if target_at_byte_for_style(text, byte, style).is_some() {
        // When: an allow-listed URI or explicit path owns `byte`, its stronger provenance wins.
        return None;
    }
    let start = text[..byte]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| is_path_delimiter(ch).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let end = text[byte..]
        .char_indices()
        .find_map(|(offset, ch)| is_path_delimiter(ch).then_some(byte + offset))
        .unwrap_or(text.len());
    let candidate = text.get(start..end)?;
    let adjacent_wrapper = text[..start]
        .chars()
        .next_back()
        .is_some_and(|ch| is_path_delimiter(ch) && !ch.is_whitespace())
        || text[end..]
            .chars()
            .next()
            .is_some_and(|ch| is_path_delimiter(ch) && !ch.is_whitespace());
    if adjacent_wrapper || !validate_bare_name(candidate, style) {
        // When: wrappers or unsafe component syntax make `candidate` ambiguous, leave it as ordinary text.
        return None;
    }
    Some(TargetMatch { start, end, target: DetectedTarget::BareName(candidate.to_string()) })
}

fn validate_bare_name(candidate: &str, style: PathStyle) -> bool {
    if candidate.is_empty()
        || candidate.len() > MAX_TARGET_BYTES
        || matches!(candidate, "." | "..")
        || candidate.chars().any(char::is_control)
        || candidate.contains(['/', '\\'])
        || candidate.ends_with(['*', '@', '=', '|'])
        || has_editor_location_suffix(candidate)
    {
        // When: `candidate` has unsafe component, decoration, or editor-suffix syntax, keep contextual output inert.
        return false;
    }
    match style {
        PathStyle::Posix => !candidate.contains('\0'),
        PathStyle::Windows => {
            !candidate.contains(':')
                && !candidate.chars().any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
                && !candidate.ends_with(['.', ' '])
        }
    }
}

fn is_path_start_boundary(text: &str, start: usize) -> bool {
    if start == 0 {
        // When: `start` is zero, the candidate begins at a row boundary without requiring a preceding delimiter.
        return true;
    }
    text[..start]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '{' | '<' | '>'))
}

fn is_path_delimiter(ch: char) -> bool {
    ch.is_whitespace() || ch.is_control() || matches!(ch, '"' | '\'' | '`' | '<' | '>')
}

fn trim_matching_wrapper(text: &str, start: usize, mut end: usize) -> usize {
    let opener = text[..start].chars().next_back();
    let closing = match opener {
        Some('(') => Some(')'),
        Some('[') => Some(']'),
        Some('{') => Some('}'),
        _ => None,
    };
    if closing.is_some() && text[..end].chars().next_back() == closing {
        let last = text[..end].char_indices().next_back().map_or(end, |(index, _)| index);
        if last >= start {
            end = last;
        }
    }
    end
}

fn has_path_prefix(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => {
            (candidate.starts_with('/') && !candidate.starts_with("//"))
                || candidate.starts_with("./")
                || candidate.starts_with("../")
                || candidate.starts_with("~/")
                || has_contextual_relative_prefix(candidate, style)
        }
        PathStyle::Windows => {
            let bytes = candidate.as_bytes();
            let drive_absolute = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && is_windows_separator(bytes[2] as char);
            drive_absolute
                || candidate.starts_with("./")
                || candidate.starts_with(".\\")
                || candidate.starts_with("../")
                || candidate.starts_with("..\\")
                || candidate.starts_with("~/")
                || candidate.starts_with("~\\")
                || has_contextual_relative_prefix(candidate, style)
        }
    }
}

fn has_contextual_relative_prefix(candidate: &str, style: PathStyle) -> bool {
    let end = candidate
        .char_indices()
        .find_map(|(index, ch)| is_path_delimiter(ch).then_some(index))
        .unwrap_or(candidate.len());
    let token = &candidate[..end];
    let separator = match style {
        PathStyle::Posix => token.char_indices().find(|(_, ch)| *ch == '/'),
        PathStyle::Windows => token.char_indices().find(|(_, ch)| is_windows_separator(*ch)),
    };
    let Some((separator, _)) = separator else {
        // When: `token` contains no native separator, leave it to contextual bare-name lookup.
        return false;
    };
    let first = &token[..separator];
    let last = match style {
        PathStyle::Posix => token.rsplit('/').next().unwrap_or_default(),
        PathStyle::Windows => token.rsplit(['/', '\\']).next().unwrap_or_default(),
    };
    if first.is_empty()
        || matches!(first, "." | "..")
        || matches!(last, "" | "." | "..")
        || token.chars().any(|ch| {
            matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '<' | '>')
        })
        || token.contains(['~', '$'])
        || (style == PathStyle::Windows && token.contains('%'))
    {
        // When: `first`, `last`, or `token` carries ambiguous wrapper, expansion, or pseudo-component syntax, keep it inert.
        return false;
    }
    !first.contains(':')
}

fn validate_path_candidate(candidate: &str, style: PathStyle) -> bool {
    if candidate.is_empty()
        || candidate.len() > MAX_TARGET_BYTES
        || candidate.chars().any(char::is_control)
        || has_editor_location_suffix(candidate)
    {
        // When: `candidate` is empty, overlong, controlled, or editor-suffixed, it is not a raw filesystem target.
        return false;
    }
    match style {
        PathStyle::Posix => {
            // When: `style` is POSIX, require slash-only local path syntax with at least one named component.
            if candidate.contains('\\') || candidate.starts_with("//") {
                // When: POSIX `candidate` contains a backslash or double-slash root, reject cross-platform and network ambiguity.
                return false;
            }
            let component_text = candidate.strip_prefix("~/").unwrap_or(candidate);
            has_named_component(component_text.split('/'))
        }
        PathStyle::Windows => {
            // When: `style` is Windows, apply drive-path component and reserved-character rules.
            if candidate.starts_with("\\\\") || candidate.starts_with("//") {
                // When: Windows `candidate` begins with a double separator, reject unsupported UNC and network paths.
                return false;
            }
            for (index, ch) in candidate.char_indices() {
                if matches!(ch, '<' | '>' | '"' | '|' | '?' | '*') || (ch == ':' && index != 1) {
                    // When: `ch` is reserved or a colon outside drive `index` 1, reject the Windows candidate.
                    return false;
                }
            }
            let bytes = candidate.as_bytes();
            let component_text = if bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && is_windows_separator(bytes[2] as char)
            {
                // A drive-root prefix is syntax, so validate only the components after it.
                &candidate[3..]
            } else if candidate.starts_with("~/") || candidate.starts_with("~\\") {
                // When: `candidate` starts with current-home syntax, validate only the path below that prefix.
                &candidate[2..]
            } else {
                // When: `bytes` do not begin with a drive or home root, validate the complete explicit-relative candidate.
                candidate
            };
            has_named_component(component_text.split(['/', '\\']))
        }
    }
}

fn has_named_component<'a>(mut components: impl Iterator<Item = &'a str>) -> bool {
    components.any(|component| !component.is_empty() && component != "." && component != "..")
}

fn has_editor_location_suffix(candidate: &str) -> bool {
    let trimmed = candidate.trim_end_matches(['/', '\\']);
    let Some((prefix, last)) = trimmed.rsplit_once(':') else {
        // When: `trimmed` has no colon suffix, it cannot encode an editor line or column location.
        return false;
    };
    if last.is_empty() || !last.bytes().all(|byte| byte.is_ascii_digit()) {
        // When: `last` is empty or nonnumeric, its colon is ordinary filename content rather than an editor location.
        return false;
    }
    prefix
        .rsplit_once(':')
        .is_some_and(|(_, line)| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
        || !matches!(prefix.as_bytes(), [drive] if drive.is_ascii_alphabetic())
}

#[inline]
fn is_windows_separator(ch: char) -> bool {
    matches!(ch, '/' | '\\')
}

#[inline]
fn is_url_body_char(c: char) -> bool {
    // RFC 3986 unreserved + sub-delims + a couple of reserved we
    // commonly see embedded in URLs in the wild, MINUS shell-meta
    // and quote chars that `validate()` rejects.
    matches!(c,
        'a'..='z' | 'A'..='Z' | '0'..='9' |
        '-' | '_' | '.' | '~' |
        '!' | '$' | '*' | '+' | ',' | ';' | '=' |
        ':' | '/' | '?' | '#' | '[' | ']' | '@' |
        '%' | '(' | ')'
    )
}

#[cfg(test)]
#[path = "url_scan_tests.rs"]
mod url_scan_tests;
