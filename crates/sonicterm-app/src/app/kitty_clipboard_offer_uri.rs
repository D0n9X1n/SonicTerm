//! Allocation-free preflight of native file lists. Walk borrowed lines and decoded characters;
//! only a staged offer whose lease covers its destination may allocate path strings or items.

/// Maximum number of native file items in one clipboard representation, on reads and writes.
pub(super) const ITEM_CAP: usize = 256;

/// Nonempty, non-comment URI lines. Refuse excessive lists before constructing native items.
pub(super) fn items(bytes: &[u8]) -> Option<impl Iterator<Item = &str> + Clone> {
    let lines = std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let count = lines.clone().take(ITEM_CAP + 1).count();
    (1..=ITEM_CAP).contains(&count).then_some(lines)
}

/// A percent-encoded component, yielding one checked Unicode scalar at a time. An invalid
/// escape, malformed UTF-8 or control character yields None rather than a partial path.
#[derive(Clone)]
struct Decoded<'uri>(&'uri [u8]);

impl Decoded<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&byte, rest) = self.0.split_first()?;
        self.0 = rest;
        if byte != b'%' {
            // When: byte is literal, it contributes one octet without consuming an escape.
            return Some(byte);
        }
        let high = char::from(*rest.first()?).to_digit(16)?;
        let low = char::from(*rest.get(1)?).to_digit(16)?;
        self.0 = &rest[2..];
        Some((high * 16 + low) as u8)
    }
}

impl Iterator for Decoded<'_> {
    type Item = Option<char>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            // When: the remaining input is_empty, there is no further scalar.
            return None;
        }
        Some((|| {
            let first = self.byte()?;
            let width = match first {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => {
                    // When: first is not a UTF-8 lead byte, the path is not decodable.
                    return None;
                }
            };
            let mut bytes = [first; 4];
            for slot in &mut bytes[1..width] {
                *slot = self.byte()?;
            }
            std::str::from_utf8(&bytes[..width])
                .ok()?
                .chars()
                .next()
                .filter(|scalar| !scalar.is_control())
        })())
    }
}

/// A decoded authority may not smuggle delimiters, credentials or Windows device namespaces.
fn valid_host(host: &str) -> bool {
    let decoded = Decoded(host.as_bytes());
    !decoded.clone().eq(".".chars().map(Some))
        && !decoded.clone().eq("..".chars().map(Some))
        && decoded.clone().all(|part| {
            part.is_some_and(|scalar| scalar.is_alphanumeric() || matches!(scalar, '.' | '-' | '_'))
        })
}

/// Split an absolute file URI into its checked host and encoded path (without the leading slash).
pub(super) fn file_parts(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file")
        || rest.contains(['?', '#', '\\'])
        || rest.contains(char::is_whitespace)
    {
        // When: scheme or rest is not a plain file URI, do not publish it as a file item.
        return None;
    }
    let (host, path) = match rest.strip_prefix("//") {
        Some(authority) => authority.split_once('/')?,
        None => ("", rest.strip_prefix('/')?),
    };
    (valid_host(host)
        && Decoded(path.as_bytes()).all(|part| part.is_some_and(|scalar| scalar != '\\')))
    .then_some((host, path))
}

/// Emit a validated absolute drive or UNC path without allocating during preflight.
#[cfg(any(test, target_os = "windows"))]
pub(super) fn windows_path(uri: &str, mut emit: impl FnMut(char)) -> Option<()> {
    let (host, path) = file_parts(uri)?;
    let chars = Decoded(path.as_bytes()).flatten();
    if host.is_empty()
        || Decoded(host.as_bytes())
            .flatten()
            .map(|scalar| scalar.to_ascii_lowercase())
            .eq("localhost".chars())
    {
        // When: host is empty or localhost, require a rooted drive path.
        let mut drive = chars.clone();
        if !drive.next()?.is_ascii_alphabetic() || drive.next()? != ':' || drive.next()? != '/' {
            // When: drive lacks a letter, colon and slash, the path is not absolute.
            return None;
        }
    } else {
        // When: host is remote, a nonempty share must follow it.
        if chars.clone().next().is_none_or(|first| first == '/') {
            // When: chars starts with a separator or is empty, there is no share name.
            return None;
        }
        emit('\\');
        emit('\\');
        Decoded(host.as_bytes()).flatten().for_each(&mut emit);
        emit('\\');
    }
    chars.map(|scalar| if scalar == '/' { '\\' } else { scalar }).for_each(emit);
    Some(())
}
