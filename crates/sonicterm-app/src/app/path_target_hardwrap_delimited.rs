//! The extent a URL's own delimiters fix inside a rebuilt numbered-box line.

/// What a URL's opening delimiter says about its extent.
pub(super) enum Delimited {
    /// No delimiter opens the URL, so the scanner reads it.
    Absent,
    /// The URL its delimiters fix, checked whole.
    Url(sonicterm_cfg::url_scan::UrlMatch),
    /// A delimiter opens the URL but ends it at another character, or its text is no URL: the
    /// destination is not readable, so no shorter reading may open.
    Unresolved,
}

/// The extent of the URL starting at byte `scheme` of the rebuilt `text`, in the token ending at
/// `token_end`. After `(`, `](` or `[` it runs to the matching `)` or `]`, so a `)` the URL holds
/// stays and markdown after the closer does not join; when the token ends first with the URL's
/// own brackets balanced, the URL ends there. After prose `(` or `[`, trailing `.`, `,`, `;` or `:`
/// is trimmed as the scanner trims it, while a trailing `!` or `?`, which a URL may end with, is
/// `Unresolved`. After a quote, a backtick or `<` it runs to the same quote, backtick or `>`; a
/// token ending at a blank leaves the delimiter wrapping more than the URL, and any other
/// character, or a doubled quote, is `Unresolved`.
pub(super) fn delimited_url(text: &str, scheme: usize, token_end: usize) -> Delimited {
    let bytes = text.as_bytes();
    let Some(&opener) = scheme.checked_sub(1).and_then(|before| bytes.get(before)) else {
        // When: nothing precedes the scheme in the rebuilt line, no delimiter opens the URL.
        return Delimited::Absent;
    };
    let end = match opener {
        b'(' => bracket_end(bytes, (b'(', b')'), scheme, token_end),
        b'[' => bracket_end(bytes, (b'[', b']'), scheme, token_end),
        b'"' | b'\'' | b'`' => quote_end(bytes, (opener, opener), token_end),
        b'<' => quote_end(bytes, (b'<', b'>'), token_end),
        _ => Err(Delimited::Absent),
    };
    let mut end = match end {
        Ok(end) => end,
        Err(decided) => {
            // When: the delimiter already `decided` the URL's extent cannot be fixed, it stands.
            return decided;
        }
    };
    let markdown = opener == b'(' && scheme >= 2 && bytes[scheme - 2] == b']';
    // Prose brackets: trailing sentence punctuation is not the URL's, as the scanner reads.
    let prose = matches!(opener, b'(' | b'[') && !markdown;
    while prose && end > scheme && matches!(bytes[end - 1], b'.' | b',' | b';' | b':') {
        end -= 1;
    }
    if prose && end > scheme && matches!(bytes[end - 1], b'!' | b'?') {
        // When: a `prose` bracket's URL ends in `!` or `?`, either may be the URL's own or the
        // sentence's, so its extent is not readable.
        return Delimited::Unresolved;
    }
    let url = text.get(scheme..end).filter(|url| sonicterm_cfg::url_scan::is_url(url));
    url.map_or(Delimited::Unresolved, |url| {
        Delimited::Url(sonicterm_cfg::url_scan::UrlMatch {
            start: scheme,
            end,
            url: url.to_string(),
        })
    })
}

/// The end of a URL an `open` bracket opens: its matching `close`, or the token's end when the
/// token ends first with the URL's own brackets balanced; `Unresolved` when it ends inside one.
fn bracket_end(
    bytes: &[u8],
    (open, close): (u8, u8),
    scheme: usize,
    token_end: usize,
) -> Result<usize, Delimited> {
    // Depth counts the URL's own brackets; a `close` at depth 0 closes the opener.
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().take(token_end).skip(scheme) {
        if *byte == close && depth == 0 {
            // When: a `close` meets `depth` 0, it closes the opener and ends the URL.
            return Ok(index);
        }
        // An `open` starts one of the URL's own brackets and a `close` ends one.
        depth = depth + usize::from(*byte == open) - usize::from(*byte == close);
    }
    (depth == 0).then_some(token_end).ok_or(Delimited::Unresolved)
}

/// The end of a URL an `open` quote, backtick or `<` opens: the `close` ending the token. A token
/// ending at a blank or the line's end leaves the delimiter wrapping more than the URL, so the
/// scanner reads it; another character, such as an apostrophe the URL holds, or a doubled quote
/// that escapes one, leaves it `Unresolved`.
fn quote_end(bytes: &[u8], (open, close): (u8, u8), token_end: usize) -> Result<usize, Delimited> {
    let doubled = open == close && bytes.get(token_end + 1) == Some(&close);
    match bytes.get(token_end) {
        Some(closer) if *closer == close && !doubled => Ok(token_end),
        None | Some(b' ') => Err(Delimited::Absent),
        Some(_) => Err(Delimited::Unresolved),
    }
}
