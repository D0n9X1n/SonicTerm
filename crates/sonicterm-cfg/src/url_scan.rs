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
    /// Byte offset (exclusive) of the visible target span.
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
const MAX_SPACED_PATH_TOKENS: usize = 8;
const MAX_PATH_CANDIDATES_PER_CELL: usize =
    MAX_SPACED_PATH_TOKENS * (MAX_SPACED_PATH_TOKENS + 1) / 2 + 1;

struct FocusedCandidateGroup {
    token_count: usize,
    source_start: usize,
    source_end: usize,
    has_prose_fallback: bool,
    candidates: Vec<TargetMatch>,
}

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
///
/// Retained for source compatibility. Production callers should select a
/// [`PathStyle`] explicitly so behavior does not depend on the build host.
#[deprecated(since = "1.2.9", note = "use find_targets_for_style with an explicit PathStyle")]
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
        if !is_path_start_boundary(text, start)
            || text[start..].chars().next().is_none_or(is_path_delimiter)
        {
            // When: `start` lacks a token boundary or lands on its delimiter, it cannot begin a raw path candidate.
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
        if !has_path_prefix(candidate, style) || !validate_path_candidate(candidate, style) {
            // When: `candidate` lacks a native prefix or violates its grammar, keep it inert terminal text.
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
///
/// Retained for source compatibility. Production callers should select a
/// [`PathStyle`] explicitly so behavior does not depend on the build host.
#[deprecated(since = "1.2.9", note = "use target_at_byte_for_style with an explicit PathStyle")]
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
///
/// Retained for source compatibility. Production callers should select a
/// [`PathStyle`] explicitly so behavior does not depend on the build host.
#[deprecated(since = "1.2.9", note = "use target_at_char_col_for_style with an explicit PathStyle")]
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

/// Return bounded URI or filesystem candidates covering character column `col`.
///
/// Ordinary ASCII spaces are soft boundaries so an asynchronous filesystem
/// probe can disambiguate existing names containing spaces. Controls, other
/// whitespace, unsafe wrappers, URI spans, and the byte cap remain hard limits.
#[must_use]
pub fn target_candidates_at_char_col_for_style(
    text: &str,
    col: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Vec<TargetMatch> {
    let Some((clicked_byte, clicked)) = text.char_indices().nth(col) else {
        // When: `col` lies beyond the row, no candidate can own the pointed cell.
        return Vec::new();
    };
    let urls = find_urls(text);
    if let Some(url) = urls.iter().find(|url| clicked_byte >= url.start && clicked_byte < url.end) {
        // When: a validated URI owns `clicked_byte`, return only that stronger provenance.
        return vec![TargetMatch {
            start: url.start,
            end: url.end,
            target: DetectedTarget::Uri(url.url.clone()),
        }];
    }
    if is_path_hard_delimiter(clicked) {
        // When: `clicked` is a control, quote, or non-space whitespace, do not bridge its hard boundary.
        return Vec::new();
    }

    let segment_start = text[..clicked_byte]
        .char_indices()
        .rev()
        .find_map(|(index, ch)| is_path_hard_delimiter(ch).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let segment_end = text[clicked_byte..]
        .char_indices()
        .find_map(|(offset, ch)| is_path_hard_delimiter(ch).then_some(clicked_byte + offset))
        .unwrap_or(text.len());
    if let Some(shell_name) =
        shell_quoted_bare_name(text, segment_start, segment_end, style, include_bare_names)
    {
        // When: `shell_quoted_bare_name` returns `shell_name`, expose only its complete unwrapped identity.
        return vec![shell_name];
    }
    if quoted_spaced_segment(text, segment_start, segment_end) {
        // When: `quoted_spaced_segment` is true, reject every partial reconstruction around unsupported quote syntax.
        return Vec::new();
    }
    let tokens = soft_space_token_spans(text, segment_start, segment_end);
    let clicked_token =
        tokens.iter().position(|(start, end)| clicked_byte >= *start && clicked_byte < *end);
    // When: `clicked` is an ordinary space, anchor between named tokens; otherwise `clicked_token` owns the candidate.
    let clicked_gap = if clicked == ' ' {
        let left = tokens.iter().rposition(|(_, end)| *end <= clicked_byte);
        let right = tokens.iter().position(|(start, _)| *start > clicked_byte);
        left.zip(right)
    } else {
        None
    };

    let Some((anchor_start, anchor_end)) =
        clicked_token.map(|index| (index, index)).or(clicked_gap)
    else {
        // When: no named token or bounded inter-token gap owns `clicked_byte`, leave the cell inert.
        return Vec::new();
    };
    let first_left = anchor_end.saturating_add(1).saturating_sub(MAX_SPACED_PATH_TOKENS);
    let mut groups = Vec::new();
    for left_index in first_left..=anchor_start {
        let last_right = tokens
            .len()
            .saturating_sub(1)
            .min(left_index.saturating_add(MAX_SPACED_PATH_TOKENS - 1));
        for right_index in anchor_end.max(left_index)..=last_right {
            let (start, end) =
                trim_outer_path_wrapper(text, tokens[left_index].0, tokens[right_index].1, style);
            if clicked_byte < start || clicked_byte >= end {
                // When: `clicked_byte` falls outside wrapper-trimmed `start..end`, this span cannot represent its target.
                continue;
            }
            let Some(candidate) = text.get(start..end) else {
                // When: `start..end` misses UTF-8 boundaries, reject the malformed candidate span.
                continue;
            };
            if candidate.len() > MAX_TARGET_BYTES
                || escaped_space_path(text, start, candidate, style)
                || unsafe_wrapper_adjacent(text, start, end)
                || urls.iter().any(|url| start < url.end && end > url.start)
            {
                // When: `candidate` exceeds a hard safety boundary or overlaps URI provenance, leave the range inert.
                continue;
            }
            let token_count = right_index - left_index + 1;
            if let Some(group) = focused_candidate_group(
                text,
                clicked_byte,
                style,
                include_bare_names,
                token_count,
                start,
                end,
            ) {
                groups.push(group);
            }
        }
    }

    groups.sort_by(|left, right| {
        right
            .has_prose_fallback
            .cmp(&left.has_prose_fallback)
            .then_with(|| left.token_count.cmp(&right.token_count))
            .then_with(|| {
                (left.source_end - left.source_start).cmp(&(right.source_end - right.source_start))
            })
            .then_with(|| left.source_start.cmp(&right.source_start))
    });
    groups.dedup_by(|right, left| {
        right.source_start == left.source_start && right.source_end == left.source_end
    });

    let mut candidates = Vec::new();
    for group in groups {
        if candidates.len() + group.candidates.len() > MAX_PATH_CANDIDATES_PER_CELL {
            // When: the complete `group` would exceed the cap, skip it rather than orphaning its literal or fallback.
            continue;
        }
        candidates.extend(group.candidates);
    }
    candidates.sort_by(|left, right| {
        (right.end - right.start)
            .cmp(&(left.end - left.start))
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
    });
    candidates
}

fn focused_candidate_group(
    text: &str,
    clicked_byte: usize,
    style: PathStyle,
    include_bare_names: bool,
    token_count: usize,
    start: usize,
    source_end: usize,
) -> Option<FocusedCandidateGroup> {
    let one_trim = text[start..source_end]
        .char_indices()
        .next_back()
        .filter(|(_, ch)| is_prose_path_punctuation(*ch))
        .map(|(offset, _)| start + offset);
    let mut full_trim = source_end;
    while let Some((offset, ch)) = text[start..full_trim].char_indices().next_back() {
        if !is_prose_path_punctuation(ch) {
            // When: `ch` is not prose punctuation, stop before trimming legal filename content.
            break;
        }
        full_trim = start + offset;
    }
    let mut ends = vec![source_end];
    if let Some(one_trim) = one_trim {
        ends.push(one_trim);
    }
    if full_trim < source_end {
        ends.push(full_trim);
    }
    ends.sort_unstable_by(|left, right| right.cmp(left));
    ends.dedup();

    let mut candidates = ends
        .into_iter()
        .filter(|end| clicked_byte < *end)
        .filter_map(|end| {
            let candidate = text.get(start..end)?;
            let target = detected_path_target(candidate, style, include_bare_names)?;
            Some(TargetMatch { start, end, target })
        })
        .collect::<Vec<_>>();
    candidates.dedup_by(|right, left| right.end == left.end && right.target == left.target);
    if candidates.is_empty() {
        // When: `candidates.is_empty()` after grammar filtering, omit the source span entirely.
        return None;
    }
    Some(FocusedCandidateGroup {
        token_count,
        source_start: start,
        source_end,
        has_prose_fallback: candidates.iter().any(|candidate| candidate.end < source_end),
        candidates,
    })
}

fn detected_path_target(
    candidate: &str,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<DetectedTarget> {
    if has_path_prefix(candidate, style) && validate_path_candidate(candidate, style) {
        Some(DetectedTarget::PathCandidate(candidate.to_string()))
    } else if include_bare_names && validate_bare_name(candidate, style) {
        // When: `include_bare_names && validate_bare_name(...)` holds, preserve CWD-only contextual provenance.
        Some(DetectedTarget::BareName(candidate.to_string()))
    } else {
        // When: explicit path and `include_bare_names` predicates reject `candidate`, leave the span inert.
        None
    }
}

fn is_prose_path_punctuation(ch: char) -> bool {
    matches!(ch, ',' | ';' | '.' | ':' | '!' | '?')
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

fn is_path_hard_delimiter(ch: char) -> bool {
    (ch.is_whitespace() && ch != ' ')
        || ch.is_control()
        || matches!(ch, '"' | '\'' | '`' | '<' | '>')
}

fn shell_quoted_bare_name(
    text: &str,
    start: usize,
    end: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<TargetMatch> {
    if !include_bare_names || !text[start..end].contains(' ') {
        // When: `include_bare_names` is false or the segment contains no space, preserve ordinary scanning.
        return None;
    }
    let quote_start = start.checked_sub(1)?;
    if text.as_bytes().get(quote_start) != Some(&b'\'') || text.as_bytes().get(end) != Some(&b'\'')
    {
        // When: `text.as_bytes().get(quote_start)` or `get(end)` is not an ASCII single quote, retain strict rejection.
        return None;
    }
    let left_boundary = text[..quote_start].chars().next_back();
    let right_boundary = text[end + 1..].chars().next();
    if left_boundary.is_some_and(|ch| !ch.is_whitespace())
        || right_boundary.is_some_and(|ch| !ch.is_whitespace())
    {
        // When: `left_boundary` or `right_boundary` is non-whitespace, reject assignment, concatenation, and prose syntax.
        return None;
    }
    let candidate = text.get(start..end)?;
    if candidate.starts_with(' ') || candidate.ends_with(' ') {
        // When: `candidate` has an outer space, reject padded or adjacent quote fragments as ambiguous.
        return None;
    }
    if candidate.split(' ').filter(|part| !part.is_empty()).count() > MAX_SPACED_PATH_TOKENS {
        // When: the quoted candidate exceeds `MAX_SPACED_PATH_TOKENS`, preserve the shared work bound.
        return None;
    }
    validate_bare_name(candidate, style).then(|| TargetMatch {
        start,
        end,
        target: DetectedTarget::BareName(candidate.to_string()),
    })
}

fn quoted_spaced_segment(text: &str, start: usize, end: usize) -> bool {
    let starts_at_content = text[start..end].chars().next().is_some_and(|ch| ch != ' ');
    let ends_at_content = text[start..end].chars().next_back().is_some_and(|ch| ch != ' ');
    let left_quote =
        text[..start].char_indices().next_back().filter(|(_, ch)| matches!(ch, '"' | '\'' | '`'));
    let right_quote = text[end..].chars().next().filter(|ch| matches!(ch, '"' | '\'' | '`'));
    let left_is_wrapper = left_quote.is_some_and(|(index, _)| {
        text[..index]
            .chars()
            .next_back()
            .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '=' | ':' | '(' | '[' | '{' | '<'))
    });
    let right_closes_or_is_unmatched =
        right_quote.is_some_and(|quote| !text[end + quote.len_utf8()..].contains(quote));
    // Quote wrappers make every inner candidate ambiguous, even when spaces pad the delimiters.
    left_is_wrapper
        || right_closes_or_is_unmatched
        || left_quote.is_some() && starts_at_content
        || right_quote.is_some() && ends_at_content
}

fn escaped_space_path(text: &str, start: usize, candidate: &str, style: PathStyle) -> bool {
    style == PathStyle::Posix
        && (candidate.contains("\\ ")
            || text[..start].strip_suffix(' ').is_some_and(|prefix| prefix.ends_with('\\')))
}

fn soft_space_token_spans(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut token_start = None;
    for (offset, ch) in text[start..end].char_indices() {
        let index = start + offset;
        if ch == ' ' {
            if let Some(token_start) = token_start.take() {
                spans.push((token_start, index));
            }
        } else if token_start.is_none() {
            // When: `token_start` is absent at non-space `ch`, record the opening byte of this named run.
            token_start = Some(index);
        }
    }
    if let Some(token_start) = token_start {
        spans.push((token_start, end));
    }
    spans
}

fn unsafe_wrapper_adjacent(text: &str, start: usize, end: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_some_and(|ch| is_path_hard_delimiter(ch) && !ch.is_whitespace())
        || text[end..]
            .chars()
            .next()
            .is_some_and(|ch| is_path_hard_delimiter(ch) && !ch.is_whitespace())
}

fn trim_outer_path_wrapper(
    text: &str,
    mut start: usize,
    mut end: usize,
    style: PathStyle,
) -> (usize, usize) {
    let Some(first) = text[start..end].chars().next() else {
        // When: `text[start..end].chars().next()` is absent, there is no wrapper pair to remove.
        return (start, end);
    };
    let Some(last) = text[start..end].chars().next_back() else {
        // When: `text[start..end].chars().next_back()` is absent, preserve the original boundaries.
        return (start, end);
    };
    if !matches!((first, last), ('(', ')') | ('[', ']') | ('{', '}')) {
        // When: `matches!((first, last), ...)` is false, retain literal punctuation.
        return (start, end);
    }
    start += first.len_utf8();
    end -= last.len_utf8();
    if start >= end || !has_path_prefix(&text[start..end], style) {
        // When: `start >= end || !has_path_prefix(...)`, restore the wrapper as literal filename punctuation.
        return (start - first.len_utf8(), end + last.len_utf8());
    }
    (start, end)
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
    let syntax_end = candidate
        .char_indices()
        .find_map(|(index, ch)| is_path_delimiter(ch).then_some(index))
        .unwrap_or(candidate.len());
    let syntax_prefix = &candidate[..syntax_end];
    let separator = match style {
        PathStyle::Posix => candidate.char_indices().find(|(_, ch)| *ch == '/'),
        PathStyle::Windows => candidate.char_indices().find(|(_, ch)| is_windows_separator(*ch)),
    };
    let Some((separator, _)) = separator else {
        // When: `candidate` contains no native separator, leave it to contextual bare-name lookup.
        return false;
    };
    let first = &candidate[..separator];
    let last = match style {
        PathStyle::Posix => candidate.rsplit('/').next().unwrap_or_default(),
        PathStyle::Windows => candidate.rsplit(['/', '\\']).next().unwrap_or_default(),
    };
    if first.is_empty()
        || matches!(first, "." | "..")
        || matches!(last, "" | "." | "..")
        || syntax_prefix.chars().any(|ch| {
            matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '<' | '>')
        })
        || syntax_prefix.contains(['~', '$'])
        || (style == PathStyle::Windows && syntax_prefix.contains('%'))
    {
        // When: `first`, `last`, or `syntax_prefix` carries ambiguous wrapper, expansion, or pseudo-component syntax, keep it inert.
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
            let components = component_text.split(['/', '\\']).collect::<Vec<_>>();
            has_named_component(components.iter().copied())
                && components.iter().all(|component| {
                    matches!(*component, "" | "." | "..") || !component.ends_with(['.', ' '])
                })
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
