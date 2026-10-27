//! Prose, punctuation-list, and log-field candidate boundaries.

use super::*;

pub(super) fn punctuation_list_member(
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
        .filter(|(_, character)| {
            get_general_category(*character) == GeneralCategory::OtherPunctuation
                && !matches!(
                    character,
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
        .map(|(offset, character)| (offset, character.len_utf8()))
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
            || extension.chars().any(|character| !character.is_alphanumeric())
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

pub(super) fn prose_leading_member(
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
    let (offset, _) = body.char_indices().find(|(_, character)| {
        get_general_category(*character) == GeneralCategory::OtherPunctuation
            && !matches!(
                character,
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

pub(super) fn log_field_candidates(
    text: &str,
    clicked: usize,
    style: PathStyle,
) -> Option<Vec<TargetMatch>> {
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
            if text[after..].chars().next().is_some_and(|character| !character.is_whitespace()) {
                // When: text after the closing quote is concatenated, reject the partial quoted value.
                return (clicked >= value_start && clicked <= after).then(Vec::new);
            }
            (end, after)
        } else {
            // When: quoted is false, stop at a proven next field while preserving spaces as filename candidates.
            let end = text[body_start..]
                .char_indices()
                .find_map(|(offset, character)| {
                    let index = body_start + offset;
                    (character.is_control() || log_field_value_start(text, index).is_some())
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

#[cfg(test)]
#[path = "url_scan_prose_tests.rs"]
mod url_scan_prose_tests;
