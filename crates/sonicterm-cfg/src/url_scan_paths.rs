//! Shared POSIX and Windows path grammar, token boundaries, and wrapper validation.

use super::*;

pub(super) fn validate_bare_name(candidate: &str, style: PathStyle) -> bool {
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
                && !candidate
                    .chars()
                    .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
                && !candidate.ends_with(['.', ' '])
        }
    }
}

pub(super) fn is_path_start_boundary(text: &str, start: usize) -> bool {
    if start == 0 {
        // When: `start` is zero, the candidate begins at a row boundary without requiring a preceding delimiter.
        return true;
    }
    text[..start].chars().next_back().is_some_and(|character| {
        character.is_whitespace() || matches!(character, '(' | '[' | '{' | '<' | '>')
    })
}

pub(super) fn is_path_delimiter(character: char) -> bool {
    character.is_whitespace()
        || character.is_control()
        || matches!(character, '"' | '\'' | '`' | '<' | '>')
}

pub(super) fn is_path_hard_delimiter(character: char) -> bool {
    (character.is_whitespace() && character != ' ')
        || character.is_control()
        || matches!(character, '"' | '\'' | '`' | '<' | '>')
}

pub(super) fn quoted_spaced_segment(text: &str, start: usize, end: usize) -> bool {
    let starts_at_content =
        text[start..end].chars().next().is_some_and(|character| character != ' ');
    let ends_at_content =
        text[start..end].chars().next_back().is_some_and(|character| character != ' ');
    let left_quote = text[..start]
        .char_indices()
        .next_back()
        .filter(|(_, character)| matches!(character, '"' | '\'' | '`'));
    let right_quote =
        text[end..].chars().next().filter(|character| matches!(character, '"' | '\'' | '`'));
    let left_is_wrapper = left_quote.is_some_and(|(index, _)| {
        text[..index].chars().next_back().is_none_or(|character| {
            character.is_whitespace() || matches!(character, '=' | ':' | '(' | '[' | '{' | '<')
        })
    });
    let right_closes_or_is_unmatched =
        right_quote.is_some_and(|quote| !text[end + quote.len_utf8()..].contains(quote));
    // Quote wrappers make every inner candidate ambiguous, even when spaces pad the delimiters.
    left_is_wrapper
        || right_closes_or_is_unmatched
        || left_quote.is_some() && starts_at_content
        || right_quote.is_some() && ends_at_content
}

pub(super) fn escaped_space_path(
    text: &str,
    start: usize,
    candidate: &str,
    style: PathStyle,
) -> bool {
    style == PathStyle::Posix
        && (candidate.contains("\\ ")
            || text[..start].strip_suffix(' ').is_some_and(|prefix| prefix.ends_with('\\')))
}

pub(super) fn soft_space_token_spans(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut token_start = None;
    for (offset, character) in text[start..end].char_indices() {
        let index = start + offset;
        if character == ' ' {
            if let Some(token_start) = token_start.take() {
                spans.push((token_start, index));
            }
        } else if token_start.is_none() {
            // When: `token_start` is absent at non-space `character`, record the opening byte of this named run.
            token_start = Some(index);
        }
    }
    if let Some(token_start) = token_start {
        spans.push((token_start, end));
    }
    spans
}

pub(super) fn unsafe_wrapper_adjacent(text: &str, start: usize, end: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_some_and(|character| is_path_hard_delimiter(character) && !character.is_whitespace())
        || text[end..].chars().next().is_some_and(|character| {
            is_path_hard_delimiter(character) && !character.is_whitespace()
        })
}

pub(super) fn trim_outer_path_wrapper(
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
            let balanced = inner.chars().all(|character| match character {
                '(' => {
                    depth += 1;
                    true
                }
                ')' => {
                    // When: `character` closes parentheses, reject an unmatched closer rather than stripping literal text.
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

pub(super) fn trim_matching_wrapper(text: &str, start: usize, mut end: usize) -> usize {
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

pub(super) fn has_path_prefix(candidate: &str, style: PathStyle) -> bool {
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
        .find_map(|(index, character)| is_path_delimiter(character).then_some(index))
        .unwrap_or(candidate.len());
    let syntax_prefix = &candidate[..syntax_end];
    let separator = match style {
        PathStyle::Posix => candidate.char_indices().find(|(_, character)| *character == '/'),
        PathStyle::Windows => {
            candidate.char_indices().find(|(_, character)| is_windows_separator(*character))
        }
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
        || syntax_prefix.chars().any(|character| {
            matches!(character, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | '<' | '>')
        })
        || syntax_prefix.contains(['~', '$'])
        || (style == PathStyle::Windows && syntax_prefix.contains('%'))
    {
        // When: `first`, `last`, or `syntax_prefix` carries ambiguous wrapper, expansion, or pseudo-component syntax, keep it inert.
        return false;
    }
    !first.contains(':')
}

pub(super) fn validate_path_candidate(candidate: &str, style: PathStyle) -> bool {
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
            for (index, character) in candidate.char_indices() {
                if matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
                    || (character == ':' && index != 1)
                {
                    // When: `character` is reserved or a colon outside drive `index` 1, reject the Windows candidate.
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
pub(super) fn is_windows_separator(character: char) -> bool {
    matches!(character, '/' | '\\')
}

#[cfg(test)]
#[path = "url_scan_paths_tests.rs"]
mod url_scan_paths_tests;
