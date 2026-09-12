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

/// One filesystem path carrying a parsed editor location suffix.
///
/// `path` is the filename with the location suffix removed and is the only
/// field a filesystem probe may use; `display` keeps the complete pointed span
/// so hover and selection still cover the suffix the user actually sees.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceReference {
    /// Filesystem path with the location suffix stripped.
    pub path: String,
    /// Complete matched text including the location suffix.
    pub display: String,
    /// First referenced line, always positive.
    pub line: u32,
    /// Referenced column when the suffix carried `:line:column`.
    pub column: Option<u32>,
    /// Final line of an ascending `:start-end` range.
    pub end_line: Option<u32>,
    /// Whether `path` satisfied explicit native path syntax rather than a contextual bare name.
    pub explicit_path: bool,
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
    /// A filesystem path qualified by a parsed editor line/column location.
    SourceReference(SourceReference),
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
        // Opening prose wrappers delimit a scheme even though they are legal inside URL paths.
        if i > 0 && is_url_body_char(bytes[i - 1] as char) && !matches!(bytes[i - 1], b'(' | b'[') {
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

/// Outcome of reading a trailing editor location suffix off one candidate.
enum SourceSuffix {
    /// The candidate carries no numeric-looking location suffix at all.
    NotSource,
    /// A numeric-looking suffix that does not parse; the whole candidate stays inert.
    Malformed,
    /// A fully parsed source reference.
    Source(SourceReference),
}

/// Parse one positive `u32`, rejecting zero, overflow, signs, and non-digits.
fn parse_positive(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        // When: `text` is empty or carries a sign, separator, or non-digit, it is not a location number.
        return None;
    }
    text.parse::<u32>().ok().filter(|value| *value > 0)
}

/// Parse one ascending `start-end` or `start–end` line range.
fn parse_line_range(text: &str) -> Option<(u32, u32)> {
    let (start, end) = text.split_once(['-', '\u{2013}'])?;
    let start = parse_positive(start)?;
    let end = parse_positive(end)?;
    // A descending range cannot describe a source span, so only ascending pairs survive.
    (end >= start).then_some((start, end))
}

/// Whether `prefix` is a bare Windows drive letter rather than a filename.
///
/// `C:12` must keep its existing inert or bare-name reading instead of being
/// reinterpreted as line 12 of a file named `C`.
fn is_drive_letter(prefix: &str) -> bool {
    matches!(prefix.as_bytes(), [letter] if letter.is_ascii_alphabetic())
}

/// Read `prefix` and the digit-leading `last` segment as one location tuple.
///
/// Returns `(path, line, column, end_line)`, or `None` when the numeric-looking
/// suffix does not parse, which the caller turns into an inert candidate.
fn split_location<'a>(
    prefix: &'a str,
    last: &str,
) -> Option<(&'a str, u32, Option<u32>, Option<u32>)> {
    let Some(trailing) = parse_positive(last) else {
        // When: `last` is not a positive number, only an ascending `start-end` range can still parse.
        let (start, end) = parse_line_range(last)?;
        return Some((prefix, start, None, Some(end)));
    };
    // A `:line:column` suffix spans two segments, so `trailing` is the column only
    // when a second numeric segment precedes it.
    let Some((head, middle)) = prefix.rsplit_once(':') else {
        // When: `prefix` holds no second segment, `trailing` is itself the line number.
        return Some((prefix, trailing, None, None));
    };
    if is_drive_letter(head) || !middle.starts_with(|ch: char| ch.is_ascii_digit()) {
        // When: `head` is a drive letter or `middle` is ordinary filename text, `trailing` is the line number.
        return Some((prefix, trailing, None, None));
    }
    // A `middle` that looks numeric but does not parse rejects the whole
    // candidate rather than silently dropping the column.
    let line = parse_positive(middle)?;
    Some((head, line, Some(trailing), None))
}

/// Split one trailing `:line`, `:line:column`, or `:start-end` suffix off `candidate`.
///
/// Returns [`SourceSuffix::Malformed`] rather than `NotSource` once a suffix
/// looks numeric, so an invalid location can never decay into a literal
/// filename that a probe would then open.
fn parse_source_reference(
    candidate: &str,
    style: PathStyle,
    include_bare_names: bool,
) -> SourceSuffix {
    let Some((prefix, last)) = candidate.rsplit_once(':') else {
        // When: `candidate` has no colon, no location suffix can be present.
        return SourceSuffix::NotSource;
    };
    if !last.starts_with(|ch: char| ch.is_ascii_digit()) {
        // When: last is nonnumeric, a preceding numeric segment still makes this a malformed location.
        if prefix.rsplit_once(':').is_some_and(|(head, segment)| {
            !is_drive_letter(head) && segment.starts_with(|ch: char| ch.is_ascii_digit())
        }) {
            // When: prefix contains a numeric location segment, never fall back to a literal path with an invalid column.
            return SourceSuffix::Malformed;
        }
        return SourceSuffix::NotSource;
    }
    if is_drive_letter(prefix) {
        // When: `prefix` is a lone drive letter, preserve the existing drive reading of `C:12`.
        return SourceSuffix::NotSource;
    }

    let Some((path, line, column, end_line)) = split_location(prefix, last) else {
        // When: `last` opens with a digit but parses as neither number nor ascending range, stay inert.
        return SourceSuffix::Malformed;
    };

    let explicit_path = has_path_prefix(path, style) && validate_path_candidate(path, style);
    if !(explicit_path || include_bare_names && validate_bare_name(path, style)) {
        // When: `path` satisfies neither explicit nor permitted contextual grammar, the reference is inert.
        return SourceSuffix::Malformed;
    }
    SourceSuffix::Source(SourceReference {
        path: path.to_string(),
        display: candidate.to_string(),
        line,
        column,
        end_line,
        explicit_path,
    })
}

/// Classify an exact local link destination; malformed local syntax must never fall back to a URI opener.
pub fn local_link_target(
    destination: &str,
    style: PathStyle,
) -> Result<Option<DetectedTarget>, &'static str> {
    let is_file = destination.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("file:"));
    let bytes = destination.as_bytes();
    let drive = style == PathStyle::Windows
        && bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && is_windows_separator(bytes[2] as char);
    let posix = style == PathStyle::Posix && destination.starts_with('/');
    if !is_file && !drive && !posix {
        // When: is_file, drive and posix are false, leave nonlocal schemes to URI policy.
        return Ok(None);
    }
    if destination.len() > MAX_TARGET_BYTES || destination.chars().any(char::is_control) {
        // When: destination is oversized or controlled, reject before decoding or allocating probe state.
        return Err("invalid local destination");
    }
    if let Some((path, fragment)) = destination.rsplit_once('#') {
        // When: destination includes a fragment, distinguish line metadata from literal native filename content.
        let location = fragment.strip_prefix('L').unwrap_or(fragment);
        if location.starts_with(|ch: char| ch.is_ascii_digit()) {
            // When: fragment denotes a line location, separate it before file URI decoding and filesystem resolution.
            if path.contains('#') {
                // When: path contains another fragment delimiter, reject ambiguous nested locations without recursion.
                return Err("ambiguous local line fragment");
            }
            let (line, end_line) = if let Some((start, end)) = location.split_once('-') {
                // When: location contains a range, validate both endpoints before creating source metadata.
                let start = parse_positive(start).ok_or("invalid line fragment")?;
                let end = parse_positive(end.strip_prefix('L').unwrap_or(end))
                    .ok_or("invalid line fragment")?;
                if end < start {
                    // When: end precedes start, do not turn a malformed range into a filename.
                    return Err("invalid line range");
                }
                (start, Some(end))
            } else {
                // When: location contains no range delimiter, require one positive line number.
                (parse_positive(location).ok_or("invalid line fragment")?, None)
            };
            let Some(DetectedTarget::PathCandidate(path)) = local_link_target(path, style)? else {
                // When: the base is not one literal local path, refuse conflicting location forms.
                return Err("invalid line fragment path");
            };
            return Ok(Some(DetectedTarget::SourceReference(SourceReference {
                path,
                display: destination.to_owned(),
                line,
                column: None,
                end_line,
                explicit_path: true,
            })));
        }
    }
    if !is_file {
        // When: is_file is false, percent characters are literal filename content.
        if destination == "/" || (drive && destination[2..].chars().all(is_windows_separator)) {
            // When: destination or drive denotes only a root, retain directory navigation without a named component.
            return Ok(Some(DetectedTarget::PathCandidate(destination.to_owned())));
        }
        return detected_path_target(destination, style, false)
            .map(Some)
            .ok_or("invalid native destination");
    }
    let body = &destination[5..];
    let encoded = if let Some(authority_path) = body.strip_prefix("//") {
        // When: body starts with an authority delimiter, require a local authority before decoding the path.
        let (authority, path) = authority_path.split_once('/').ok_or("file URI has no path")?;
        if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
            // When: authority is not explicitly local, never reinterpret a remote share as a local path.
            return Err("nonlocal file URI");
        }
        path
    } else {
        // When: body has no authority prefix, accept only an explicit Windows drive-rooted file: path.
        let bytes = body.as_bytes();
        if style != PathStyle::Windows
            || bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'/'
        {
            // When: style or bytes do not specify a Windows drive root, reject relative file: destinations.
            return Err("file URI requires an absolute local path");
        }
        body
    };
    if encoded.contains(['?', '#', '\\']) {
        // When: encoded contains ambiguous URI delimiters, require percent-encoded filename bytes instead.
        return Err("ambiguous file URI path");
    }
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            // When: byte is percent, consume exactly one encoded byte without recursive decoding.
            let high = bytes.next().and_then(|b| (b as char).to_digit(16));
            let low = bytes.next().and_then(|b| (b as char).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                // When: high or low is missing, reject incomplete or nonhex percent encoding before any probe.
                return Err("invalid percent encoding");
            };
            let byte = (high * 16 + low) as u8;
            if matches!(byte, b'/' | b'\\') {
                // When: matches! identifies slash or backslash in byte, reject encoded path structure before normalization.
                return Err("encoded file URI separator");
            }
            decoded.push(byte);
        } else {
            // When: byte is not percent, preserve the original UTF-8 byte.
            decoded.push(byte);
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| "file URI is not UTF-8")?;
    if decoded.split('/').any(|component| matches!(component, "." | "..")) {
        // When: decoded has dot components, reject URI traversal instead of hiding it through lexical normalization.
        return Err("file URI traversal");
    }
    let path = match style {
        PathStyle::Posix => format!("/{decoded}"),
        PathStyle::Windows => decoded,
    };
    let absolute = match style {
        PathStyle::Posix => path.starts_with('/') && !path.starts_with("//"),
        PathStyle::Windows => {
            let bytes = path.as_bytes();
            bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'/'
        }
    };
    let root = path == "/"
        || (style == PathStyle::Windows && absolute && path[2..].bytes().all(|byte| byte == b'/'));
    if !absolute || !(root || validate_path_candidate(&path, style)) {
        // When: decoded path violates native absolute grammar, block aliases, controls and stream syntax before probing.
        return Err("invalid file URI path");
    }
    Ok(Some(DetectedTarget::PathCandidate(path)))
}

fn detected_path_target(
    candidate: &str,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<DetectedTarget> {
    match parse_source_reference(candidate, style, include_bare_names) {
        SourceSuffix::Source(reference) => {
            // When: SourceSuffix::Source carries a location, its provenance outranks a literal reading of the span.
            return Some(DetectedTarget::SourceReference(reference));
        }
        SourceSuffix::Malformed => {
            // When: SourceSuffix::Malformed rejects location syntax, refuse the literal-filename fallback.
            return None;
        }
        SourceSuffix::NotSource => {
            // When: SourceSuffix::NotSource finds no location, continue with the unchanged literal path grammar.
        }
    }
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
    // Keep query separators inside the target while excluding characters validate() rejects.
    matches!(c,
        'a'..='z' | 'A'..='Z' | '0'..='9' |
        '-' | '_' | '.' | '~' |
        '!' | '$' | '&' | '*' | '+' | ',' | ';' | '=' |
        ':' | '/' | '?' | '#' | '[' | ']' | '@' |
        '%' | '(' | ')'
    )
}

#[cfg(test)]
#[path = "url_scan_tests.rs"]
mod url_scan_tests;
