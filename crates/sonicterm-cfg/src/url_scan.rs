//! Plain-text URL detection for terminal grid rows.
//!
//! Scans a row of terminal text and returns the byte ranges that look
//! like links. The scanner is deliberately narrow:
//!
//! - Only `http://`, `https://`, `mailto:` and `file://` schemes are
//!   recognised. `file://` spans are detected so the app can classify
//!   them as local targets; the URI opener ([`crate::url_open::validate`])
//!   refuses every `file:` URI and dispatches only the other three.
//! - URL characters are limited to RFC 3986 unreserved / sub-delims /
//!   reserved minus a handful of shell-meta and quote chars (`<`, `>`,
//!   `"`, `'`, backtick, whitespace, control). This intentionally
//!   under-matches at the edges (e.g. trailing punctuation like `.`
//!   or `)` is trimmed) but the result always passes the opener's
//!   shared lexical URI check.
//! - No regex / `once_cell` dependency: the scanner is a small hand
//!   loop so we can keep `sonicterm-cfg`'s dep surface minimal and avoid
//!   per-frame regex compilation cost.
//!
//! The contract is: every returned `(start, end)` slice passes that shared
//! check with the detection schemes, and every slice that is not a `file:`
//! URI also passes [`crate::url_open::validate`]. Tests below assert this.

use crate::url_open::check_uri;

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
    /// Inclusive source byte offset, retaining delimiters that establish the target boundary.
    pub source_start: usize,
    /// Exclusive source byte offset, retaining removed punctuation but excluding neighboring prose.
    pub source_end: usize,
    /// Literal targets that must be proven missing before this alternative can be selected.
    pub missing_before: Vec<DetectedTarget>,
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

/// Schemes detection recognises. The URI opener dispatches all but `file://`,
/// which the app classifies as a local target instead.
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

/// Return every URL substring of `text` whose scheme detection recognises and
/// which passes the opener's shared URI check. `file://` matches are returned
/// for local-target classification; [`crate::url_open::validate`] refuses them.
pub fn find_urls(text: &str) -> Vec<UrlMatch> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        // Find the next plausible scheme start. We anchor on ASCII
        // letters because every supported scheme begins with one.
        if !bytes[start].is_ascii_alphabetic() {
            // When: `bytes[start]` is not ASCII alphabetic, it cannot start an allow-listed scheme.
            start += 1;
            continue;
        }
        let mut matched_scheme = None;
        for scheme in SCHEMES {
            let scheme_bytes = scheme.as_bytes();
            // Use `text.get(..)` rather than `&text[..]` so a byte
            // range that lands inside a multi-byte UTF-8 char (e.g.
            // `❯` from an oh-my-zsh prompt) returns `None` instead of
            // panicking. Schemes are pure ASCII so a non-boundary end
            // index can never be a real match anyway.
            if let Some(slice) = text.get(start..start + scheme_bytes.len()) {
                // When: `text.get(...)` yields `slice`, the candidate ended on UTF-8 boundaries and can be compared safely.
                if slice.eq_ignore_ascii_case(scheme) {
                    // When: `slice` equals `scheme` ignoring case, retain this scheme length and stop probing alternatives.
                    matched_scheme = Some(scheme_bytes.len());
                    break;
                }
            }
        }
        let Some(scheme_len) = matched_scheme else {
            // When: `matched_scheme` is absent, advance one byte and continue searching for a scheme start.
            start += 1;
            continue;
        };
        // Opening prose wrappers delimit a scheme even though they are legal inside URL paths.
        if start > 0
            && is_url_body_char(bytes[start - 1] as char)
            && !matches!(bytes[start - 1], b'(' | b'[')
        {
            // When: `start` follows a URL-body character, this scheme text is embedded in a larger token.
            start += 1;
            continue;
        }
        let mut end = start + scheme_len;
        while end < bytes.len() && is_url_body_char(bytes[end] as char) {
            end += 1;
        }
        // Trim trailing punctuation that's commonly adjacent to a
        // URL in prose (`)`, `.`, `,`, `;`, `:`, `!`, `?`).
        while end > start + scheme_len {
            let last = bytes[end - 1] as char;
            if matches!(last, ')' | ']' | '.' | ',' | ';' | ':' | '!' | '?') {
                end -= 1;
            } else {
                // When: `matches!(last, ...)` is false, preserve `last` as part of the candidate URL.
                break;
            }
        }
        // Require at least one body byte after the scheme.
        if end <= start + scheme_len {
            // When: `end` contains no body beyond `scheme_len`, skip the empty URL candidate.
            start += scheme_len;
            continue;
        }
        let url = &text[start..end];
        if check_uri(url, SCHEMES).is_ok() {
            out.push(UrlMatch { start, end, url: url.to_string() });
        }
        start = end.max(start + 1);
    }
    out
}

/// Return the URL covering byte offset `byte_col`, if any.
pub fn url_at_byte(text: &str, byte_col: usize) -> Option<UrlMatch> {
    find_url_at(text, byte_col)
}

/// The URL covering byte offset `byte`, found by scanning only the run of URL-body bytes around
/// it. A URL never spans a byte outside that run, and the byte before the run is never a body
/// byte, so the result equals scanning the whole line, without allocating every URL in it.
#[must_use]
pub fn find_url_at(text: &str, byte: usize) -> Option<UrlMatch> {
    let bytes = text.as_bytes();
    if !bytes.get(byte).is_some_and(|value| is_url_body_char(*value as char)) {
        // When: the byte is not a URL body byte, no URL covers it.
        return None;
    }
    let run_start = bytes[..byte]
        .iter()
        .rposition(|value| !is_url_body_char(*value as char))
        .map_or(0, |index| index + 1);
    let run_end = bytes[byte..]
        .iter()
        .position(|value| !is_url_body_char(*value as char))
        .map_or(bytes.len(), |offset| byte + offset);
    find_urls_in(text, run_start, run_end)
        .into_iter()
        .find(|url_match| byte >= url_match.start && byte < url_match.end)
}

/// Every URL in `text[start..end]`, at offsets into `text`. Equals the full scan's URLs inside
/// that range when `start` and `end` sit on bytes no URL spans, as hard delimiters are.
fn find_urls_in(text: &str, start: usize, end: usize) -> Vec<UrlMatch> {
    find_urls(&text[start..end])
        .into_iter()
        .map(|url_match| UrlMatch {
            start: url_match.start + start,
            end: url_match.end + start,
            url: url_match.url,
        })
        .collect()
}

/// Return the URL covering character column `col` (0-based, counting
/// `char`s not bytes — matches the terminal grid model).
pub fn url_at_char_col(text: &str, col: usize) -> Option<UrlMatch> {
    let mut byte = None;
    for (column, (byte_offset, _)) in text.char_indices().enumerate() {
        if column == col {
            // When: character index `column` reaches `col`, retain its UTF-8 byte offset for URL lookup.
            byte = Some(byte_offset);
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
            source_start: url.start,
            source_end: url.end,
            missing_before: Vec::new(),
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
        for (offset, character) in text[start..].char_indices().skip(1) {
            if is_path_delimiter(character) {
                // When: `is_path_delimiter(character)` truncates path syntax, record whether it makes the complete token ambiguous.
                ambiguous_delimiter = character.is_control() || !character.is_whitespace();
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
            source_start: start,
            source_end: end,
            missing_before: Vec::new(),
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
    if let Some(url) = find_url_at(text, clicked_byte) {
        // When: a validated URI owns `clicked_byte`, return only that stronger provenance.
        let source_start =
            text[..url.start].trim_end_matches(['(', '[', '{', '\'', '"', '`']).len();
        let source_end = text.len()
            - text[url.end..]
                .trim_start_matches([',', ';', '.', ':', '!', '?', ')', ']', '}', '\'', '"', '`'])
                .len();
        return vec![TargetMatch {
            start: url.start,
            end: url.end,
            source_start,
            source_end,
            missing_before: Vec::new(),
            target: DetectedTarget::Uri(url.url),
        }];
    }
    if let Some(found) = log_field_candidates(text, clicked_byte, style) {
        // When: an explicit field owns clicked_byte, its value bounds prevent assignment fragments from leaking out.
        return found;
    }
    if let Some(found) = structured_path_candidates(text, clicked_byte, style, include_bare_names) {
        // When: a structure owns clicked_byte, its complete boundary prevents fallback to inner fragments.
        return found;
    }
    if is_path_hard_delimiter(clicked) {
        // When: `clicked` is a control, quote, or non-space whitespace, do not bridge its hard boundary.
        return Vec::new();
    }

    let segment_start = text[..clicked_byte]
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            is_path_hard_delimiter(character).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let segment_end = text[clicked_byte..]
        .char_indices()
        .find_map(|(offset, character)| {
            is_path_hard_delimiter(character).then_some(clicked_byte + offset)
        })
        .unwrap_or(text.len());
    // Every candidate lies inside this hard-delimited segment, and no URL spans a hard delimiter,
    // so the URLs a candidate may overlap are exactly the segment's.
    let urls = find_urls_in(text, segment_start, segment_end);
    if let Some(quoted) = quoted_path_target(text, clicked_byte, segment_start, segment_end, style)
    {
        // When: quoted recognizes an explicit quoted segment, never fall back to partial inner paths.
        return quoted.into_iter().collect();
    }
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
    if let Some(grouped) =
        grouped_source_target(text, clicked_byte, segment_start, segment_end, style)
    {
        // When: grouped recognizes citation syntax, one pointed location owns the complete validated group.
        return grouped.into_iter().collect();
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
            if let Some(mut group) = focused_candidate_group(
                text,
                clicked_byte,
                style,
                include_bare_names,
                token_count,
                start,
                end,
            ) {
                let member =
                    punctuation_list_member(text, clicked_byte, style, include_bare_names, &group)
                        .or_else(|| {
                            let member = prose_leading_member(text, clicked_byte, style, &group)?;
                            group.has_prose_fallback = true;
                            Some(member)
                        });
                if let Some(member) = member {
                    group.candidates.push(member);
                }
                groups.push(group);
            }
        }
    }

    let guarded = groups
        .iter()
        .flat_map(|group| &group.candidates)
        .filter(|candidate| !candidate.missing_before.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    for candidate in groups.iter_mut().flat_map(|group| &mut group.candidates) {
        for member in &guarded {
            if candidate.start >= member.start && candidate.end <= member.end {
                // Every overlapping member retains the literal guards before either group can hit the candidate cap.
                candidate.source_start = candidate.source_start.min(member.source_start);
                candidate.source_end = candidate.source_end.max(member.source_end);
                for literal in &member.missing_before {
                    if !candidate.missing_before.contains(literal) {
                        candidate.missing_before.push(literal.clone());
                    }
                }
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

/// Select conservative failure text from candidates already restricted to the pointed cell.
#[must_use]
pub fn explicit_path_feedback<'a>(
    candidates: impl Iterator<Item = &'a DetectedTarget>,
    style: PathStyle,
) -> Option<&'a str> {
    let mut selected: Option<(&str, bool)> = None;
    for candidate in candidates {
        let (path, display, source) = match candidate {
            DetectedTarget::PathCandidate(path) => (path.as_str(), path.as_str(), false),
            DetectedTarget::SourceReference(reference) => {
                (reference.path.as_str(), reference.display.as_str(), true)
            }
            _ => {
                // When: `candidate` is a URI or unverified bare name, filesystem failure text is not authorized.
                continue;
            }
        };
        let rooted = path.starts_with('/')
            || path.starts_with("~/")
            || path.starts_with("~\\")
            || style == PathStyle::Windows && path.as_bytes().get(1) == Some(&b':');
        let spaced = path.chars().any(char::is_whitespace);
        let crosses_file_word = path.split_whitespace().rev().skip(1).any(|word| {
            word.rsplit(['/', '\\']).next().is_some_and(|name| {
                name.rsplit_once('.')
                    .is_some_and(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
            })
        });
        let ends_in_filename = path.rsplit(['/', '\\']).next().is_some_and(|name| {
            name.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && !extension.is_empty() && !extension.contains(' ')
            })
        });
        if spaced && (!rooted || crosses_file_word || !ends_in_filename) {
            // When: `spaced` lacks `rooted`/`ends_in_filename` evidence or `crosses_file_word`, defer to validation.
            continue;
        }
        if !source && !has_path_prefix(path, style) {
            // When: `path` has neither a source suffix nor explicit path syntax, unverified feedback stays inert.
            continue;
        }
        if selected.is_none_or(|(previous, was_rooted)| {
            rooted && !was_rooted
                || rooted == was_rooted
                    && if rooted {
                        display.len() > previous.len()
                    } else {
                        // When: `rooted` is false, prefer the narrow filename rather than unverified contextual prose.
                        display.len() < previous.len()
                    }
        }) {
            selected = Some((display, rooted));
        }
    }
    selected.map(|(display, _)| display)
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
    let source_start = start;
    let prose_end =
        start + text[start..source_end].trim_end_matches(is_prose_path_punctuation).len();
    // Trailing prose can hide the closer from the initial wrapper pass.
    let (inner_start, inner_end) = if prose_end < source_end {
        trim_outer_path_wrapper(text, start, prose_end, style)
    } else {
        // When: `prose_end` reaches `source_end`, retain the initial wrapper decision.
        (start, source_end)
    };
    // Replacing rejected outer tiers keeps the three-candidate group bound unchanged.
    let (start, candidate_end) = if inner_start > start {
        (inner_start, inner_end)
    } else {
        // When: `inner_start` did not advance past `start`, keep literal filename punctuation candidates.
        (start, source_end)
    };
    if clicked_byte < start
        || clicked_byte >= candidate_end
        || escaped_space_path(text, start, &text[start..candidate_end], style)
        || unsafe_wrapper_adjacent(text, start, candidate_end)
    {
        // When: the exposed `start..candidate_end` loses pointer ownership or safe boundaries, omit its entire group.
        return None;
    }
    let one_trim = text[start..candidate_end]
        .char_indices()
        .next_back()
        .filter(|(_, character)| is_prose_path_punctuation(*character))
        .map(|(offset, _)| start + offset);
    let mut full_trim = candidate_end;
    while let Some((offset, character)) = text[start..full_trim].char_indices().next_back() {
        if !is_prose_path_punctuation(character) {
            // When: `character` is not prose punctuation, stop before trimming legal filename content.
            break;
        }
        full_trim = start + offset;
    }
    let mut ends = vec![candidate_end];
    if let Some(one_trim) = one_trim {
        ends.push(one_trim);
    }
    if full_trim < candidate_end {
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
            Some(TargetMatch {
                start,
                end,
                source_start,
                source_end,
                missing_before: Vec::new(),
                target,
            })
        })
        .collect::<Vec<_>>();
    candidates.dedup_by(|right, left| right.end == left.end && right.target == left.target);
    if candidates.is_empty() {
        // When: `candidates.is_empty()` after grammar filtering, omit the source span entirely.
        return None;
    }
    Some(FocusedCandidateGroup {
        token_count,
        source_start,
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

fn is_prose_path_punctuation(character: char) -> bool {
    matches!(character, ',' | ';' | '.' | ':' | '!' | '?')
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
        .find_map(|(index, character)| {
            is_path_delimiter(character).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let end = text[byte..]
        .char_indices()
        .find_map(|(offset, character)| is_path_delimiter(character).then_some(byte + offset))
        .unwrap_or(text.len());
    let candidate = text.get(start..end)?;
    let adjacent_wrapper = text[..start]
        .chars()
        .next_back()
        .is_some_and(|character| is_path_delimiter(character) && !character.is_whitespace())
        || text[end..]
            .chars()
            .next()
            .is_some_and(|character| is_path_delimiter(character) && !character.is_whitespace());
    if adjacent_wrapper || !validate_bare_name(candidate, style) {
        // When: wrappers or unsafe component syntax make `candidate` ambiguous, leave it as ordinary text.
        return None;
    }
    Some(TargetMatch {
        start,
        end,
        source_start: start,
        source_end: end,
        missing_before: Vec::new(),
        target: DetectedTarget::BareName(candidate.to_string()),
    })
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
    if left_boundary.is_some_and(|character| !character.is_whitespace())
        || right_boundary.is_some_and(|character| !character.is_whitespace())
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
        source_start: quote_start,
        source_end: end + 1,
        missing_before: Vec::new(),
        target: DetectedTarget::BareName(candidate.to_string()),
    })
}

#[inline]
fn is_url_body_char(character: char) -> bool {
    // Keep query separators inside the target while excluding characters the URI check rejects.
    matches!(character,
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

#[path = "url_scan_source.rs"]
mod url_scan_source;
pub use url_scan_source::local_link_target;
use url_scan_source::*;

#[path = "url_scan_structure.rs"]
mod url_scan_structure;
pub use url_scan_structure::is_path_boundary_character;
use url_scan_structure::*;

#[path = "url_scan_prose.rs"]
mod url_scan_prose;
use url_scan_prose::*;

#[path = "url_scan_paths.rs"]
mod url_scan_paths;
use url_scan_paths::*;
