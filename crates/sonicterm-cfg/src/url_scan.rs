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
        if check_uri(url, SCHEMES).is_ok() {
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
    let urls = find_urls(text);
    if let Some(url) = urls.iter().find(|url| clicked_byte >= url.start && clicked_byte < url.end) {
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
            target: DetectedTarget::Uri(url.url.clone()),
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
        .find_map(|(index, ch)| is_path_hard_delimiter(ch).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let segment_end = text[clicked_byte..]
        .char_indices()
        .find_map(|(offset, ch)| is_path_hard_delimiter(ch).then_some(clicked_byte + offset))
        .unwrap_or(text.len());
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
        .filter(|(_, ch)| is_prose_path_punctuation(*ch))
        .map(|(offset, _)| start + offset);
    let mut full_trim = candidate_end;
    while let Some((offset, ch)) = text[start..full_trim].char_indices().next_back() {
        if !is_prose_path_punctuation(ch) {
            // When: `ch` is not prose punctuation, stop before trimming legal filename content.
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
    Some(TargetMatch {
        start,
        end,
        source_start: start,
        source_end: end,
        missing_before: Vec::new(),
        target: DetectedTarget::BareName(candidate.to_string()),
    })
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

fn punctuation_list_member(
    text: &str,
    clicked: usize,
    style: PathStyle,
    include_bare_names: bool,
    group: &FocusedCandidateGroup,
) -> Option<TargetMatch> {
    use unicode_general_category::{get_general_category, GeneralCategory};
    let literal = group.candidates.first()?;
    if !literal.missing_before.is_empty()
        || !matches!(literal.target, DetectedTarget::PathCandidate(_) | DetectedTarget::BareName(_))
    {
        // When: literal is already guarded or not a filesystem target, avoid recursive list interpretation.
        return None;
    }
    let body = &text[literal.start..literal.end];
    let separators = body
        .char_indices()
        .filter(|(_, ch)| {
            get_general_category(*ch) == GeneralCategory::OtherPunctuation
                && !matches!(
                    ch,
                    '/' | '\\'
                        | '.'
                        | ':'
                        | '\''
                        | '"'
                        | '`'
                        | '%'
                        | '?'
                        | '#'
                        | '_'
                        | '@'
                        | '&'
                        | '!'
                        | '*'
                )
        })
        .collect::<Vec<_>>();
    if separators.is_empty() || separators.len() >= MAX_SPACED_PATH_TOKENS {
        // When: separators are absent or exceed the member budget, preserve the literal without generating alternatives.
        return None;
    }
    let mut start = 0;
    let mut selected = None;
    for (end, width) in separators
        .into_iter()
        .map(|(i, ch)| (i, ch.len_utf8()))
        .chain(std::iter::once((body.len(), 0)))
    {
        let raw = &body[start..end];
        let value = raw.trim_matches(' ');
        let left = literal.start + start + raw.len() - raw.trim_start_matches(' ').len();
        let right = left + value.len();
        let target = detected_path_target(value, style, include_bare_names)?;
        let path = match &target {
            DetectedTarget::PathCandidate(path) | DetectedTarget::BareName(path) => path,
            DetectedTarget::SourceReference(reference) => &reference.path,
            DetectedTarget::Uri(_) => {
                // When: target is Uri, list punctuation cannot reinterpret its stronger provenance as a filesystem member.
                return None;
            }
        };
        let name = path.rsplit(['/', '\\']).next()?;
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty()
            || extension.is_empty()
            || extension.chars().any(|ch| !ch.is_alphanumeric())
        {
            // When: stem or extension is absent or malformed, these fragments cannot prove a file-list boundary.
            return None;
        }
        if (left..right).contains(&clicked) {
            selected = Some(TargetMatch {
                start: left,
                end: right,
                source_start: literal.source_start,
                source_end: literal.source_end,
                missing_before: vec![literal.target.clone()],
                target,
            });
        }
        start = end + width;
    }
    selected
}

fn prose_leading_member(
    text: &str,
    clicked: usize,
    style: PathStyle,
    group: &FocusedCandidateGroup,
) -> Option<TargetMatch> {
    use unicode_general_category::{get_general_category, GeneralCategory};
    let literal = group.candidates.first()?;
    if group.token_count != 1
        || !literal.missing_before.is_empty()
        || !matches!(literal.target, DetectedTarget::PathCandidate(_))
    {
        // When: group is spaced, guarded, or non-path, prose cannot reinterpret its existing candidate identity.
        return None;
    }
    let body = &text[literal.start..literal.end];
    let explicit = match style {
        PathStyle::Posix => {
            body.starts_with("~/")
                || body.starts_with("./")
                || body.starts_with("../")
                || body.starts_with('/') && !body.starts_with("//")
        }
        PathStyle::Windows => {
            body.starts_with("~/")
                || body.starts_with("~\\")
                || body.starts_with("./")
                || body.starts_with(".\\")
                || body.starts_with("../")
                || body.starts_with("..\\")
                || matches!(body.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
        }
    };
    if !explicit {
        // When: body lacks an explicit home, root, or dot prefix, neighboring prose cannot create a contextual target.
        return None;
    }
    let (offset, _) = body.char_indices().find(|(_, ch)| {
        get_general_category(*ch) == GeneralCategory::OtherPunctuation
            && !matches!(
                ch,
                '/' | '\\'
                    | '.'
                    | ':'
                    | '\''
                    | '"'
                    | '`'
                    | '%'
                    | '?'
                    | '#'
                    | '_'
                    | '@'
                    | '&'
                    | '!'
                    | '*'
            )
    })?;
    let end = literal.start + offset;
    if !(literal.start..end).contains(&clicked) {
        // When: clicked reaches the separator or prose, the leading path cannot own that pointer.
        return None;
    }
    let target = detected_path_target(&body[..offset], style, false)?;
    if !matches!(target, DetectedTarget::PathCandidate(_)) {
        // When: matches! rejects PathCandidate, prose cannot change source-reference or URI precedence.
        return None;
    }
    Some(TargetMatch {
        start: literal.start,
        end,
        source_start: literal.source_start,
        source_end: literal.source_end,
        missing_before: vec![literal.target.clone()],
        target,
    })
}

fn log_field_value_start(text: &str, start: usize) -> Option<usize> {
    if start > 0 && !text[..start].ends_with(char::is_whitespace) {
        // When: start follows non-whitespace text, it cannot introduce an independent field key.
        return None;
    }
    let mut length = 0;
    for (index, byte) in text[start..].bytes().enumerate() {
        if byte == b'=' && index > 0 {
            // When: byte ends a nonempty identifier, the following byte starts its field value.
            return Some(start + index + 1);
        }
        if !(byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()) {
            // When: byte is not an identifier character at index, do not split arbitrary filename text on equals.
            return None;
        }
        length += 1;
        if length > 64 {
            // When: length exceeds the field-key budget, preserve the ordinary bounded path scan instead.
            return None;
        }
    }
    None
}

fn log_field_candidates(text: &str, clicked: usize, style: PathStyle) -> Option<Vec<TargetMatch>> {
    for (start, _) in text.char_indices().take_while(|(index, _)| *index <= clicked) {
        let Some(value_start) = log_field_value_start(text, start) else {
            // When: start is not a field key boundary, continue searching without claiming its text.
            continue;
        };
        let first = text[value_start..].chars().next()?;
        let quoted = matches!(first, '\'' | '"');
        // When: quoted is true, first belongs to field syntax rather than to the path body.
        let body_start = value_start + if quoted { first.len_utf8() } else { 0 };
        let body = &text[body_start..];
        let rooted = match style {
            PathStyle::Windows => {
                matches!(body.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
            }
            PathStyle::Posix => body.starts_with('/') && !body.starts_with("//"),
        };
        if !rooted {
            // When: body is not rooted, an assignment alone cannot promote it into an explicit path.
            continue;
        }
        let (body_end, source_end) = if quoted {
            // When: quoted is true, only its matching closer can end the literal value.
            let Some(close) = body.find(first) else {
                // When: body lacks the matching first quote, its inner text cannot become a separate path.
                return (clicked >= value_start).then(Vec::new);
            };
            let end = body_start + close;
            let after = end + first.len_utf8();
            if text[after..].chars().next().is_some_and(|ch| !ch.is_whitespace()) {
                // When: text after the closing quote is concatenated, reject the partial quoted value.
                return (clicked >= value_start && clicked <= after).then(Vec::new);
            }
            (end, after)
        } else {
            // When: quoted is false, stop at a proven next field while preserving spaces as filename candidates.
            let end = text[body_start..]
                .char_indices()
                .find_map(|(offset, ch)| {
                    let index = body_start + offset;
                    (ch.is_control() || log_field_value_start(text, index).is_some())
                        .then_some(index)
                })
                .unwrap_or(text.len());
            (text[..end].trim_end_matches(' ').len(), end)
        };
        if clicked < start || clicked >= source_end {
            // When: clicked lies outside this field source, a different field may own the pointer.
            continue;
        }
        if clicked < body_start || clicked >= body_end || body_end - body_start > MAX_TARGET_BYTES {
            // When: clicked hits syntax or body exceeds MAX_TARGET_BYTES, claim the field without exposing a partial target.
            return Some(Vec::new());
        }
        let value = &text[body_start..body_end];
        let mut found = if quoted {
            // When: quoted is true, validate value atomically rather than enumerating shorter space-delimited candidates.
            if value.starts_with(' ')
                || value.ends_with(' ')
                || value.contains(['$', '`'])
                || style == PathStyle::Windows && value.contains('%')
                || escaped_space_path(text, body_start, value, style)
                || value.chars().any(char::is_control)
            {
                // When: value contains padding, expansion, escaping, or controls, it cannot identify a literal quoted path.
                return Some(Vec::new());
            }
            detected_path_target(value, style, false)
                .map(|target| TargetMatch {
                    start: 0,
                    end: value.len(),
                    source_start: 0,
                    source_end: value.len(),
                    missing_before: Vec::new(),
                    target,
                })
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            // When: quoted is false, reuse the bounded candidate scanner with a value-relative pointer.
            let column = value[..clicked - body_start].chars().count();
            target_candidates_at_char_col_for_style(value, column, style, false)
        };
        for matched in &mut found {
            matched.start += body_start;
            matched.end += body_start;
            matched.source_start = start;
            matched.source_end = source_end;
        }
        return Some(found);
    }
    None
}

fn presentation_pair(ch: char) -> Option<char> {
    match ch {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' | '"' | '`' => Some(ch),
        '（' => Some('）'),
        '【' => Some('】'),
        '《' => Some('》'),
        '「' => Some('」'),
        '『' => Some('』'),
        '“' => Some('”'),
        '‘' => Some('’'),
        '«' => Some('»'),
        _ => None,
    }
}

fn presentation_closer(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}' | '）' | '】' | '》' | '」' | '』' | '”' | '’' | '»')
}

fn outer_separator(ch: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};
    !matches!(ch, '/' | '\\' | '\'' | '"' | '`')
        && matches!(
            get_general_category(ch),
            GeneralCategory::OtherPunctuation | GeneralCategory::DashPunctuation
        )
}

/// Whether a source-only scalar can be a structural delimiter or prose separator, never path content.
#[must_use]
pub fn is_path_boundary_character(ch: char) -> bool {
    presentation_pair(ch).is_some() || presentation_closer(ch) || outer_separator(ch)
}

fn structure_tail_end(text: &str, end: usize) -> Option<usize> {
    let mut cursor = end;
    for (offset, ch) in text[end..].char_indices() {
        if !outer_separator(ch) {
            // When: ch ends the separator run, leave neighboring prose outside the validation span.
            break;
        }
        cursor = end + offset + ch.len_utf8();
    }
    let next = text[cursor..].chars().next();
    if cursor == end {
        // When: cursor equals end, no separator proves independence from adjacent filename text.
        return next.is_none_or(char::is_whitespace).then_some(end);
    }
    if text[end..cursor].ends_with(['.', ':']) && next.is_some_and(|ch| !ch.is_whitespace()) {
        // When: text ends in a dot or colon before next, preserve filename/location continuations rather than truncating them.
        return None;
    }
    if next.is_some_and(|ch| ch.is_control() || matches!(ch, '/' | '\\') || presentation_closer(ch))
    {
        // When: next continues path structure or a malformed wrapper, punctuation cannot authorize an inner prefix.
        return None;
    }
    Some(cursor)
}

fn structure_close(text: &str, open: usize, opener: char, closer: char) -> Option<usize> {
    let start = open + opener.len_utf8();
    let quoted = matches!(opener, '\'' | '"' | '`' | '“' | '‘' | '«');
    let mut stack = vec![closer];
    for (offset, ch) in text[start..].char_indices() {
        if offset > MAX_TARGET_BYTES || ch.is_control() {
            // When: offset exceeds the byte budget or ch is controlled, incomplete source cannot authorize a path.
            return None;
        }
        if Some(&ch) == stack.last() {
            // When: ch closes stack's current pair, only the final pop establishes the outer boundary.
            stack.pop();
            if stack.is_empty() {
                // When: stack is empty, offset names the proven outer closer rather than a filename's inner bracket.
                return Some(start + offset);
            }
        } else if !quoted && !stack.last().is_some_and(|ch| matches!(ch, '\'' | '"' | '`')) {
            // When: quoted is false and stack is outside quotes, paired filename brackets stay inside the outer structure.
            if let Some(close) = presentation_pair(ch) {
                // When: presentation_pair finds close, track it without selecting an inner filename fragment.
                if stack.len() == MAX_SPACED_PATH_TOKENS {
                    // When: stack reaches the nesting bound, reject rather than scan unbounded structure.
                    return None;
                }
                stack.push(close);
            } else if presentation_closer(ch) {
                // When: presentation_closer finds a different closer, never repair mismatched source text.
                return None;
            }
        } else if matches!(ch, '\'' | '"' | '`' | '“' | '”' | '‘' | '’') {
            // When: matches! finds another quote in ch before closure, refuse shell-like concatenation or escaping.
            return None;
        }
    }
    None
}

fn structure_prefix_start(text: &str, floor: usize, open: usize, opener: char) -> Option<usize> {
    let prefix = &text[floor..open];
    let word_start = floor
        + prefix
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + prefix[i..].chars().next().unwrap().len_utf8());
    let word = &text[word_start..open];
    let call = opener == '('
        && !word.is_empty()
        && word
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit());
    if !word.is_empty() && !call {
        // When: word is not an identifier call, retain its prefix rather than treating a suffix as independent.
        return None;
    }
    let rooted_prefix = text[floor..word_start].split_whitespace().any(|word| {
        word.starts_with(['/', '\\'])
            || word.starts_with("~/")
            || word.starts_with("~\\")
            || word.starts_with("./")
            || word.starts_with(".\\")
            || word.starts_with("../")
            || word.starts_with("..\\")
            || matches!(word.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
    });
    if rooted_prefix {
        // When: rooted_prefix proves an unconsumed explicit anchor, preserve its spaced name instead of exposing an inner suffix.
        return None;
    }
    Some(if call { word_start } else { open })
}

fn structured_path_candidates(
    text: &str,
    clicked: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<Vec<TargetMatch>> {
    let mut floor = 0;
    let mut skip_until = 0;
    for (open, opener) in text.char_indices() {
        if open < skip_until {
            // When: open belongs to a previously consumed pair, do not promote nested presentation fragments.
            continue;
        }
        if opener.is_control() || (opener.is_whitespace() && opener != ' ') {
            // When: opener is a hard delimiter, prior path anchors cannot span it.
            floor = open + opener.len_utf8();
            continue;
        }
        let Some(closer) = presentation_pair(opener) else {
            // When: opener has no presentation pair, ordinary text does not establish structure.
            continue;
        };
        let start = open + opener.len_utf8();
        let Some(close) = structure_close(text, open, opener, closer) else {
            // When: an incomplete structure encloses explicit path text, prevent a suffix from becoming another target.
            if clicked >= start
                && !text[start..].starts_with(char::is_whitespace)
                && structure_prefix_start(text, floor, open, opener).is_some()
                && has_path_prefix(&text[start..], style)
            {
                // When: clicked lies inside an incomplete explicit path structure, never recover a partial destination.
                return Some(Vec::new());
            }
            continue;
        };
        let end = close + closer.len_utf8();
        skip_until = end;
        let body = &text[start..close];
        let owns = (open..end).contains(&clicked);
        let source_start = structure_prefix_start(text, floor, open, opener);
        let source_end = structure_tail_end(text, end);
        if clicked >= end && source_end.is_some_and(|tail| clicked < tail) {
            // When: clicked is in the separator run, it must not activate the enclosed filename.
            return Some(Vec::new());
        }
        if !owns {
            // When: owns is false, retain only completed independent structures as lookbehind boundaries.
            if source_start.is_some() && source_end.is_some() {
                // Both validated source bounds separate this completed anchor from the next target.
                floor = end;
            }
            continue;
        }
        // When: body is an existing quote-in-wrapper form, its quote parser retains the full outer safety span.
        if body.starts_with(['\'', '"', '`']) && body.ends_with(body.chars().next().unwrap()) {
            return None;
        }
        if !has_path_prefix(body, style) {
            // When: body lacks path syntax, distinguish literal brackets and supported quoted listing names.
            if source_start.is_none()
                || text[floor..open].contains(['/', '\\'])
                || opener == '\'' && include_bare_names && !body.contains(['/', '\\'])
            {
                // When: source_start is absent or opener encloses a bare listing name, preserve the existing literal scanner.
                return None;
            }
            return Some(Vec::new());
        }
        let (Some(source_start), Some(source_end)) = (source_start, source_end) else {
            // When: source_start or source_end is unproven, no inner prefix may acquire filesystem authority.
            return Some(Vec::new());
        };
        if clicked < start
            || clicked >= close
            || body.len() > MAX_TARGET_BYTES
            || body.starts_with(' ')
            || body.ends_with(' ')
            || body.split(' ').filter(|word| !word.is_empty()).count() > MAX_SPACED_PATH_TOKENS
            || find_urls(body).iter().any(|url| url.start < body.len())
            || body.chars().any(char::is_control)
            || (matches!(opener, '\'' | '"' | '`' | '“' | '‘' | '«')
                && (body.contains(['$', '%']) || escaped_space_path(text, start, body, style)))
        {
            // When: clicked misses body or body violates bounds, provenance, or literal quote rules, reject the entire structure.
            return Some(Vec::new());
        }
        let inner_clicked = clicked - start;
        let grouped = body.char_indices().find_map(|(i, ch)| {
            (ch == ',' && body[i + 1..].trim_start_matches(' ').starts_with(':')).then_some(i)
        });
        let mut found = if let Some(comma) = grouped {
            // When: grouped supplies comma, validate every location under the same complete path anchor.
            if body[..comma].contains(' ') {
                // When: body before comma contains spaces, never reinterpret its final word as a shorter anchor.
                return Some(Vec::new());
            }
            parse_source_group(body, inner_clicked, 0, comma, body.len(), style)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            // When: grouped is absent, preserve literal-first punctuation alternatives within the structure.
            focused_candidate_group(body, inner_clicked, style, false, 1, 0, body.len())
                .map_or_else(Vec::new, |group| group.candidates)
        };
        for matched in &mut found {
            matched.start += start;
            matched.end += start;
            matched.source_start = source_start;
            matched.source_end = source_end;
        }
        return Some(found);
    }
    None
}

fn quoted_path_target(
    text: &str,
    clicked: usize,
    start: usize,
    end: usize,
    style: PathStyle,
) -> Option<Option<TargetMatch>> {
    let open = text[..start].chars().next_back()?;
    if !matches!(open, '\'' | '"' | '`') {
        // When: matches rejects the presentation quote opener, leave ordinary scanning unchanged.
        return None;
    }
    let candidate = &text[start..end];
    if candidate.starts_with(' ') || !has_path_prefix(candidate, style) {
        // When: candidate is not immediate explicit path content, preserve the closing-quote and bare-name cases.
        return None;
    }
    let quote_start = start - open.len_utf8();
    let close_end = end + open.len_utf8();
    if !text[end..].starts_with(open)
        || candidate.len() > MAX_TARGET_BYTES
        || candidate.ends_with(' ')
        || candidate.contains(['$', '%'])
        || candidate.chars().any(is_path_hard_delimiter)
        || escaped_space_path(text, start, candidate, style)
        || !structured_outer_boundary(text, quote_start, close_end)
    {
        // When: candidate or its open/close boundaries are ambiguous, reject without reconstructing a partial path.
        return Some(None);
    }
    let target = detected_path_target(candidate, style, false)?;
    let enclosed = text[..quote_start].ends_with(['(', '[', '{']);
    let source_start = quote_start - usize::from(enclosed);
    let wrapper_end = close_end + usize::from(enclosed);
    let source_end = wrapper_end + text[wrapper_end..].len()
        - text[wrapper_end..].trim_start_matches(is_prose_path_punctuation).len();
    Some((start <= clicked && clicked < end).then_some(TargetMatch {
        start,
        end,
        source_start,
        source_end,
        missing_before: Vec::new(),
        target,
    }))
}

fn structured_outer_boundary(text: &str, start: usize, end: usize) -> bool {
    let left = text[..start].chars().next_back();
    let right = text[end..].chars().next();
    let closer = match left {
        Some('(') => Some(')'),
        Some('[') => Some(']'),
        Some('{') => Some('}'),
        _ => None,
    };
    if let Some(closer) = closer {
        // When: closer is required by a wrapper, enforce a whole standalone pair before accepting prose punctuation.
        let before = &text[..start - 1];
        if before.chars().next_back().is_some_and(|ch| !ch.is_whitespace()) || right != Some(closer)
        {
            // When: before is concatenated or right mismatches closer, never repair the enclosing syntax.
            return false;
        }
        return text[end + 1..]
            .trim_start_matches(is_prose_path_punctuation)
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
    }
    left.is_none_or(char::is_whitespace)
        && text[end..]
            .trim_start_matches(is_prose_path_punctuation)
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

fn grouped_source_target(
    text: &str,
    clicked: usize,
    segment_start: usize,
    segment_end: usize,
    style: PathStyle,
) -> Option<Option<TargetMatch>> {
    let segment = &text[segment_start..segment_end];
    let mut search_start = segment_start;
    for (offset, ch) in segment.char_indices() {
        if segment_start + offset < search_start
            || ch != ','
            || !segment[offset + 1..].trim_start_matches(' ').starts_with(':')
        {
            // When: ch and its following segment lack comma-colon syntax, they cannot introduce a shared-path location.
            continue;
        }
        let comma = segment_start + offset;
        let prefix = &text[search_start..comma];
        let trimmed_start = search_start + prefix.len() - prefix.trim_start_matches(' ').len();
        let wrapper_start = prefix
            .char_indices()
            .rev()
            .find_map(|(index, ch)| {
                (matches!(ch, '(' | '[' | '{')
                    && prefix[..index].chars().next_back().is_none_or(char::is_whitespace))
                .then_some(search_start + index)
            })
            .unwrap_or(trimmed_start);
        let discarded = &text[trimmed_start..wrapper_start];
        let ambiguous_prefix = discarded.contains(['/', '\\', '(', '[', '{']);
        let first_start = if ambiguous_prefix { trimmed_start } else { wrapper_start };
        let mut group_end = comma;
        for word in text[comma + 1..segment_end].split_inclusive(' ') {
            if word.trim().is_empty() || word.starts_with(':') {
                group_end += word.len();
            } else {
                // When: word is not an abbreviated location or gap, stop before neighboring prose and paths.
                break;
            }
        }
        group_end = (group_end + 1).min(segment_end);
        search_start = group_end;
        if clicked < first_start || clicked >= group_end {
            // When: clicked belongs outside first_start..group_end, keep independent neighboring targets searchable.
            continue;
        }
        if text[first_start..comma].contains(' ') {
            // When: first_start..comma contains spaces, reject the complete ambiguous anchor instead of shortening its filename.
            return Some(None);
        }
        return Some(parse_source_group(text, clicked, first_start, comma, group_end, style));
    }
    None
}

fn parse_source_group(
    text: &str,
    clicked: usize,
    first_start: usize,
    comma: usize,
    segment_end: usize,
    style: PathStyle,
) -> Option<TargetMatch> {
    let opener = text[first_start..].chars().next()?;
    let closer = match opener {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    };
    let start = first_start + usize::from(closer.is_some());
    if segment_end - start > MAX_TARGET_BYTES {
        // When: segment_end exceeds start by the byte budget, reject before constructing inherited references.
        return None;
    }
    let SourceSuffix::Source(first) = parse_source_reference(&text[start..comma], style, false)
    else {
        // When: first lacks a valid explicit source anchor, abbreviated locations cannot authorize a filename.
        return None;
    };
    let mut references = vec![(start, comma, first.clone())];
    let mut pos = comma;
    loop {
        if references.len() == MAX_SPACED_PATH_TOKENS {
            // When: references fills the shared item budget, reject the whole group rather than a clickable prefix.
            return None;
        }
        pos += 1;
        while text.as_bytes().get(pos) == Some(&b' ') {
            pos += 1;
        }
        let member_start = pos;
        if text.as_bytes().get(pos) != Some(&b':') {
            // When: pos does not begin a colon location, reject a malformed continuation rather than inheriting prose.
            return None;
        }
        pos += 1;
        let suffix_start = pos;
        while pos < segment_end {
            let ch = text[pos..].chars().next()?;
            if !(ch.is_ascii_digit() || matches!(ch, ':' | '-' | '\u{2013}')) {
                // When: ch ends numeric location syntax, leave its enclosing delimiter for boundary validation.
                break;
            }
            pos += ch.len_utf8();
        }
        let suffix = &text[suffix_start..pos];
        let combined = format!("{}:{suffix}", first.path);
        let SourceSuffix::Source(reference) = parse_source_reference(&combined, style, false)
        else {
            // When: reference cannot validate combined path/location syntax, invalidate every member of the group.
            return None;
        };
        references.push((member_start, pos, reference));
        if text.as_bytes().get(pos) != Some(&b',') {
            // When: pos is not another comma, validate the final group boundary before yielding any target.
            break;
        }
    }
    let end = pos;
    if end - start > MAX_TARGET_BYTES || !structured_outer_boundary(text, start, end) {
        // When: start..end lacks complete bounded outer syntax, reject instead of exposing a prefix.
        return None;
    }
    if closer.is_some() && text[end..].chars().next() != closer {
        // When: closer does not match the final character, never repair a malformed group wrapper.
        return None;
    }
    if clicked >= end {
        // When: clicked belongs to trailing punctuation, it cannot own the preceding reference.
        return None;
    }
    let selected =
        references.into_iter().find(|(left, right, _)| (*left..*right).contains(&clicked));
    selected.map(|(_, _, mut reference)| {
        reference.display = text[start..end].to_string();
        TargetMatch {
            start,
            end,
            source_start: first_start,
            source_end: segment_end,
            missing_before: Vec::new(),
            target: DetectedTarget::SourceReference(reference),
        }
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
        source_start: quote_start,
        source_end: end + 1,
        missing_before: Vec::new(),
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
    if let Some(open) = text[start..end].find('(') {
        // When: `open` exists, only an identifier-call boundary may expose an inner path.
        let name = &text[start..start + open];
        let identifier = !name.is_empty()
            && name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()
            });
        if identifier && text[..end].ends_with(')') {
            // When: `identifier` and the final closer agree, nested filename parentheses must still balance.
            let inner_start = start + open + 1;
            let inner_end = end - 1;
            let inner = &text[inner_start..inner_end];
            let mut depth = 0usize;
            let balanced = inner.chars().all(|ch| match ch {
                '(' => {
                    depth += 1;
                    true
                }
                ')' => {
                    // When: `ch` closes parentheses, reject an unmatched closer rather than stripping literal text.
                    if depth == 0 {
                        // When: `depth` is zero, this closer cannot belong to the candidate's inner path.
                        return false;
                    }
                    depth -= 1;
                    true
                }
                _ => true,
            }) && depth == 0;
            if balanced && has_path_prefix(inner, style) {
                // When: `balanced` inner text has explicit path syntax, discard only the call's outer wrapper.
                return (inner_start, inner_end);
            }
        }
    }
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
    // Keep query separators inside the target while excluding characters the URI check rejects.
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
