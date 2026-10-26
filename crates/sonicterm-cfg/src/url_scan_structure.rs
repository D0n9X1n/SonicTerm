//! Balanced wrappers and incremental structural path candidate scanning.

use super::*;

fn presentation_pair(character: char) -> Option<char> {
    match character {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' | '"' | '`' => Some(character),
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

fn presentation_closer(character: char) -> bool {
    matches!(character, ')' | ']' | '}' | '）' | '】' | '》' | '」' | '』' | '”' | '’' | '»')
}

fn outer_separator(character: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory};
    !matches!(character, '/' | '\\' | '\'' | '"' | '`')
        && matches!(
            get_general_category(character),
            GeneralCategory::OtherPunctuation | GeneralCategory::DashPunctuation
        )
}

/// Whether a source-only scalar can be a structural delimiter or prose separator, never path content.
#[must_use]
pub fn is_path_boundary_character(character: char) -> bool {
    presentation_pair(character).is_some()
        || presentation_closer(character)
        || outer_separator(character)
}

fn structure_tail_end(text: &str, end: usize) -> Option<usize> {
    let mut cursor = end;
    for (offset, character) in text[end..].char_indices() {
        if !outer_separator(character) {
            // When: character ends the separator run, leave neighboring prose outside the validation span.
            break;
        }
        cursor = end + offset + character.len_utf8();
    }
    let next = text[cursor..].chars().next();
    if cursor == end {
        // When: cursor equals end, no separator proves independence from adjacent filename text.
        return next.is_none_or(char::is_whitespace).then_some(end);
    }
    if text[end..cursor].ends_with(['.', ':'])
        && next.is_some_and(|character| !character.is_whitespace())
    {
        // When: text ends in a dot or colon before next, preserve filename/location continuations rather than truncating them.
        return None;
    }
    if next.is_some_and(|character| {
        character.is_control() || matches!(character, '/' | '\\') || presentation_closer(character)
    }) {
        // When: next continues path structure or a malformed wrapper, punctuation cannot authorize an inner prefix.
        return None;
    }
    Some(cursor)
}

/// Test-only count of bytes the structural parse visits, so a test can prove its work is linear.
#[cfg(test)]
pub(crate) mod structure_work {
    use std::cell::Cell;

    thread_local! {
        static BYTES: Cell<u64> = const { Cell::new(0) };
    }

    /// Add `bytes` visited by one structural step.
    pub(crate) fn add(bytes: usize) {
        BYTES.with(|count| count.set(count.get() + bytes as u64));
    }

    /// Bytes visited by `body` on this thread.
    pub(crate) fn measure(body: impl FnOnce()) -> u64 {
        let before = BYTES.with(Cell::get);
        body();
        BYTES.with(Cell::get) - before
    }
}

fn structure_close(text: &str, open: usize, opener: char, closer: char) -> Option<usize> {
    let start = open + opener.len_utf8();
    let quoted = matches!(opener, '\'' | '"' | '`' | '“' | '‘' | '«');
    let mut stack = vec![closer];
    for (offset, character) in text[start..].char_indices() {
        #[cfg(test)]
        structure_work::add(character.len_utf8());
        if offset > MAX_TARGET_BYTES || character.is_control() {
            // When: offset exceeds the byte budget or character is controlled, incomplete source cannot authorize a path.
            return None;
        }
        if Some(&character) == stack.last() {
            // When: character closes stack's current pair, only the final pop establishes the outer boundary.
            stack.pop();
            if stack.is_empty() {
                // When: stack is empty, offset names the proven outer closer rather than a filename's inner bracket.
                return Some(start + offset);
            }
        } else if !quoted
            && !stack.last().is_some_and(|character| matches!(character, '\'' | '"' | '`'))
        {
            // When: quoted is false and stack is outside quotes, paired filename brackets stay inside the outer structure.
            if let Some(close) = presentation_pair(character) {
                // When: presentation_pair finds close, track it without selecting an inner filename fragment.
                if stack.len() == MAX_SPACED_PATH_TOKENS {
                    // When: stack reaches the nesting bound, reject rather than scan unbounded structure.
                    return None;
                }
                stack.push(close);
            } else if presentation_closer(character) {
                // When: presentation_closer finds a different closer, never repair mismatched source text.
                return None;
            }
        } else if matches!(character, '\'' | '"' | '`' | '“' | '”' | '‘' | '’') {
            // When: matches! finds another quote in character before closure, refuse shell-like concatenation or escaping.
            return None;
        }
    }
    None
}

/// Whether `word` opens with an explicit path anchor: a root, a home or dot prefix, or a drive.
fn rooted_word(word: &str) -> bool {
    word.starts_with(['/', '\\'])
        || word.starts_with("~/")
        || word.starts_with("~\\")
        || word.starts_with("./")
        || word.starts_with(".\\")
        || word.starts_with("../")
        || word.starts_with("..\\")
        || matches!(word.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

/// The prefix check as a rescan from the floor: the test-only reference [`PrefixScan`] must equal.
#[cfg(test)]
fn structure_prefix_start_reference(
    text: &str,
    floor: usize,
    open: usize,
    opener: char,
) -> Option<usize> {
    let prefix = &text[floor..open];
    let word_start = floor
        + prefix.rfind(char::is_whitespace).map_or(0, |space_offset| {
            space_offset + prefix[space_offset..].chars().next().unwrap().len_utf8()
        });
    let word = &text[word_start..open];
    let call = opener == '('
        && !word.is_empty()
        && word.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()
        });
    if !word.is_empty() && !call {
        // When: word is non-empty and not an identifier call, the prefix owns this structure.
        return None;
    }
    if text[floor..word_start].split_whitespace().any(rooted_word) {
        // When: any whitespace-split word before word_start is rooted, an explicit anchor owns it.
        return None;
    }
    Some(if call { word_start } else { open })
}

/// The text between the structural floor and the next opener, summarized as it is read, so each
/// opener's prefix check costs only the current word's length rather than a rescan from the floor.
struct PrefixScan {
    /// Byte offset of the floor; nothing before it belongs to the prefix.
    floor: usize,
    /// Byte offset where the current, still-open word starts.
    word_start: usize,
    /// Whether every byte of the current word so far keeps it an identifier.
    identifier: bool,
    /// Whether a completed word since the floor opened with an explicit path anchor.
    rooted: bool,
}

impl PrefixScan {
    /// An empty prefix starting at byte offset `floor`.
    fn at(floor: usize) -> Self {
        Self { floor, word_start: floor, identifier: true, rooted: false }
    }

    /// Read `character`, which starts at byte offset `index`, into the prefix.
    fn feed(&mut self, text: &str, index: usize, character: char) {
        if index < self.floor {
            // When: `index` precedes the floor, a reset already skipped past this character.
            return;
        }
        #[cfg(test)]
        structure_work::add(character.len_utf8());
        if character.is_whitespace() {
            // When: `character` ends a word, record whether that word was rooted.
            self.rooted |= self.word_start < index && rooted_word(&text[self.word_start..index]);
            self.word_start = index + character.len_utf8();
            self.identifier = true;
            return;
        }
        let first = index == self.word_start;
        // A multi-byte character is never an identifier byte, as in a bytewise check.
        self.identifier &= character == '_'
            || character.is_ascii_alphabetic()
            || (!first && character.is_ascii_digit());
    }

    /// [`Self::source_start`], checked under tests against the rescan it replaces from `floor`.
    fn checked_source_start(
        &self,
        text: &str,
        floor: usize,
        open: usize,
        opener: char,
    ) -> Option<usize> {
        let found = self.source_start(text, open, opener);
        #[cfg(test)]
        assert_eq!(
            found,
            structure_prefix_start_reference(text, floor, open, opener),
            "{text:?} at {open} from floor {floor}"
        );
        #[cfg(not(test))]
        let _ = floor;
        found
    }

    /// Where the source of a structure opened by `opener` at `open` starts: the identifier of a
    /// call, or the opener itself. `None` when a non-call word touches the opener or a completed
    /// word since the floor is rooted, so a suffix of a longer explicit path is never independent.
    fn source_start(&self, text: &str, open: usize, opener: char) -> Option<usize> {
        let word = &text[self.word_start..open];
        let call = opener == '(' && !word.is_empty() && self.identifier;
        if !word.is_empty() && !call {
            // When: word is not an identifier call, retain its prefix rather than treating a suffix as independent.
            return None;
        }
        if self.rooted {
            // When: rooted proves an unconsumed explicit anchor, preserve its spaced name instead of exposing an inner suffix.
            return None;
        }
        Some(if call { self.word_start } else { open })
    }
}

pub(super) fn structured_path_candidates(
    text: &str,
    clicked: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<Vec<TargetMatch>> {
    let mut floor = 0;
    let mut skip_until = 0;
    let mut prefix = PrefixScan::at(0);
    for (open, opener) in text.char_indices() {
        if open < skip_until {
            // When: open belongs to a previously consumed pair, do not promote nested presentation fragments.
            prefix.feed(text, open, opener);
            continue;
        }
        if opener.is_control() || (opener.is_whitespace() && opener != ' ') {
            // When: opener is a hard delimiter, prior path anchors cannot span it.
            floor = open + opener.len_utf8();
            prefix = PrefixScan::at(floor);
            continue;
        }
        let Some(closer) = presentation_pair(opener) else {
            // When: opener has no presentation pair, ordinary text does not establish structure.
            prefix.feed(text, open, opener);
            continue;
        };
        let start = open + opener.len_utf8();
        let Some(close) = structure_close(text, open, opener, closer) else {
            // When: an incomplete structure encloses explicit path text, prevent a suffix from becoming another target.
            if clicked >= start
                && !text[start..].starts_with(char::is_whitespace)
                && prefix.checked_source_start(text, floor, open, opener).is_some()
                && has_path_prefix(&text[start..], style)
            {
                // When: clicked lies inside an incomplete explicit path structure, never recover a partial destination.
                return Some(Vec::new());
            }
            prefix.feed(text, open, opener);
            continue;
        };
        let end = close + closer.len_utf8();
        skip_until = end;
        let body = &text[start..close];
        let owns = (open..end).contains(&clicked);
        let source_start = prefix.checked_source_start(text, floor, open, opener);
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
                prefix = PrefixScan::at(floor);
            } else {
                // When: source_start or source_end is unproven, the opener stays part of the prefix.
                prefix.feed(text, open, opener);
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
        let grouped = body.char_indices().find_map(|(offset, character)| {
            (character == ',' && body[offset + 1..].trim_start_matches(' ').starts_with(':'))
                .then_some(offset)
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

pub(super) fn quoted_path_target(
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

pub(super) fn structured_outer_boundary(text: &str, start: usize, end: usize) -> bool {
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
        if before.chars().next_back().is_some_and(|character| !character.is_whitespace())
            || right != Some(closer)
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

#[cfg(test)]
#[path = "url_scan_structure_tests.rs"]
mod url_scan_structure_tests;
