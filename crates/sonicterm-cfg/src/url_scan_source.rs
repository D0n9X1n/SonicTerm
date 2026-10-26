//! Local-link destinations and typed editor line, column, and range references.

use super::*;

/// Outcome of reading a trailing editor location suffix off one candidate.
pub(super) enum SourceSuffix {
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
    if is_drive_letter(head) || !middle.starts_with(|character: char| character.is_ascii_digit()) {
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
pub(super) fn parse_source_reference(
    candidate: &str,
    style: PathStyle,
    include_bare_names: bool,
) -> SourceSuffix {
    let Some((prefix, last)) = candidate.rsplit_once(':') else {
        // When: `candidate` has no colon, no location suffix can be present.
        return SourceSuffix::NotSource;
    };
    if !last.starts_with(|character: char| character.is_ascii_digit()) {
        // When: last is nonnumeric, a preceding numeric segment still makes this a malformed location.
        if prefix.rsplit_once(':').is_some_and(|(head, segment)| {
            !is_drive_letter(head)
                && segment.starts_with(|character: char| character.is_ascii_digit())
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
        if location.starts_with(|character: char| character.is_ascii_digit()) {
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
            let high = bytes.next().and_then(|digit| (digit as char).to_digit(16));
            let low = bytes.next().and_then(|digit| (digit as char).to_digit(16));
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

pub(super) fn grouped_source_target(
    text: &str,
    clicked: usize,
    segment_start: usize,
    segment_end: usize,
    style: PathStyle,
) -> Option<Option<TargetMatch>> {
    let segment = &text[segment_start..segment_end];
    let mut search_start = segment_start;
    for (offset, character) in segment.char_indices() {
        if segment_start + offset < search_start
            || character != ','
            || !segment[offset + 1..].trim_start_matches(' ').starts_with(':')
        {
            // When: character and its following segment lack comma-colon syntax, they cannot introduce a shared-path location.
            continue;
        }
        let comma = segment_start + offset;
        let prefix = &text[search_start..comma];
        let trimmed_start = search_start + prefix.len() - prefix.trim_start_matches(' ').len();
        let wrapper_start = prefix
            .char_indices()
            .rev()
            .find_map(|(index, character)| {
                (matches!(character, '(' | '[' | '{')
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

pub(super) fn parse_source_group(
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
            let character = text[pos..].chars().next()?;
            if !(character.is_ascii_digit() || matches!(character, ':' | '-' | '\u{2013}')) {
                // When: character ends numeric location syntax, leave its enclosing delimiter for boundary validation.
                break;
            }
            pos += character.len_utf8();
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

#[cfg(test)]
#[path = "url_scan_source_tests.rs"]
mod url_scan_source_tests;
