//! The extent a URL's own delimiters fix inside a rebuilt numbered-box line.

/// What a URL's opening delimiter says about its extent.
pub(super) enum Delimited {
    /// No `(` or quote opens the URL, so the scanner reads it.
    Absent,
    /// The URL its delimiters fix, checked whole.
    Url(sonicterm_cfg::url_scan::UrlMatch),
    /// A delimiter opens the URL but ends it at another character, or its text is no URL: the
    /// destination is not readable, so no shorter reading may open.
    Unresolved,
}

/// The extent of the URL starting at byte `scheme` of the rebuilt `text`, in the token ending at
/// `token_end`. After `(` or `](` it runs to the matching `)`, so a `)` the URL holds stays and
/// markdown after the closer does not join; when the token ends first with the URL's own
/// parentheses balanced, the URL ends there, and after a prose `(` trailing sentence punctuation
/// is trimmed as the scanner trims it. After a quote it runs to the same quote; a token ending at
/// a blank leaves the quote wrapping more than the URL, and any other character is `Unresolved`.
pub(super) fn delimited_url(text: &str, scheme: usize, token_end: usize) -> Delimited {
    let bytes = text.as_bytes();
    let Some(&opener) = scheme.checked_sub(1).and_then(|before| bytes.get(before)) else {
        // When: nothing precedes the scheme in the rebuilt line, no delimiter opens the URL.
        return Delimited::Absent;
    };
    let end = match opener {
        b'(' => paren_end(bytes, scheme, token_end),
        b'"' | b'\'' => quote_end(bytes, opener, token_end),
        _ => Err(Delimited::Absent),
    };
    let mut end = match end {
        Ok(end) => end,
        Err(decided) => {
            // When: the delimiter already `decided` the URL's extent cannot be fixed, it stands.
            return decided;
        }
    };
    let markdown = scheme >= 2 && bytes[scheme - 2] == b']';
    // Prose parentheses: trailing sentence punctuation is not the URL's, as the scanner reads.
    let trims = opener == b'(' && !markdown;
    while trims && end > scheme && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?')
    {
        end -= 1;
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

/// The end of a URL a `(` opens: its matching `)`, or the token's end when the token ends first
/// with the URL's own parentheses balanced; `Unresolved` when it ends inside one of them.
fn paren_end(bytes: &[u8], scheme: usize, token_end: usize) -> Result<usize, Delimited> {
    // Depth counts the URL's own parentheses; a `)` at depth 0 closes the opener.
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().take(token_end).skip(scheme) {
        if *byte == b')' && depth == 0 {
            // When: a `)` meets `depth` 0, it closes the opener and ends the URL.
            return Ok(index);
        }
        // An `(` opens one of the URL's own parentheses and a `)` closes one.
        depth = depth + usize::from(*byte == b'(') - usize::from(*byte == b')');
    }
    (depth == 0).then_some(token_end).ok_or(Delimited::Unresolved)
}

/// The end of a URL a quote `opener` opens: the same quote ending the token. A token ending at a
/// blank or the line's end leaves the quote wrapping more than the URL, so the scanner reads it;
/// another character, such as an apostrophe the URL holds, leaves it `Unresolved`.
fn quote_end(bytes: &[u8], opener: u8, token_end: usize) -> Result<usize, Delimited> {
    match bytes.get(token_end) {
        Some(closer) if *closer == opener => Ok(token_end),
        None | Some(b' ') => Err(Delimited::Absent),
        Some(_) => Err(Delimited::Unresolved),
    }
}
